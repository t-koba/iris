//! Thumbnails, previews and metadata for images, RAW files, video and PDF, from local paths or
//! through range reads on a backend.

pub mod encoded;
pub mod image_generic;
pub mod io;
pub mod jpeg;
#[cfg(target_os = "macos")]
mod macos_ffi;
pub mod metadata;
mod modern_image;
pub mod pdf;
pub mod pdf_text;
pub mod pipeline;
mod pipeline_memory;
pub mod raw;
pub mod scheduler;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod video;
