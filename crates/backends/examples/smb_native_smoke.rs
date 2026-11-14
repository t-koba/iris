use iris_backends::smb::adapter::SmbAdapter;
use iris_backends::smb::connection::SmbConnectionConfig;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const VERIFY_BUFFER_BYTES: usize = 64 * 1024;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let server = env_or("IRIS_SMB_HOST", "127.0.0.1");
    let port = env_or("IRIS_SMB_PORT", "1445")
        .parse::<u16>()
        .map_err(|_| "IRIS_SMB_PORT must be a valid port".to_string())?;
    let share_name = env_or("IRIS_SMB_SHARE", "bench");
    let username = env_or("IRIS_SMB_USER", "iris");
    let password = env_or("IRIS_SMB_PASSWORD", "iris");
    let backend_id = BackendId("smb:container-smoke".to_string());
    let adapter = SmbAdapter::new(
        SmbConnectionConfig {
            server,
            port,
            share_name,
            username: Some(username),
            // The standalone Samba fixture is explicitly configured for WORKGROUP.
            domain: Some("WORKGROUP".to_string()),
            password: Some(password),
            workstation: Some("IRIS-CI".to_string()),
            allow_guest: false,
            actor: Default::default(),
            listing: Default::default(),
        },
        backend_id.clone(),
    );
    adapter.connect().await.map_err(|error| error.to_string())?;

    let suffix = std::process::id();
    let source = object_ref(&backend_id, &format!("iris-smoke-{suffix}.bin"));
    let copied = object_ref(&backend_id, &format!("iris-smoke-{suffix}-copied.bin"));
    let renamed = object_ref(&backend_id, &format!("iris-smoke-{suffix}-renamed.bin"));
    let payload_path = std::env::var("IRIS_SMB_SMOKE_PAYLOAD")
        .map_err(|_| "IRIS_SMB_SMOKE_PAYLOAD must name the native SMB smoke payload".to_string())?;
    let payload_size = std::env::var("IRIS_SMB_SMOKE_BYTES")
        .map_err(|_| "IRIS_SMB_SMOKE_BYTES must set the payload size".to_string())?
        .parse::<u64>()
        .map_err(|_| "IRIS_SMB_SMOKE_BYTES must be an unsigned integer".to_string())?;
    if payload_size == 0 {
        return Err("native SMB smoke payload must not be empty".to_string());
    }
    create_position_varying_payload(&payload_path, payload_size).await?;
    let payload_file = tokio::fs::File::open(&payload_path)
        .await
        .map_err(|error| format!("cannot open native SMB smoke payload: {error}"))?;

    adapter
        .write_from_reader(
            &source,
            Box::pin(payload_file),
            Some(payload_size),
            ConflictResolution::Fail,
        )
        .await
        .map_err(|error| error.to_string())?;

    let mut handle = adapter
        .read(&source, None)
        .await
        .map_err(|error| error.to_string())?;
    let mut expected_payload = tokio::fs::File::open(&payload_path)
        .await
        .map_err(|error| format!("cannot reopen native SMB smoke payload: {error}"))?;
    verify_streams_match(&mut handle.stream, &mut expected_payload, payload_size)
        .await
        .map_err(|error| format!("native SMB read did not match the uploaded payload: {error}"))?;

    adapter
        .copy(
            &source,
            &copied,
            CopyOptions {
                on_conflict: ConflictResolution::Fail,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut copied_handle = adapter
        .read(&copied, None)
        .await
        .map_err(|error| error.to_string())?;
    let mut expected_copy = tokio::fs::File::open(&payload_path)
        .await
        .map_err(|error| format!("cannot reopen native SMB smoke payload: {error}"))?;
    verify_streams_match(&mut copied_handle.stream, &mut expected_copy, payload_size)
        .await
        .map_err(|error| {
            format!("native SMB COPYCHUNK result did not match the source: {error}")
        })?;

    adapter
        .move_object(
            &source,
            &renamed,
            MoveOptions {
                on_conflict: ConflictResolution::Fail,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let root = object_ref(&backend_id, "");
    let expected_paging_files = env_or("IRIS_SMB_PAGING_FILES", "0")
        .parse::<usize>()
        .map_err(|_| "IRIS_SMB_PAGING_FILES must be a number".to_string())?;
    let mut cursor = None;
    let mut found_renamed = false;
    let mut paging_files = 0usize;
    loop {
        let page = adapter
            .list_names(&root, cursor)
            .await
            .map_err(|error| error.to_string())?;
        for entry in page.entries {
            let entry = entry.name();
            found_renamed |= entry.object_ref.display_path == renamed.display_path;
            paging_files += usize::from(entry.name.starts_with("paging-"));
        }
        let Some(next) = page.next_cursor else {
            break;
        };
        cursor = Some(next);
    }
    if !found_renamed {
        return Err("renamed file was not returned by native SMB listing".to_string());
    }
    if paging_files != expected_paging_files {
        return Err(format!(
            "native SMB paged listing returned {paging_files} fixture files; expected {expected_paging_files}"
        ));
    }

    adapter
        .delete(&renamed, DeleteMode::Permanent)
        .await
        .map_err(|error| error.to_string())?;
    adapter
        .delete(&copied, DeleteMode::Permanent)
        .await
        .map_err(|error| error.to_string())?;
    println!("native SMB container smoke passed");
    Ok(())
}

async fn create_position_varying_payload(path: &str, length: u64) -> Result<(), String> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| format!("cannot create native SMB smoke payload: {error}"))?;
    let mut buffer = [0; VERIFY_BUFFER_BYTES];
    let mut offset = 0u64;
    while offset < length {
        let chunk_len = usize::try_from((length - offset).min(VERIFY_BUFFER_BYTES as u64))
            .map_err(|_| "payload chunk length is out of range".to_string())?;
        fill_position_varying_bytes(&mut buffer[..chunk_len], offset);
        file.write_all(&buffer[..chunk_len])
            .await
            .map_err(|error| format!("cannot write native SMB smoke payload: {error}"))?;
        offset += chunk_len as u64;
    }
    file.flush()
        .await
        .map_err(|error| format!("cannot flush native SMB smoke payload: {error}"))?;
    Ok(())
}

fn fill_position_varying_bytes(buffer: &mut [u8], offset: u64) {
    debug_assert_eq!(offset % 8, 0);
    for (word_index, bytes) in buffer.chunks_mut(8).enumerate() {
        let word = splitmix64(offset / 8 + word_index as u64).to_le_bytes();
        bytes.copy_from_slice(&word[..bytes.len()]);
    }
}

fn splitmix64(value: u64) -> u64 {
    let mut mixed = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

async fn verify_streams_match<A, E>(
    actual: &mut A,
    expected: &mut E,
    expected_len: u64,
) -> Result<(), String>
where
    A: AsyncRead + Unpin,
    E: AsyncRead + Unpin,
{
    let mut actual_buffer = [0; VERIFY_BUFFER_BYTES];
    let mut expected_buffer = [0; VERIFY_BUFFER_BYTES];
    let mut remaining = expected_len;
    while remaining > 0 {
        let chunk_len = usize::try_from(remaining.min(VERIFY_BUFFER_BYTES as u64))
            .map_err(|_| "payload chunk length is out of range".to_string())?;
        actual
            .read_exact(&mut actual_buffer[..chunk_len])
            .await
            .map_err(|error| format!("cannot read SMB output: {error}"))?;
        expected
            .read_exact(&mut expected_buffer[..chunk_len])
            .await
            .map_err(|error| format!("cannot read expected payload: {error}"))?;
        if actual_buffer[..chunk_len] != expected_buffer[..chunk_len] {
            return Err("payload bytes differ".to_string());
        }
        remaining -= chunk_len as u64;
    }

    let mut extra = [0];
    if actual
        .read(&mut extra)
        .await
        .map_err(|error| format!("cannot check SMB output length: {error}"))?
        != 0
    {
        return Err("SMB output is longer than the expected payload".to_string());
    }
    if expected
        .read(&mut extra)
        .await
        .map_err(|error| format!("cannot check expected payload length: {error}"))?
        != 0
    {
        return Err("expected payload changed while the smoke was running".to_string());
    }
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn object_ref(backend_id: &BackendId, display_path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: backend_id.clone(),
            stable_id: StableId(display_path.to_string()),
        },
        parent_stable_id: None,
        display_path: display_path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn stream_comparison_handles_multiple_chunks_and_rejects_repeated_data() {
        let mut expected = vec![0; VERIFY_BUFFER_BYTES * 2 + 17];
        for (chunk_index, chunk) in expected.chunks_mut(VERIFY_BUFFER_BYTES).enumerate() {
            fill_position_varying_bytes(chunk, (chunk_index * VERIFY_BUFFER_BYTES) as u64);
        }
        let mut actual = Cursor::new(expected.clone());
        let mut expected_stream = Cursor::new(expected.clone());
        verify_streams_match(&mut actual, &mut expected_stream, expected.len() as u64)
            .await
            .expect("matching streams");

        let mut repeated = expected.clone();
        repeated[VERIFY_BUFFER_BYTES..VERIFY_BUFFER_BYTES * 2]
            .copy_from_slice(&expected[..VERIFY_BUFFER_BYTES]);
        let mut actual = Cursor::new(repeated);
        let mut expected_stream = Cursor::new(expected.clone());
        assert_eq!(
            verify_streams_match(&mut actual, &mut expected_stream, expected.len() as u64)
                .await
                .expect_err("repeated chunk must fail"),
            "payload bytes differ"
        );

        let mut short = Cursor::new(expected[..expected.len() - 1].to_vec());
        let mut expected_stream = Cursor::new(expected.clone());
        assert!(
            verify_streams_match(&mut short, &mut expected_stream, expected.len() as u64)
                .await
                .expect_err("truncated content must fail")
                .starts_with("cannot read SMB output:")
        );

        let mut long = Cursor::new([expected.clone(), vec![0]].concat());
        let mut expected_stream = Cursor::new(expected.clone());
        assert_eq!(
            verify_streams_match(&mut long, &mut expected_stream, expected.len() as u64)
                .await
                .expect_err("excess content must fail"),
            "SMB output is longer than the expected payload"
        );
    }
}
