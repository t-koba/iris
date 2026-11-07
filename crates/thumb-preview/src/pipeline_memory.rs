use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::memory_budget::{MemoryBudget, MemoryBudgetPermit};
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::progress::ByteRange;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::debug;

pub(crate) const MEMORY_BUDGET_EXCEEDED_OPERATION: &str = "thumbnail input exceeds memory budget";
pub(crate) const MEMORY_BUDGET_UNKNOWN_LENGTH_OPERATION: &str =
    "thumbnail input size unknown for in-memory read";

#[derive(Debug)]
pub(crate) struct BufferedInput {
    pub(crate) data: Vec<u8>,
    _permit: MemoryBudgetPermit,
}

impl BufferedInput {
    pub(crate) fn into_data(self) -> Vec<u8> {
        self.data
    }
}

pub(crate) async fn read_full_with_budget(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    operation: &'static str,
) -> Result<BufferedInput, BackendError> {
    let handle = backend.read(object, None).await?;
    let Some(content_length) = handle.content_length else {
        return Err(BackendError::Unsupported {
            operation: MEMORY_BUDGET_UNKNOWN_LENGTH_OPERATION,
        });
    };
    let expected_len = usize::try_from(content_length).map_err(|_| BackendError::Unsupported {
        operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
    })?;
    let Some(permit) = MemoryBudget::global().acquire(expected_len).await else {
        return Err(BackendError::Unsupported {
            operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
        });
    };

    let mut data = Vec::with_capacity(expected_len);
    read_reserved_bytes(handle.stream, &mut data, content_length).await?;
    debug!(
        "read thumbnail input into memory: {} bytes for {}",
        data.len(),
        operation
    );

    Ok(BufferedInput {
        data,
        _permit: permit,
    })
}

/// Reuses an already-downloaded prefix and requests only the remaining range when supported.
pub(crate) async fn read_full_with_prefix_with_budget(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    prefix: Vec<u8>,
    requested_prefix_len: usize,
    operation: &'static str,
) -> Result<BufferedInput, BackendError> {
    if prefix.is_empty() || !backend.capabilities().can_range_read {
        return read_full_with_budget(backend, object, operation).await;
    }

    // A short range response proves that the probe reached EOF, so no second request is needed.
    if prefix.len() < requested_prefix_len {
        let expected_len = prefix.len();
        let Some(permit) = MemoryBudget::global().acquire(expected_len).await else {
            return Err(BackendError::Unsupported {
                operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
            });
        };
        return Ok(BufferedInput {
            data: prefix,
            _permit: permit,
        });
    }

    let handle = backend
        .read(
            object,
            Some(ByteRange {
                offset: prefix.len() as u64,
                length: None,
            }),
        )
        .await?;
    let Some(remaining_length) = handle.content_length else {
        return Err(BackendError::Unsupported {
            operation: MEMORY_BUDGET_UNKNOWN_LENGTH_OPERATION,
        });
    };
    let total_length =
        (prefix.len() as u64)
            .checked_add(remaining_length)
            .ok_or(BackendError::Unsupported {
                operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
            })?;
    let expected_len = usize::try_from(total_length).map_err(|_| BackendError::Unsupported {
        operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
    })?;
    let Some(permit) = MemoryBudget::global().acquire(expected_len).await else {
        return Err(BackendError::Unsupported {
            operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
        });
    };

    let mut data = Vec::with_capacity(expected_len);
    data.extend_from_slice(&prefix);
    read_reserved_bytes(handle.stream, &mut data, remaining_length).await?;
    debug!(
        "read thumbnail input using cached prefix: {} bytes for {}",
        data.len(),
        operation
    );

    Ok(BufferedInput {
        data,
        _permit: permit,
    })
}

