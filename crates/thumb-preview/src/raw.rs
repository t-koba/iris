use crate::io::binary_reader::{BlockingBinaryReader, RemoteWindowSource, SourceFile};
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::metadata::MediaMetadata;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::thumbnail::{ThumbnailResult, ThumbnailSource};
use std::collections::HashSet;
use std::io;
use std::io::Seek;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use tracing::debug;

mod bmff_boxes;
mod bmff_samples;
mod candidates;
mod containers;
mod structured_metadata;
mod tiff;
mod tiff_numeric;

use bmff_boxes::*;
use bmff_samples::*;
use candidates::*;
use containers::*;
use structured_metadata::*;
use tiff::*;
use tiff_numeric::*;

const RAW_CORE_EXTENSIONS: &[&str] = &["arw", "cr3", "dng", "nef"];

const MIN_JPEG_BYTES: usize = 256;
const JPEG_PROBE_BYTES: usize = 256 * 1024;
const MAX_IFD_VISITS: usize = 96;
const MAX_IFD_ENTRIES: usize = 512;
const MAX_TAG_ARRAY_COUNT: usize = 256;

const TIFF_MAGIC_STANDARD: u16 = 42;
const TIFF_MAGIC_BIGTIFF: u16 = 43;
const TIFF_MAGIC_RW2: u16 = 0x55;
const BIGTIFF_OFFSET_SIZE: u16 = 8;
const BIGTIFF_ENTRY_BYTES: usize = 20;

const TAG_NEW_SUBFILE_TYPE: u16 = 0x00FE;
const TAG_SUBFILE_TYPE: u16 = 0x00FF;
const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_BITS_PER_SAMPLE: u16 = 0x0102;
const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_PANASONIC_SENSOR_WIDTH: u16 = 0x0002;
const TAG_PANASONIC_SENSOR_HEIGHT: u16 = 0x0003;
const TAG_PANASONIC_SENSOR_TOP_BORDER: u16 = 0x0004;
const TAG_PANASONIC_SENSOR_LEFT_BORDER: u16 = 0x0005;
const TAG_PANASONIC_SENSOR_BOTTOM_BORDER: u16 = 0x0006;
const TAG_PANASONIC_SENSOR_RIGHT_BORDER: u16 = 0x0007;
const TAG_PANASONIC_BITS_PER_SAMPLE: u16 = 0x000A;
const TAG_PANASONIC_RAW_VERSION: u16 = 0x0001;
const TAG_PANASONIC_RAW_FORMAT: u16 = 0x002D;
const TAG_COMPRESSION: u16 = 0x0103;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_TILE_OFFSETS: u16 = 0x0144;
const TAG_TILE_BYTE_COUNTS: u16 = 0x0145;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_PANASONIC_JPG_FROM_RAW: u16 = 0x002E;
const TAG_PANASONIC_JPG_FROM_RAW2: u16 = 0x0127;
const TAG_EXIF_IFD_POINTER: u16 = 0x8769;
const TAG_JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;

const COMPRESSION_JPEG_OLD: u16 = 6;
const COMPRESSION_JPEG_NEW: u16 = 7;

const TIFF_TYPE_BYTE: u16 = 1;
const TIFF_TYPE_ASCII: u16 = 2;
const TIFF_TYPE_SHORT: u16 = 3;
const TIFF_TYPE_LONG: u16 = 4;
const TIFF_TYPE_RATIONAL: u16 = 5;
const TIFF_TYPE_SBYTE: u16 = 6;
const TIFF_TYPE_UNDEFINED: u16 = 7;
const TIFF_TYPE_SSHORT: u16 = 8;
const TIFF_TYPE_SLONG: u16 = 9;
const TIFF_TYPE_SRATIONAL: u16 = 10;
const TIFF_TYPE_FLOAT: u16 = 11;
const TIFF_TYPE_DOUBLE: u16 = 12;
const TIFF_TYPE_IFD: u16 = 13;
const TIFF_TYPE_LONG8: u16 = 16;
const TIFF_TYPE_SLONG8: u16 = 17;
const TIFF_TYPE_IFD8: u16 = 18;

const BOX_TYPE_MOOV: [u8; 4] = *b"moov";
const BOX_TYPE_TRAK: [u8; 4] = *b"trak";
const BOX_TYPE_MDIA: [u8; 4] = *b"mdia";
const BOX_TYPE_MINF: [u8; 4] = *b"minf";
const BOX_TYPE_STBL: [u8; 4] = *b"stbl";
const BOX_TYPE_STSC: [u8; 4] = *b"stsc";
const BOX_TYPE_STCO: [u8; 4] = *b"stco";
const BOX_TYPE_CO64: [u8; 4] = *b"co64";
const BOX_TYPE_STSZ: [u8; 4] = *b"stsz";
const BOX_TYPE_STZ2: [u8; 4] = *b"stz2";

