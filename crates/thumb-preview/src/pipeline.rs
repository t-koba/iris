use crate::pipeline_memory::{
    read_full_with_prefix_with_budget, should_fallback_to_temp_file, BufferedInput,
};
use crate::{encoded::passthrough_encoded_image, image_generic, jpeg, pdf, raw, video};
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::thumbnail::{
    ImageFormat, ThumbnailHint, ThumbnailResult, ThumbnailSource,
};
use std::io;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tracing::debug;

const JPEG_HEADER_PROBE_BYTES: u64 = 64 * 1024;
const ENCODED_IMAGE_HEADER_PROBE_BYTES: u64 = 128 * 1024;

/// Fetches thumbnails in order of preference:
///
/// 1. the backend's direct thumbnail API, when the capability exists
/// 2. embedded preview extraction
/// 3. decoding from a partial read
/// 4. decoding the whole file
pub struct ThumbnailPipeline;

fn cancellation_error() -> BackendError {
    BackendError::Internal(Box::new(io::Error::new(
        io::ErrorKind::Interrupted,
        "thumbnail generation canceled",
    )))
}

fn ensure_not_cancelled(cancel_token: &CancellationToken) -> Result<(), BackendError> {
    if cancel_token.is_cancelled() {
        Err(cancellation_error())
    } else {
        Ok(())
    }
}

impl ThumbnailPipeline {
    /// Tries each pipeline stage in order through the backend adapter.
    pub async fn generate(
        backend: &dyn BackendAdapter,
        object: &ObjectRef,
        hint: &ThumbnailHint,
        content_type: Option<&str>,
    ) -> Result<ThumbnailResult, BackendError> {
        let cancel_token = CancellationToken::new();
        Self::generate_with_cancellation(backend, object, hint, content_type, &cancel_token).await
    }

