#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("trash operation failed: {message}")]
    Trash { message: String },

    #[error("file watch failed: {message}")]
    FileWatch { message: String },

    #[error("default app launch failed: {message}")]
    DefaultApp { message: String },

    #[error("credential store error: {message}")]
    CredentialStore { message: String },

    #[error("mount operation failed: {message}")]
    Mount { message: String },

    #[error("path error: {message}")]
    Path { message: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
