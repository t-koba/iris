use crate::types::object::ObjectRef;
use std::fmt;

#[derive(Debug)]
pub struct AuthErrorDetail {
    pub message: String,
    pub is_token_expired: bool,
}

impl fmt::Display for AuthErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug)]
pub struct NetworkErrorDetail {
    pub message: String,
    pub is_timeout: bool,
}

impl fmt::Display for NetworkErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Common error type; adapters convert their internal errors into it.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// Authentication failed or the token expired.
    #[error("authentication error: {0}")]
    Auth(AuthErrorDetail),

    /// Unreachable or timed out.
    #[error("network error: {0}")]
    Network(NetworkErrorDetail),

    #[error("not found: {}", .object.display_path)]
    NotFound { object: ObjectRef },

    #[error("permission denied: {operation} on {}", .object.display_path)]
    PermissionDenied {
        object: ObjectRef,
        operation: &'static str,
    },

    #[error("storage full")]
    StorageFull,

    #[error("unsupported operation: {operation}")]
    Unsupported { operation: &'static str },

    /// An object with the same name already exists.
    #[error("conflict: {}", .object.display_path)]
    Conflict { object: ObjectRef },

    /// Backend-specific error.
    #[error("internal error: {0}")]
    Internal(Box<dyn std::error::Error + Send + Sync>),
}