    #[tracing::instrument(
        skip(backend, hint, cancel_token),
        fields(path = %object.display_path, max_dimension = hint.max_dimension)
    )]
    pub async fn generate_with_cancellation(
        backend: &dyn BackendAdapter,
        object: &ObjectRef,
        hint: &ThumbnailHint,
        content_type: Option<&str>,
        cancel_token: &CancellationToken,
    ) -> Result<ThumbnailResult, BackendError> {
        ensure_not_cancelled(cancel_token)?;
        if backend.capabilities().can_direct_thumbnail {
            match backend.get_thumbnail(object, hint.clone()).await {
                Ok(result) => {
                    debug!("thumbnail via DirectThumbnail API");
                    return Ok(result);
                }
                Err(BackendError::Unsupported { .. }) => {}
                Err(e) => return Err(e),
            }
        }

        let file_type = classify_for_local_path(Path::new(&object.display_path), content_type);
        let input_context = ImageInputContext {
            backend,
            object,
            hint,
            content_type,
            cancel_token,
        };

        match file_type {
            FileType::Jpeg => {
                // Read only the head for EXIF thumbnail extraction.
                let header_data =
                    read_partial(backend, object, 0, JPEG_HEADER_PROBE_BYTES, cancel_token).await?;
                if let Some(result) =
                    jpeg::extract_embedded_jpeg_thumbnail(&header_data, hint.max_dimension)
                {
                    return Ok(result);
                }
                // Small JPEGs are passed through without decoding.
                if let Some((width, height)) = jpeg::jpeg_dimensions(&header_data) {
                    if width <= hint.max_dimension && height <= hint.max_dimension {
                        let full_data = match input_context
                            .complete(header_data, JPEG_HEADER_PROBE_BYTES, "jpeg passthrough")
                            .await?
                        {
                            CompleteImageInput::Buffered(data) => data,
                            CompleteImageInput::Generated(result) => return Ok(result),
                        };
                        ensure_not_cancelled(cancel_token)?;
                        return Ok(ThumbnailResult {
                            data: full_data.into_data(),
                            format: ImageFormat::Jpeg,
                            width,
                            height,
                            source: ThumbnailSource::OriginalEncoded,
                        });
                    }
                }
                // No EXIF thumbnail and a large JPEG: decode in memory (no temp file).
                let full_data = match input_context
                    .complete(header_data, JPEG_HEADER_PROBE_BYTES, "jpeg decode")
                    .await?
                {
                    CompleteImageInput::Buffered(data) => data,
                    CompleteImageInput::Generated(result) => return Ok(result),
                };
                let hint = hint.clone();
                decode_in_memory(full_data, hint, cancel_token).await
            }
            FileType::Image => {
                let header_data = read_partial(
                    backend,
                    object,
                    0,
                    ENCODED_IMAGE_HEADER_PROBE_BYTES,
                    cancel_token,
                )
                .await?;
                // If the header probe allows passthrough, read everything and return it as is.
                if let Some(info) = image_generic::probe_passthrough_candidate(&header_data, hint) {
                    let full_data = match input_context
                        .complete(
                            header_data,
                            ENCODED_IMAGE_HEADER_PROBE_BYTES,
                            "encoded image passthrough",
                        )
                        .await?
                    {
                        CompleteImageInput::Buffered(data) => data,
                        CompleteImageInput::Generated(result) => return Ok(result),
                    };
                    ensure_not_cancelled(cancel_token)?;
                    return Ok(passthrough_encoded_image(
                        full_data.into_data(),
                        info,
                        ThumbnailSource::OriginalEncoded,
                    ));
                }
                // Otherwise decode in memory (no temp file).
                let full_data = match input_context
                    .complete(
                        header_data,
                        ENCODED_IMAGE_HEADER_PROBE_BYTES,
                        "encoded image decode",
                    )
                    .await?
                {
                    CompleteImageInput::Buffered(data) => data,
                    CompleteImageInput::Generated(result) => return Ok(result),
                };
                let hint = hint.clone();
                decode_in_memory(full_data, hint, cancel_token).await
            }
            FileType::Raw => {
                if let Some(result) = raw::extract_raw_thumbnail_from_backend_with_cancellation(
                    backend,
                    object,
                    hint.max_dimension,
                    cancel_token,
                )
                .await?
                {
                    return Ok(result);
                }
                Err(BackendError::Unsupported {
                    operation: "raw embedded preview missing",
                })
            }
            FileType::Pdf => {
                generate_from_downloaded_temp_path(
                    backend,
                    object,
                    hint,
                    content_type,
                    cancel_token,
                )
                .await
            }
            FileType::Video => {
                if !video::supports_video_thumbnails() {
                    return Err(BackendError::Unsupported {
                        operation: "video thumbnail generation without video feature",
                    });
                }
                generate_from_downloaded_temp_path(
                    backend,
                    object,
                    hint,
                    content_type,
                    cancel_token,
                )
                .await
            }
            FileType::Other => Err(BackendError::Unsupported {
                operation: "thumbnail for unknown file type",
            }),
        }
    }

    /// Generates a thumbnail straight from a local path, bypassing the backend adapter.
    pub fn generate_from_path(
        file_path: &Path,
        hint: &ThumbnailHint,
        content_type: Option<&str>,
    ) -> Option<ThumbnailResult> {
        let cancel_token = CancellationToken::new();
        Self::generate_from_path_result_with_cancellation(
            file_path,
            hint,
            content_type,
            &cancel_token,
        )
        .ok()
    }

    pub fn generate_from_path_result_with_cancellation(
        file_path: &Path,
        hint: &ThumbnailHint,
        content_type: Option<&str>,
        cancel_token: &CancellationToken,
    ) -> Result<ThumbnailResult, BackendError> {
        ensure_not_cancelled(cancel_token)?;
        let file_type = classify_for_local_path(file_path, content_type);
        let ext = file_path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);

        match file_type {
            FileType::Jpeg => local_thumbnail_result(
                jpeg::extract_jpeg_thumbnail_from_path(file_path, hint.max_dimension),
                "jpeg thumbnail generation failed",
                cancel_token,
            ),
            FileType::Image => local_thumbnail_result(
                image_generic::extract_image_thumbnail(file_path, hint),
                "image thumbnail generation failed",
                cancel_token,
            ),
            FileType::Raw => {
                if !raw::supports_any_raw_thumbnails()
                    || ext
                        .as_deref()
                        .is_some_and(|extension| !raw::supports_raw_extension(extension))
                {
                    return Err(BackendError::Unsupported {
                        operation: "raw thumbnail generation without raw feature",
                    });
                }

                let result = raw::extract_raw_thumbnail_with_cancellation(
                    file_path,
                    hint.max_dimension,
                    cancel_token,
                );
                if let Some(thumbnail) = result {
                    return Ok(thumbnail);
                }
                ensure_not_cancelled(cancel_token)?;
                Err(BackendError::Unsupported {
                    operation: "raw embedded preview missing",
                })
            }
            FileType::Video => {
                if !video::supports_video_thumbnails() {
                    return Err(BackendError::Unsupported {
                        operation: "video thumbnail generation without video feature",
                    });
                }
                local_thumbnail_result(
                    video::extract_video_thumbnail(file_path, hint.max_dimension),
                    "video thumbnail generation failed",
                    cancel_token,
                )
            }
            FileType::Pdf => {
                if !pdf::supports_pdf_thumbnails() {
                    return Err(BackendError::Unsupported {
                        operation: "pdf thumbnail generation without pdf feature",
                    });
                }
                local_thumbnail_result(
                    pdf::extract_pdf_thumbnail(file_path, hint.max_dimension),
                    "pdf thumbnail generation failed",
                    cancel_token,
                )
            }
            FileType::Other => {
                // Unknown formats are still tried with the `image` crate for local files.
                local_thumbnail_result(
                    image_generic::extract_image_thumbnail(file_path, hint),
                    "image thumbnail generation failed",
                    cancel_token,
                )
            }
        }
    }
}

