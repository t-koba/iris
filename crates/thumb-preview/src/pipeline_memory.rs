use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::memory_budget::{MemoryBudget, MemoryBudgetPermit};
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::progress::ByteRange;
use tokio::io::AsyncReadExt;
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
    let mut reader = handle.stream;
    reader
        .read_to_end(&mut data)
        .await
        .map_err(|error| BackendError::Internal(Box::new(error)))?;
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
    let mut reader = handle.stream;
    reader
        .read_to_end(&mut data)
        .await
        .map_err(|error| BackendError::Internal(Box::new(error)))?;
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

pub(crate) fn should_fallback_to_temp_file(error: &BackendError) -> bool {
    matches!(
        error,
        BackendError::Unsupported { operation }
            if *operation == MEMORY_BUDGET_EXCEEDED_OPERATION
                || *operation == MEMORY_BUDGET_UNKNOWN_LENGTH_OPERATION
    )
}
