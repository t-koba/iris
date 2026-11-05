use crate::error::PlatformError;

/// OS credential store (macOS Keychain / Windows Credential Manager / Linux Secret Service) via `keyring`.
pub struct OsCredentialStore;

impl OsCredentialStore {
    pub fn store(&self, service: &str, key: &str, value: &str) -> Result<(), PlatformError> {
        let entry =
            keyring::Entry::new(service, key).map_err(|e| PlatformError::CredentialStore {
                message: e.to_string(),
            })?;
        entry
            .set_password(value)
            .map_err(|e| PlatformError::CredentialStore {
                message: e.to_string(),
            })
    }

    pub fn load(&self, service: &str, key: &str) -> Result<String, PlatformError> {
        let entry =
            keyring::Entry::new(service, key).map_err(|e| PlatformError::CredentialStore {
                message: e.to_string(),
            })?;
        entry
            .get_password()
            .map_err(|e| PlatformError::CredentialStore {
                message: e.to_string(),
            })
    }

    pub fn delete(&self, service: &str, key: &str) -> Result<(), PlatformError> {
        let entry =
            keyring::Entry::new(service, key).map_err(|e| PlatformError::CredentialStore {
                message: e.to_string(),
            })?;
        entry
            .delete_credential()
            .map_err(|e| PlatformError::CredentialStore {
                message: e.to_string(),
            })
    }
}
