use super::{S3Adapter, CONDITIONAL_WRITE_RETRIES};
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::Future;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::options::ConflictResolution;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) const SINGLE_PART_UPLOAD_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MULTIPART_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const MULTIPART_UPLOAD_CONCURRENCY: usize = 8;

type MultipartUploadFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<(i32, aws_sdk_s3::types::CompletedPart), BackendError>>
            + Send
            + 'a,
    >,
>;

// Only a completion response carries a retryable status. Part/start/reader
// failures must never be mistaken for permission to replay a whole upload.
struct MultipartFailure {
    status: Option<u16>,
    error: BackendError,
}

impl From<BackendError> for MultipartFailure {
    fn from(error: BackendError) -> Self {
        Self {
            status: None,
            error,
        }
    }
}

impl S3Adapter {
    pub(super) async fn put_object_bytes(
        &self,
        requested_key: &str,
        key: &str,
        data: Vec<u8>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), BackendError> {
        self.require_conditional_writes(on_conflict)?;
        let conditional = !matches!(on_conflict, ConflictResolution::Overwrite);
        // ByteStream clones share these buffered bytes; no stream is re-read.
        let body = bytes::Bytes::from(data);
        let mut key = key.to_string();
        for retry in 0..=CONDITIONAL_WRITE_RETRIES {
            let error = match self
                .client()
                .await?
                .put_object()
                .bucket(&self.config.bucket)
                .key(&key)
                .set_if_none_match(conditional.then(|| "*".into()))
                .body(aws_sdk_s3::primitives::ByteStream::from(body.clone()))
                .send()
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) => error,
            };
            let status = error
                .raw_response()
                .map(|response| response.status().as_u16());
            if conditional
                && retry < CONDITIONAL_WRITE_RETRIES
                && (status == Some(409)
                    || (status == Some(412) && matches!(on_conflict, ConflictResolution::Rename)))
            {
                key = self
                    .resolve_write_key_excluding(
                        requested_key.to_string(),
                        on_conflict,
                        (status == Some(412)).then_some(key.as_str()),
                    )
                    .await?;
            } else {
                return Err(self.write_error("put", &key, conditional, status, error));
            }
        }
        unreachable!("bounded put loop always returns")
    }

    pub(super) async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        chunk: Vec<u8>,
    ) -> Result<aws_sdk_s3::types::CompletedPart, BackendError> {
        let response = self
            .client()
            .await?
            .upload_part()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(aws_sdk_s3::primitives::ByteStream::from(chunk))
            .send()
            .await
            .map_err(|error| self.network_error("upload_part", error))?;

        Ok(aws_sdk_s3::types::CompletedPart::builder()
            .part_number(part_number)
            .set_e_tag(response.e_tag().map(str::to_string))
            .build())
    }

    pub(super) async fn abort_multipart_upload_quietly(&self, key: &str, upload_id: &str) {
        let Ok(client) = self.client().await else {
            return;
        };
        let _ = client
            .abort_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await;
    }

    pub(super) async fn complete_multipart_upload_by_key(
        &self,
        key: &str,
        upload_id: &str,
        parts: Vec<aws_sdk_s3::types::CompletedPart>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), BackendError> {
        self.complete_multipart_upload_attempt(key, upload_id, parts, on_conflict)
            .await
            .map_err(|failure| failure.error)
    }

    async fn complete_multipart_upload_attempt(
        &self,
        key: &str,
        upload_id: &str,
        parts: Vec<aws_sdk_s3::types::CompletedPart>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), MultipartFailure> {
        self.require_conditional_writes(on_conflict)?;
        self.client()
            .await?
            .complete_multipart_upload()
            .set_if_none_match(
                (!matches!(on_conflict, ConflictResolution::Overwrite)).then(|| "*".into()),
            )
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(
                aws_sdk_s3::types::CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send()
            .await
            .map_err(|error| {
                let status = error
                    .raw_response()
                    .map(|response| response.status().as_u16());
                MultipartFailure {
                    status,
                    error: self.write_error(
                        "complete_multipart_upload",
                        key,
                        !matches!(on_conflict, ConflictResolution::Overwrite),
                        status,
                        error,
                    ),
                }
            })?;
        Ok(())
    }

    pub(super) async fn start_multipart_upload(&self, key: &str) -> Result<String, BackendError> {
        let response = self
            .client()
            .await?
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| self.network_error("create_multipart_upload", error))?;
        response
            .upload_id()
            .map(str::to_string)
            .ok_or_else(|| self.network_error("create_multipart_upload", "missing upload id"))
    }

    pub(super) async fn upload_multipart_chunks(
        &self,
        key: &str,
        chunks: Vec<Vec<u8>>,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), BackendError> {
        self.upload_multipart_attempt(key, chunks, reader, on_conflict)
            .await
            .map_err(|failure| failure.error)
    }

    async fn upload_multipart_attempt(
        &self,
        key: &str,
        mut chunks: Vec<Vec<u8>>,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), MultipartFailure> {
        self.require_conditional_writes(on_conflict)?;
        let upload_id = self.start_multipart_upload(key).await?;

        let result = async {
            let mut parts = Vec::new();
            let mut in_flight = FuturesUnordered::<MultipartUploadFuture<'_>>::new();
            let mut part_number = 1i32;

            for chunk in chunks.drain(..) {
                enqueue_upload_part(self, key, &upload_id, part_number, chunk, &mut in_flight);
                if in_flight.len() >= MULTIPART_UPLOAD_CONCURRENCY {
                    parts.push(await_uploaded_part(&mut in_flight).await?);
                }
                part_number += 1;
            }

            loop {
                let chunk = read_chunk(&mut reader, MULTIPART_CHUNK_BYTES).await?;
                if chunk.is_empty() {
                    break;
                }
                enqueue_upload_part(self, key, &upload_id, part_number, chunk, &mut in_flight);
                if in_flight.len() >= MULTIPART_UPLOAD_CONCURRENCY {
                    parts.push(await_uploaded_part(&mut in_flight).await?);
                }
                part_number += 1;
            }

            while !in_flight.is_empty() {
                parts.push(await_uploaded_part(&mut in_flight).await?);
            }

            parts.sort_by_key(|(part_number, _)| *part_number);
            self.complete_multipart_upload_attempt(
                key,
                &upload_id,
                parts.into_iter().map(|(_, part)| part).collect(),
                on_conflict,
            )
            .await
        }
        .await;

        if result.is_err() {
            self.abort_multipart_upload_quietly(key, &upload_id).await;
        }
        result
    }

    // Only the Vec-backed entry point may restart a multipart upload. Streamed
    // writes and session completion cannot reconstruct already-consumed parts.
    pub(super) async fn upload_buffered_multipart(
        &self,
        requested_key: &str,
        mut key: String,
        data: Vec<u8>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), BackendError> {
        for retry in 0..=CONDITIONAL_WRITE_RETRIES {
            let chunks = data
                .chunks(MULTIPART_CHUNK_BYTES)
                .map(<[u8]>::to_vec)
                .collect();
            let failure = match self
                .upload_multipart_attempt(&key, chunks, Box::pin(tokio::io::empty()), on_conflict)
                .await
            {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            if !matches!(on_conflict, ConflictResolution::Overwrite)
                && retry < CONDITIONAL_WRITE_RETRIES
                && (failure.status == Some(409)
                    || (failure.status == Some(412)
                        && matches!(on_conflict, ConflictResolution::Rename)))
            {
                key = self
                    .resolve_write_key_excluding(
                        requested_key.to_string(),
                        on_conflict,
                        (failure.status == Some(412)).then_some(key.as_str()),
                    )
                    .await?;
            } else {
                return Err(failure.error);
            }
        }
        unreachable!("bounded multipart loop always returns")
    }

    pub(super) async fn upload_from_reader(
        &self,
        requested_key: &str,
        key: &str,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
        on_conflict: &ConflictResolution,
    ) -> Result<(), BackendError> {
        if content_length.is_some_and(|len| len <= SINGLE_PART_UPLOAD_MAX_BYTES as u64) {
            let small = read_chunk(&mut reader, SINGLE_PART_UPLOAD_MAX_BYTES).await?;
            return self
                .put_object_bytes(requested_key, key, small, on_conflict)
                .await;
        }

        let first_chunk = read_chunk(&mut reader, MULTIPART_CHUNK_BYTES).await?;
        if first_chunk.is_empty() {
            return self
                .put_object_bytes(requested_key, key, Vec::new(), on_conflict)
                .await;
        }

        if content_length.is_none() && first_chunk.len() < SINGLE_PART_UPLOAD_MAX_BYTES {
            let second_chunk = read_chunk(&mut reader, MULTIPART_CHUNK_BYTES).await?;
            if second_chunk.is_empty() {
                return self
                    .put_object_bytes(requested_key, key, first_chunk, on_conflict)
                    .await;
            }
            return self
                .upload_multipart_chunks(key, vec![first_chunk, second_chunk], reader, on_conflict)
                .await;
        }

        self.upload_multipart_chunks(key, vec![first_chunk], reader, on_conflict)
            .await
    }
}

