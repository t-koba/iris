use iris_backends::smb::adapter::SmbAdapter;
use iris_backends::smb::connection::SmbConnectionConfig;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use std::io::Cursor;
use tokio::io::AsyncReadExt;

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
            domain: None,
            password: Some(password),
            workstation: Some("IRIS-CI".to_string()),
            allow_guest: false,
        },
        backend_id.clone(),
    );
    adapter.connect().await.map_err(|error| error.to_string())?;

    let suffix = std::process::id();
    let source = object_ref(&backend_id, &format!("iris-smoke-{suffix}.bin"));
    let copied = object_ref(&backend_id, &format!("iris-smoke-{suffix}-copied.bin"));
    let renamed = object_ref(&backend_id, &format!("iris-smoke-{suffix}-renamed.bin"));
    let payload = b"iris native smb integration payload".to_vec();

    adapter
        .write_from_reader(
            &source,
            Box::pin(Cursor::new(payload.clone())),
            Some(payload.len() as u64),
            ConflictResolution::Fail,
        )
        .await
        .map_err(|error| error.to_string())?;

    let mut handle = adapter
        .read(&source, None)
        .await
        .map_err(|error| error.to_string())?;
    let mut downloaded = Vec::new();
    handle
        .stream
        .read_to_end(&mut downloaded)
        .await
        .map_err(|error| error.to_string())?;
    if downloaded != payload {
        return Err("native SMB read did not match the uploaded payload".to_string());
    }

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
    let mut copied_payload = Vec::new();
    copied_handle
        .stream
        .read_to_end(&mut copied_payload)
        .await
        .map_err(|error| error.to_string())?;
    if copied_payload != payload {
        return Err("native SMB COPYCHUNK result did not match the source".to_string());
    }

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