#[cfg(feature = "raw-full")]
const X3F_SIGNATURE: &[u8; 4] = b"FOVb";
#[cfg(feature = "raw-full")]
const X3F_IMAGE_DIRECTORY_ID: &[u8; 4] = b"IMA2";
#[cfg(feature = "raw-full")]
const X3F_DIRECTORY_MIN_VERSION: u32 = 0x0002_0000;
#[cfg(feature = "raw-full")]
const X3F_IMAGE_HEADER_BYTES: usize = 28;
#[cfg(feature = "raw-full")]
const X3F_FILE_VERSION_OFFSET: u64 = 4;
#[cfg(feature = "raw-full")]
const X3F_IMAGE_WIDTH_OFFSET: u64 = 0x28;
#[cfg(feature = "raw-full")]
const X3F_IMAGE_HEIGHT_OFFSET: u64 = 0x2C;
#[cfg(feature = "raw-full")]
const X3F_ROTATION_OFFSET: u64 = 0x30;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub media: MediaMetadata,
    pub exif_entries: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub(crate) struct StructuredRawMetadata {
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) entries: Vec<(String, String)>,
}

/// Extracts the embedded JPEG preview; RAW data itself is never decoded because it is too slow.
/// Returns `None` when there is no embedded preview (the UI shows a fallback icon).
///
/// The container structure is parsed directly; there is no brute-force JPEG marker scan.
pub fn extract_raw_thumbnail(file_path: &Path, max_dimension: u32) -> Option<ThumbnailResult> {
    let cancel_token = CancellationToken::new();
    extract_raw_thumbnail_with_cancellation(file_path, max_dimension, &cancel_token)
}

pub fn extract_raw_thumbnail_with_cancellation(
    file_path: &Path,
    max_dimension: u32,
    cancel_token: &CancellationToken,
) -> Option<ThumbnailResult> {
    if cancel_token.is_cancelled() {
        return None;
    }
    let ext = supported_raw_extension(file_path)?;

    let source = SourceFile::open(file_path)?;
    let candidate = match ext.as_str() {
        "cr3" => extract_cr3_embedded_jpeg(&source, max_dimension),
        #[cfg(feature = "raw-full")]
        "raf" => extract_raf_embedded_jpeg(&source, max_dimension),
        #[cfg(feature = "raw-full")]
        "x3f" => extract_x3f_embedded_jpeg(&source, max_dimension),
        _ => extract_tiff_embedded_jpeg(&source, max_dimension),
    }?;
    if cancel_token.is_cancelled() {
        return None;
    }

    debug!(
        "extracted structured embedded JPEG preview from RAW: {:?} ({}x{})",
        file_path, candidate.width, candidate.height
    );

    let width = candidate.width;
    let height = candidate.height;
    let jpeg = candidate.into_bytes(&source)?;
    crate::jpeg::fit_existing_or_resize_jpeg(
        jpeg,
        width,
        height,
        max_dimension,
        ThumbnailSource::EmbeddedPreview,
        ThumbnailSource::EmbeddedPreview,
    )
}

/// Same as above over backend range reads: candidates come from parsing the structure
/// and only the selected JPEG range is fetched.
pub async fn extract_raw_thumbnail_from_backend_with_cancellation(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    max_dimension: u32,
    cancel_token: &CancellationToken,
) -> Result<Option<ThumbnailResult>, BackendError> {
    ensure_not_cancelled(cancel_token)?;
    let Some(ext) = supported_raw_extension(Path::new(&object.display_path)) else {
        return Ok(None);
    };
    if !backend.capabilities().can_range_read {
        return Ok(None);
    }

    let Some(len) = backend.stat(object).await?.size else {
        return Ok(None);
    };
    let found = with_blocking_remote_source(
        backend,
        object,
        len,
        |source| {
            let candidate = match ext.as_str() {
                "cr3" => extract_cr3_embedded_jpeg(source, max_dimension),
                #[cfg(feature = "raw-full")]
                "raf" => extract_raf_embedded_jpeg(source, max_dimension),
                #[cfg(feature = "raw-full")]
                "x3f" => extract_x3f_embedded_jpeg(source, max_dimension),
                _ => extract_tiff_embedded_jpeg(source, max_dimension),
            }?;
            let (width, height) = (candidate.width, candidate.height);
            Some((candidate.into_bytes(source)?, width, height))
        },
        cancel_token,
    )?;
    let Some((jpeg, width, height)) = found else {
        return Ok(None);
    };
    ensure_not_cancelled(cancel_token)?;

    debug!(
        "extracted structured embedded JPEG preview from remote RAW: {} ({}x{})",
        object.display_path, width, height
    );

    Ok(crate::jpeg::fit_existing_or_resize_jpeg(
        jpeg,
        width,
        height,
        max_dimension,
        ThumbnailSource::EmbeddedPreview,
        ThumbnailSource::EmbeddedPreview,
    ))
}