pub(super) async fn read_chunk(
    reader: &mut Pin<Box<dyn AsyncRead + Send>>,
    chunk_size: usize,
) -> Result<Vec<u8>, BackendError> {
    let mut buffer = vec![0u8; chunk_size];
    let mut bytes_read = 0usize;
    while bytes_read < chunk_size {
        let read_now = reader
            .read(&mut buffer[bytes_read..])
            .await
            .map_err(|error| BackendError::Internal(Box::new(error)))?;
        if read_now == 0 {
            break;
        }
        bytes_read += read_now;
    }
    buffer.truncate(bytes_read);
    Ok(buffer)
}

fn enqueue_upload_part<'a>(
    adapter: &'a S3Adapter,
    key: &'a str,
    upload_id: &'a str,
    part_number: i32,
    chunk: Vec<u8>,
    in_flight: &mut FuturesUnordered<MultipartUploadFuture<'a>>,
) {
    in_flight.push(Box::pin(async move {
        adapter
            .upload_part(key, upload_id, part_number, chunk)
            .await
            .map(|part| (part_number, part))
    }));
}

async fn await_uploaded_part(
    in_flight: &mut FuturesUnordered<MultipartUploadFuture<'_>>,
) -> Result<(i32, aws_sdk_s3::types::CompletedPart), BackendError> {
    in_flight.next().await.ok_or_else(|| {
        BackendError::Internal(Box::new(std::io::Error::other(
            "multipart upload queue unexpectedly empty",
        )))
    })?
}