enum FileType {
    Jpeg,
    Image, // PNG, TIFF, BMP, GIF, WebP, ... decoded by the image crate
    Raw,
    Video,
    Pdf,
    Other,
}

fn classify_content_type(content_type: Option<&str>) -> FileType {
    let ct = match content_type {
        Some(ct) => ct.to_lowercase(),
        None => return FileType::Other,
    };

    if ct.contains("jpeg") || ct.contains("jpg") {
        FileType::Jpeg
    } else if ct.contains("pdf") {
        FileType::Pdf
    } else if ct.contains("video") {
        FileType::Video
    } else if is_raw_content_type(&ct) {
        FileType::Raw
    } else if ct.starts_with("image/")
        || ct.contains("vnd.ms-photo")
        || ct.contains("jpeg-xr")
        || ct.contains("jpegxl")
    {
        FileType::Image
    } else {
        FileType::Other
    }
}

fn classify_for_local_path(file_path: &Path, content_type: Option<&str>) -> FileType {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    // RAW extensions win over MIME so DNG is not treated as image/tiff.
    if raw::is_known_raw_extension(&ext) {
        return FileType::Raw;
    }

    let by_mime = classify_content_type(content_type);
    if !matches!(by_mime, FileType::Other) {
        return by_mime;
    }

    if matches!(ext.as_str(), "jpg" | "jpeg") {
        FileType::Jpeg
    } else if matches!(
        ext.as_str(),
        "png"
            | "gif"
            | "bmp"
            | "webp"
            | "tif"
            | "tiff"
            | "ico"
            | "avif"
            | "heic"
            | "heif"
            | "hif"
            | "jp2"
            | "j2k"
            | "j2c"
            | "jpc"
            | "jpf"
            | "jpx"
            | "jpm"
            | "jxr"
            | "hdp"
            | "wdp"
            | "jxl"
    ) {
        FileType::Image
    } else if ext == "pdf" {
        FileType::Pdf
    } else if matches!(ext.as_str(), "mp4" | "mov" | "mkv" | "avi" | "mj2") {
        FileType::Video
    } else {
        FileType::Other
    }
}

fn is_raw_content_type(ct: &str) -> bool {
    ct.contains("x-canon-cr2")
        || ct.contains("x-canon-cr3")
        || ct.contains("x-nikon-nef")
        || ct.contains("x-sony-arw")
        || ct.contains("x-fuji-raf")
        || ct.contains("x-olympus-orf")
        || ct.contains("x-panasonic-rw2")
        || ct.contains("x-adobe-dng")
        || ct.contains("x-sigma-x3f")
        || ct.contains("x-raw")
}

enum CompleteImageInput {
    Buffered(BufferedInput),
    Generated(ThumbnailResult),
}

struct ImageInputContext<'a> {
    backend: &'a dyn BackendAdapter,
    object: &'a ObjectRef,
    hint: &'a ThumbnailHint,
    content_type: Option<&'a str>,
    cancel_token: &'a CancellationToken,
}