/// Append only the reserved remainder, then check EOF without retaining an extra byte.
async fn read_reserved_bytes(
    mut reader: Pin<Box<dyn AsyncRead + Send>>,
    data: &mut Vec<u8>,
    remaining_length: u64,
) -> Result<(), BackendError> {
    reader
        .as_mut()
        .take(remaining_length)
        .read_to_end(data)
        .await
        .map_err(|error| BackendError::Internal(Box::new(error)))?;
    if reader
        .read(&mut [0; 1])
        .await
        .map_err(|error| BackendError::Internal(Box::new(error)))?
        != 0
    {
        return Err(BackendError::Unsupported {
            operation: MEMORY_BUDGET_EXCEEDED_OPERATION,
        });
    }
    Ok(())
}

pub(crate) fn should_fallback_to_temp_file(error: &BackendError) -> bool {
    matches!(
        error,
        BackendError::Unsupported { operation }
            if *operation == MEMORY_BUDGET_EXCEEDED_OPERATION
                || *operation == MEMORY_BUDGET_UNKNOWN_LENGTH_OPERATION
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{object, MemoryBackend};
    use std::sync::atomic::Ordering;

    async fn read(
        backend: &MemoryBackend,
        prefix_len: usize,
    ) -> Result<BufferedInput, BackendError> {
        let object = object("input");
        if prefix_len == 0 {
            read_full_with_budget(backend, &object, "test").await
        } else {
            read_full_with_prefix_with_budget(
                backend,
                &object,
                backend.bytes[..prefix_len].to_vec(),
                prefix_len,
                "test",
            )
            .await
        }
    }

    #[tokio::test]
    async fn understated_lengths_fall_back_after_only_one_extra_byte() {
        for prefix_len in [0, 2] {
            for remaining_length in [0, 3] {
                let backend = MemoryBackend {
                    bytes: b"prefix and oversized suffix".to_vec(),
                    content_length: Some(Some(remaining_length)),
                    ..MemoryBackend::default()
                };
                let error = read(&backend, prefix_len).await.expect_err("overrun");
                assert!(matches!(
                    error,
                    BackendError::Unsupported {
                        operation: MEMORY_BUDGET_EXCEEDED_OPERATION
                    }
                ));
                assert!(should_fallback_to_temp_file(&error));
                assert_eq!(
                    backend.bytes_read.load(Ordering::Relaxed),
                    remaining_length as usize + 1
                );
                let reads = backend.reads.lock().expect("reads");
                assert_eq!(reads.len(), 1);
                assert_eq!(
                    reads[0].as_ref().map(|range| range.offset),
                    (prefix_len != 0).then_some(prefix_len as u64)
                );
            }
        }
    }

    #[tokio::test]
    async fn accurate_lengths_preserve_the_full_input() {
        for prefix_len in [0, 2] {
            for bytes in [b"".as_slice(), b"ab", b"abcdef"] {
                if prefix_len > bytes.len() {
                    continue;
                }
                let backend = MemoryBackend {
                    bytes: bytes.to_vec(),
                    ..MemoryBackend::default()
                };
                let input = read(&backend, prefix_len).await.expect("full input");
                assert_eq!(input.data, bytes);
                assert_eq!(
                    backend.bytes_read.load(Ordering::Relaxed),
                    bytes.len() - prefix_len
                );
            }
        }
    }

    #[tokio::test]
    async fn unknown_lengths_fall_back_without_consuming_the_stream() {
        for prefix_len in [0, 2] {
            let backend = MemoryBackend {
                bytes: b"abcdef".to_vec(),
                content_length: Some(None),
                ..MemoryBackend::default()
            };
            let error = read(&backend, prefix_len)
                .await
                .expect_err("unknown length");
            assert!(matches!(
                error,
                BackendError::Unsupported {
                    operation: MEMORY_BUDGET_UNKNOWN_LENGTH_OPERATION
                }
            ));
            assert!(should_fallback_to_temp_file(&error));
            assert_eq!(backend.bytes_read.load(Ordering::Relaxed), 0);
        }
    }
}
