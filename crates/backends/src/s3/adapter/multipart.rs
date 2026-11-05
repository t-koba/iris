use super::S3Adapter;
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::Future;
use iris_core_domain::types::error::BackendError;
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

impl S3Adapter {
    pub(super) async fn put_object_bytes(
        &self,
        key: &str,
        data: Vec<u8>,
    ) -> Result<(), BackendError> {
        self.client()
            .await?
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .body(aws_sdk_s3::primitives::ByteStream::from(data))
            .send()
            .await
            .map_err(|error| self.network_error("put", error))?;
        Ok(())
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
    ) -> Result<(), BackendError> {
        self.client()
            .await?
            .complete_multipart_upload()
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
            .map_err(|error| self.network_error("complete_multipart_upload", error))?;
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
        mut chunks: Vec<Vec<u8>>,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
    ) -> Result<(), BackendError> {
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
            self.complete_multipart_upload_by_key(
                key,
                &upload_id,
                parts.into_iter().map(|(_, part)| part).collect(),
            )
            .await
        }
        .await;

        if result.is_err() {
            self.abort_multipart_upload_quietly(key, &upload_id).await;
        }
        result
    }

    pub(super) async fn upload_from_reader(
        &self,
        key: &str,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
    ) -> Result<(), BackendError> {
        if content_length.is_some_and(|len| len <= SINGLE_PART_UPLOAD_MAX_BYTES as u64) {
            let small = read_chunk(&mut reader, SINGLE_PART_UPLOAD_MAX_BYTES).await?;
            return self.put_object_bytes(key, small).await;
        }

        let first_chunk = read_chunk(&mut reader, MULTIPART_CHUNK_BYTES).await?;
        if first_chunk.is_empty() {
            return self.put_object_bytes(key, Vec::new()).await;
        }

        if content_length.is_none() && first_chunk.len() < SINGLE_PART_UPLOAD_MAX_BYTES {
            let second_chunk = read_chunk(&mut reader, MULTIPART_CHUNK_BYTES).await?;
            if second_chunk.is_empty() {
                return self.put_object_bytes(key, first_chunk).await;
            }
            return self
                .upload_multipart_chunks(key, vec![first_chunk, second_chunk], reader)
                .await;
        }

        self.upload_multipart_chunks(key, vec![first_chunk], reader)
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
