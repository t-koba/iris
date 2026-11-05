use crate::types::progress::ByteRange;
use std::pin::Pin;
use tokio::io::AsyncRead;

/// Async byte stream returned by `read()`; covers only the requested range for range reads.
pub struct ReadHandle {
    pub stream: Pin<Box<dyn AsyncRead + Send>>,
    pub content_length: Option<u64>,
    pub content_range: Option<ByteRange>,
}
