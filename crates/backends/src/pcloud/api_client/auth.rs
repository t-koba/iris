use super::{PCloudApiClient, PCloudResponse};
use iris_core_domain::types::error::BackendError;

impl PCloudApiClient {
    pub(super) fn ensure_success<T>(
        endpoint: &str,
        body: &PCloudResponse<T>,
    ) -> Result<(), BackendError> {
        if body.result == 0 {
            return Ok(());
        }

        let message = match &body.error {
            Some(err) => format!("pCloud {} failed: {}", endpoint, err),
            None => format!(
                "pCloud {} failed with result={} and no error message",
                endpoint, body.result
            ),
        };
        Err(BackendError::Internal(Box::new(std::io::Error::other(
            message,
        ))))
    }
}