impl ImageInputContext<'_> {
    async fn complete(
        &self,
        prefix: Vec<u8>,
        requested_prefix_len: u64,
        operation: &'static str,
    ) -> Result<CompleteImageInput, BackendError> {
        match read_full_with_prefix_with_budget(
            self.backend,
            self.object,
            prefix,
            requested_prefix_len as usize,
            operation,
        )
        .await
        {
            Ok(data) => Ok(CompleteImageInput::Buffered(data)),
            Err(error) if should_fallback_to_temp_file(&error) => {
                generate_from_downloaded_temp_path(
                    self.backend,
                    self.object,
                    self.hint,
                    self.content_type,
                    self.cancel_token,
                )
                .await
                .map(CompleteImageInput::Generated)
            }
            Err(error) => Err(error),
        }
    }
}

async fn read_partial(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    offset: u64,
    length: u64,
    cancel_token: &CancellationToken,
) -> Result<Vec<u8>, BackendError> {
    ensure_not_cancelled(cancel_token)?;
    let handle = backend
        .read(
            object,
            Some(ByteRange {
                offset,
                length: Some(length),
            }),
        )
        .await?;

    let mut data = Vec::with_capacity(length as usize);
    let mut reader = handle.stream;
    reader
        .read_to_end(&mut data)
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    ensure_not_cancelled(cancel_token)?;

    Ok(data)
}

async fn decode_in_memory(
    data: BufferedInput,
    hint: ThumbnailHint,
    cancel_token: &CancellationToken,
) -> Result<ThumbnailResult, BackendError> {
    ensure_not_cancelled(cancel_token)?;
    tokio::task::spawn_blocking(move || {
        image_generic::extract_image_thumbnail_from_bytes(&data.data, &hint).ok_or(
            BackendError::Unsupported {
                operation: "thumbnail generation from in-memory data",
            },
        )
    })
    .await
    .map_err(|e| BackendError::Internal(Box::new(io::Error::other(e.to_string()))))?
}

fn temp_path_for_object(object: &ObjectRef) -> std::path::PathBuf {
    let extension = Path::new(&object.display_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .unwrap_or("bin");
    let unique_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "iris-thumb-{}-{}.{}",
        std::process::id(),
        unique_id,
        extension
    ))
}

async fn stream_to_temp_file(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    cancel_token: &CancellationToken,
) -> Result<std::path::PathBuf, BackendError> {
    ensure_not_cancelled(cancel_token)?;
    let temp_path = temp_path_for_object(object);
    let handle = backend.read(object, None).await?;
    let mut reader = handle.stream;
    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;

    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        ensure_not_cancelled(cancel_token)?;
        let bytes_read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| BackendError::Internal(Box::new(error)))?;
        if bytes_read == 0 {
            break;
        }
        if let Err(error) = file.write_all(&buffer[..bytes_read]).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(BackendError::Internal(Box::new(error)));
        }
    }

    if let Err(error) = file.flush().await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(BackendError::Internal(Box::new(error)));
    }

    Ok(temp_path)
}

async fn generate_from_downloaded_temp_path(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    hint: &ThumbnailHint,
    content_type: Option<&str>,
    cancel_token: &CancellationToken,
) -> Result<ThumbnailResult, BackendError> {
    let temp_path = stream_to_temp_file(backend, object, cancel_token).await?;
    let decode_path = temp_path.clone();
    let hint = hint.clone();
    let content_type = content_type.map(str::to_owned);
    let cancel_token = cancel_token.clone();

    let result = tokio::task::spawn_blocking(move || {
        ThumbnailPipeline::generate_from_path_result_with_cancellation(
            &decode_path,
            &hint,
            content_type.as_deref(),
            &cancel_token,
        )
    })
    .await
    .map_err(|error| BackendError::Internal(Box::new(io::Error::other(error.to_string()))))?;

    let _ = tokio::fs::remove_file(&temp_path).await;
    result
}

fn local_thumbnail_result(
    thumbnail: Option<ThumbnailResult>,
    operation: &'static str,
    cancel_token: &CancellationToken,
) -> Result<ThumbnailResult, BackendError> {
    if let Some(thumbnail) = thumbnail {
        return Ok(thumbnail);
    }
    ensure_not_cancelled(cancel_token)?;
    Err(BackendError::Internal(Box::new(io::Error::other(
        operation,
    ))))
}

#[cfg(test)]
mod tests;