fn ensure_not_cancelled(cancel_token: &CancellationToken) -> Result<(), BackendError> {
    if cancel_token.is_cancelled() {
        Err(BackendError::Internal(Box::new(io::Error::new(
            io::ErrorKind::Interrupted,
            "raw thumbnail generation canceled",
        ))))
    } else {
        Ok(())
    }
}

/// Runs `f` on a scoped thread, where blocking reads of the remote object are allowed.
fn with_blocking_remote_source<T>(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    len: u64,
    f: impl FnOnce(&RemoteWindowSource<'_>) -> T + Send,
    cancel_token: &CancellationToken,
) -> Result<T, BackendError>
where
    T: Send,
{
    let blocking = RemoteWindowSource::new(
        backend,
        object,
        len,
        tokio::runtime::Handle::current(),
        Some(cancel_token.clone()),
    );
    let output = std::thread::scope(|scope| match scope.spawn(|| f(&blocking)).join() {
        Ok(output) => output,
        Err(payload) => std::panic::resume_unwind(payload),
    });
    if let Some(error) = blocking.take_error() {
        Err(error)
    } else {
        Ok(output)
    }
}

/// Size and header entries of an X3F file.
#[cfg(feature = "raw-full")]
pub(crate) fn x3f_header_metadata(
    source: &impl BlockingBinaryReader,
) -> Option<StructuredRawMetadata> {
    extract_x3f_structured_metadata(source)
}

pub fn extract_raw_metadata(file_path: &Path) -> Option<RawMetadata> {
    let ext = supported_raw_extension(file_path)?;
    let source = SourceFile::open(file_path)?;

    let mut structured = match ext.as_str() {
        #[cfg(feature = "raw-full")]
        "x3f" => extract_x3f_structured_metadata(&source),
        _ => extract_tiff_structured_metadata(&source),
    }
    .unwrap_or_default();

    let (exif_width, exif_height, exif_entries) = extract_filtered_exif_metadata(file_path);
    if structured.width.is_none() {
        structured.width = exif_width;
    }
    if structured.height.is_none() {
        structured.height = exif_height;
    }
    merge_metadata_entries(&mut structured.entries, exif_entries);

    Some(RawMetadata {
        width: structured.width,
        height: structured.height,
        media: crate::metadata::media_metadata_from_display_entries(
            structured.width,
            structured.height,
            &structured.entries,
        ),
        exif_entries: structured.entries,
    })
}

fn supported_raw_extension(file_path: &Path) -> Option<String> {
    let ext = file_path.extension()?.to_str()?.to_ascii_lowercase();
    if !supports_raw_extension(&ext) {
        debug!("unsupported RAW extension: {}", ext);
        return None;
    }
    Some(ext)
}

pub fn supports_raw_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    if RAW_CORE_EXTENSIONS.contains(&ext.as_str()) {
        return cfg!(any(feature = "raw-core", feature = "raw-full"));
    }
    #[cfg(feature = "raw-full")]
    if matches!(ext.as_str(), "cr2" | "orf" | "raf" | "rw2" | "x3f") {
        return true;
    }
    false
}

pub const fn supports_any_raw_thumbnails() -> bool {
    cfg!(any(feature = "raw-core", feature = "raw-full"))
}

#[derive(Clone, Debug)]
enum StructuredCandidateSource {
    FileRange { start: u64, len: usize },
    InlineBytes(Vec<u8>),
}

#[derive(Clone, Debug)]
struct StructuredJpegCandidate {
    source: StructuredCandidateSource,
    width: u32,
    height: u32,
    byte_len: usize,
}

impl StructuredJpegCandidate {
    fn longest_edge(&self) -> u32 {
        self.width.max(self.height)
    }

    fn into_bytes(self, source: &impl BlockingBinaryReader) -> Option<Vec<u8>> {
        match self.source {
            StructuredCandidateSource::FileRange { start, len } => source.read_range(start, len),
            StructuredCandidateSource::InlineBytes(bytes) => Some(bytes),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Endian {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug)]
struct FileBoxHeader {
    start: u64,
    size: u64,
    header_len: u64,
    typ: [u8; 4],
}

impl FileBoxHeader {
    fn data_start(self) -> u64 {
        self.start + self.header_len
    }

    fn end(self) -> u64 {
        self.start + self.size
    }
}

#[derive(Clone, Copy, Debug)]
struct PrimaryImageDimensionsCandidate {
    width: u32,
    height: u32,
    reduced_resolution: bool,
}

impl PrimaryImageDimensionsCandidate {
    fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_metadata;
