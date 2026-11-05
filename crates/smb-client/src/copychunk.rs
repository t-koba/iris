use crate::connection::{SmbConnectionError, SmbResponse};
use crate::file::{close, open_file_for_read, open_file_for_write, SmbHandle};
use crate::packet::create::Smb2CreateDisposition;
use crate::packet::header::Smb2Command;
use crate::packet::ioctl::{Smb2IoctlFlags, Smb2IoctlRequest, Smb2IoctlResponse};
use crate::wire::{le_u32, push_u32, push_u64};
use crate::SmbClientError;
use crate::SmbConnectionActor;

pub const FSCTL_SRV_REQUEST_RESUME_KEY: u32 = 0x0014_0078;
pub const FSCTL_SRV_COPYCHUNK: u32 = 0x0014_40f2;
pub const SMB2_RESUME_KEY_BYTES: usize = 24;
pub const DEFAULT_COPYCHUNK_CHUNK_BYTES: u32 = 1024 * 1024;
pub const DEFAULT_COPYCHUNK_MAX_CHUNKS: usize = 16;

const SRV_REQUEST_RESUME_KEY_RESPONSE_MIN_BYTES: usize = SMB2_RESUME_KEY_BYTES;
const SRV_COPYCHUNK_COPY_FIXED_BYTES: usize = SMB2_RESUME_KEY_BYTES + 8;
const SRV_COPYCHUNK_BYTES: usize = 24;
const SRV_COPYCHUNK_RESPONSE_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmbCopyChunk {
    pub source_offset: u64,
    pub target_offset: u64,
    pub length: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmbCopyChunkResponse {
    pub chunks_written: u32,
    pub chunk_bytes_written: u32,
    pub total_bytes_written: u32,
}

pub async fn copy_file_chunks(
    connection: &SmbConnectionActor,
    source_path: impl Into<String>,
    target_path: impl Into<String>,
    create_disposition: Smb2CreateDisposition,
) -> Result<u64, SmbConnectionError> {
    let source = open_file_for_read(connection, source_path).await?;
    let target = match open_file_for_write(connection, target_path, create_disposition).await {
        Ok(target) => target,
        Err(error) => {
            let _ = close(connection, source).await;
            return Err(error);
        }
    };
    let copy_result = copy_open_file_chunks(connection, &source, &target).await;
    let close_source_result = close(connection, source).await.map(|_| ());
    let close_target_result = close(connection, target).await.map(|_| ());
    match (copy_result, close_source_result.and(close_target_result)) {
        (Ok(bytes), Ok(())) => Ok(bytes),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

pub async fn request_resume_key(
    connection: &SmbConnectionActor,
    file: &SmbHandle,
) -> Result<[u8; SMB2_RESUME_KEY_BYTES], SmbConnectionError> {
    let response = connection
        .request(
            Smb2Command::Ioctl,
            Smb2IoctlRequest {
                ctl_code: FSCTL_SRV_REQUEST_RESUME_KEY,
                file_id: file.file_id,
                input: Vec::new(),
                max_input_response: 0,
                output: Vec::new(),
                max_output_response: 32,
                flags: Smb2IoctlFlags::IS_FSCTL,
            }
            .encode()?,
        )
        .await?;
    ensure_success(&response, Smb2Command::Ioctl)?;
    let response = Smb2IoctlResponse::decode_packet(&response.to_packet())?;
    decode_resume_key_response(&response.output)
}

pub async fn copy_chunks(
    connection: &SmbConnectionActor,
    target: &SmbHandle,
    resume_key: &[u8; SMB2_RESUME_KEY_BYTES],
    chunks: &[SmbCopyChunk],
) -> Result<SmbCopyChunkResponse, SmbConnectionError> {
    let response = connection
        .request(
            Smb2Command::Ioctl,
            Smb2IoctlRequest {
                ctl_code: FSCTL_SRV_COPYCHUNK,
                file_id: target.file_id,
                input: smb2_srv_copychunk_copy(resume_key, chunks)?,
                max_input_response: 0,
                output: Vec::new(),
                max_output_response: SRV_COPYCHUNK_RESPONSE_BYTES as u32,
                flags: Smb2IoctlFlags::IS_FSCTL,
            }
            .encode()?,
        )
        .await?;
    ensure_success(&response, Smb2Command::Ioctl)?;
    let response = Smb2IoctlResponse::decode_packet(&response.to_packet())?;
    decode_copychunk_response(&response.output)
}

pub fn smb2_srv_copychunk_copy(
    resume_key: &[u8; SMB2_RESUME_KEY_BYTES],
    chunks: &[SmbCopyChunk],
) -> Result<Vec<u8>, SmbClientError> {
    let chunk_count = u32::try_from(chunks.len()).map_err(|_| SmbClientError::FieldTooLarge {
        field: "srv_copychunk.chunk_count",
        length: chunks.len(),
        max: u32::MAX as usize,
    })?;
    let mut bytes =
        Vec::with_capacity(SRV_COPYCHUNK_COPY_FIXED_BYTES + chunks.len() * SRV_COPYCHUNK_BYTES);
    bytes.extend_from_slice(resume_key);
    push_u32(&mut bytes, chunk_count);
    push_u32(&mut bytes, 0);
    for chunk in chunks {
        push_u64(&mut bytes, chunk.source_offset);
        push_u64(&mut bytes, chunk.target_offset);
        push_u32(&mut bytes, chunk.length);
        push_u32(&mut bytes, 0);
    }
    Ok(bytes)
}

pub fn decode_resume_key_response(
    output: &[u8],
) -> Result<[u8; SMB2_RESUME_KEY_BYTES], SmbConnectionError> {
    if output.len() < SRV_REQUEST_RESUME_KEY_RESPONSE_MIN_BYTES {
        return Err(SmbClientError::ShortFrame {
            expected: SRV_REQUEST_RESUME_KEY_RESPONSE_MIN_BYTES,
            actual: output.len(),
        }
        .into());
    }
    let mut key = [0; SMB2_RESUME_KEY_BYTES];
    key.copy_from_slice(&output[..SMB2_RESUME_KEY_BYTES]);
    Ok(key)
}

pub fn decode_copychunk_response(
    output: &[u8],
) -> Result<SmbCopyChunkResponse, SmbConnectionError> {
    if output.len() < SRV_COPYCHUNK_RESPONSE_BYTES {
        return Err(SmbClientError::ShortFrame {
            expected: SRV_COPYCHUNK_RESPONSE_BYTES,
            actual: output.len(),
        }
        .into());
    }
    Ok(SmbCopyChunkResponse {
        chunks_written: le_u32(output, 0),
        chunk_bytes_written: le_u32(output, 4),
        total_bytes_written: le_u32(output, 8),
    })
}

async fn copy_open_file_chunks(
    connection: &SmbConnectionActor,
    source: &SmbHandle,
    target: &SmbHandle,
) -> Result<u64, SmbConnectionError> {
    if source.end_of_file == 0 {
        return Ok(0);
    }

    let resume_key = request_resume_key(connection, source).await?;
    let mut copied = 0u64;
    while copied < source.end_of_file {
        let chunks = copychunk_batch(copied, source.end_of_file);
        let expected_bytes = chunks
            .iter()
            .map(|chunk| u64::from(chunk.length))
            .sum::<u64>();
        let response = copy_chunks(connection, target, &resume_key, &chunks).await?;
        let written = u64::from(response.total_bytes_written);
        if written == 0 || written > expected_bytes {
            return Err(SmbConnectionError::ShortWrite {
                expected: expected_bytes.min(u64::from(u32::MAX)) as u32,
                actual: response.total_bytes_written,
            });
        }
        copied = copied.saturating_add(written);
    }
    Ok(copied)
}

fn copychunk_batch(copied: u64, total_size: u64) -> Vec<SmbCopyChunk> {
    let mut chunks = Vec::new();
    let mut offset = copied;
    while offset < total_size && chunks.len() < DEFAULT_COPYCHUNK_MAX_CHUNKS {
        let remaining = total_size - offset;
        let length = remaining.min(u64::from(DEFAULT_COPYCHUNK_CHUNK_BYTES)) as u32;
        chunks.push(SmbCopyChunk {
            source_offset: offset,
            target_offset: offset,
            length,
        });
        offset = offset.saturating_add(u64::from(length));
    }
    chunks
}

fn ensure_success(response: &SmbResponse, command: Smb2Command) -> Result<(), SmbConnectionError> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(SmbConnectionError::UnexpectedStatus {
            command,
            status: status.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copychunk_copy_encodes_resume_key_count_and_chunks() {
        let resume_key = [0x5a; SMB2_RESUME_KEY_BYTES];
        let encoded = smb2_srv_copychunk_copy(
            &resume_key,
            &[
                SmbCopyChunk {
                    source_offset: 10,
                    target_offset: 20,
                    length: 30,
                },
                SmbCopyChunk {
                    source_offset: 40,
                    target_offset: 50,
                    length: 60,
                },
            ],
        )
        .expect("encode");

        assert_eq!(&encoded[..SMB2_RESUME_KEY_BYTES], &resume_key);
        assert_eq!(le_u32(&encoded, 24), 2);
        assert_eq!(le_u32(&encoded, 28), 0);
        assert_eq!(u64::from_le_bytes(encoded[32..40].try_into().unwrap()), 10);
        assert_eq!(u64::from_le_bytes(encoded[40..48].try_into().unwrap()), 20);
        assert_eq!(le_u32(&encoded, 48), 30);
        assert_eq!(le_u32(&encoded, 52), 0);
        assert_eq!(u64::from_le_bytes(encoded[56..64].try_into().unwrap()), 40);
        assert_eq!(u64::from_le_bytes(encoded[64..72].try_into().unwrap()), 50);
        assert_eq!(le_u32(&encoded, 72), 60);
        assert_eq!(le_u32(&encoded, 76), 0);
    }

    #[test]
    fn resume_key_response_uses_first_24_bytes() {
        let mut output = vec![0x7b; 32];
        output[24..28].copy_from_slice(&8u32.to_le_bytes());

        let key = decode_resume_key_response(&output).expect("decode");

        assert_eq!(key, [0x7b; SMB2_RESUME_KEY_BYTES]);
    }

    #[test]
    fn copychunk_response_decodes_counts() {
        let mut output = vec![0; 12];
        output[0..4].copy_from_slice(&3u32.to_le_bytes());
        output[4..8].copy_from_slice(&1024u32.to_le_bytes());
        output[8..12].copy_from_slice(&2048u32.to_le_bytes());

        let response = decode_copychunk_response(&output).expect("decode");

        assert_eq!(
            response,
            SmbCopyChunkResponse {
                chunks_written: 3,
                chunk_bytes_written: 1024,
                total_bytes_written: 2048
            }
        );
    }

    #[test]
    fn copychunk_batch_uses_one_mib_chunks_and_in_flight_limit() {
        let chunks = copychunk_batch(0, u64::from(DEFAULT_COPYCHUNK_CHUNK_BYTES) * 20 + 7);

        assert_eq!(chunks.len(), DEFAULT_COPYCHUNK_MAX_CHUNKS);
        assert_eq!(chunks[0].source_offset, 0);
        assert_eq!(chunks[0].length, DEFAULT_COPYCHUNK_CHUNK_BYTES);
        assert_eq!(
            chunks[DEFAULT_COPYCHUNK_MAX_CHUNKS - 1].source_offset,
            u64::from(DEFAULT_COPYCHUNK_CHUNK_BYTES) * 15
        );
    }
}
