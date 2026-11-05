use crate::token::TokenSet;
use iris_platform_integration::credential_store::OsCredentialStore;
use iris_platform_integration::error::PlatformError;
use thiserror::Error;

const SERVICE_PREFIX: &str = "iris-fm";

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("platform credential store error: {0}")]
    Platform(#[from] PlatformError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("credential not found: {key}")]
    NotFound { key: String },
}

/// Stores and loads a `TokenSet` per backend id in the OS credential store.
pub struct CredentialManager {
    store: OsCredentialStore,
}

impl Default for CredentialManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialManager {
    pub fn new() -> Self {
        Self {
            store: OsCredentialStore,
        }
    }

    pub fn save_token(
        &self,
        backend_id: &str,
        account_id: &str,
        token: &TokenSet,
    ) -> Result<(), CredentialError> {
        let service = format!("{}-{}", SERVICE_PREFIX, backend_id);
        let value = serde_json::to_string(token)?;
        self.store.store(&service, account_id, &value)?;
        Ok(())
    }

    pub fn load_token(
        &self,
        backend_id: &str,
        account_id: &str,
    ) -> Result<TokenSet, CredentialError> {
        let service = format!("{}-{}", SERVICE_PREFIX, backend_id);
        let value =
            self.store
                .load(&service, account_id)
                .map_err(|_| CredentialError::NotFound {
                    key: format!("{}:{}", backend_id, account_id),
                })?;
        let token: TokenSet = serde_json::from_str(&value)?;
        Ok(token)
    }
}
