//! Synthetic RAW containers for tests and benchmarks; each embeds the given JPEG preview.

use std::io::Cursor;
use std::path::PathBuf;

const TIFF_MAGIC_STANDARD: u16 = 42;
const TIFF_MAGIC_BIGTIFF: u16 = 43;
const TIFF_MAGIC_RW2: u16 = 0x55;
const TAG_COMPRESSION: u16 = 0x0103;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;
const TAG_PANASONIC_JPG_FROM_RAW: u16 = 0x002E;
const TIFF_TYPE_SHORT: u16 = 3;
const TIFF_TYPE_LONG: u16 = 4;
const TIFF_TYPE_UNDEFINED: u16 = 7;
const TIFF_TYPE_LONG8: u16 = 16;
const COMPRESSION_JPEG: u32 = 7;

/// A JPEG with a gradient pattern, so previews of different sizes are told apart.
pub fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([
            ((x * 17 + y * 3) % 256) as u8,
            ((x * 5 + y * 11) % 256) as u8,
            ((x * 13 + y * 7) % 256) as u8,
        ])
    });
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut output, image::ImageFormat::Jpeg)
        .expect("encode jpeg");
    output.into_inner()
}

/// A path in the temp directory that no other test uses.
pub fn temp_path(extension: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "iris-thumb-preview-{}-{id}.{extension}",
        std::process::id()
    ))
}

/// One little-endian TIFF IFD entry.
pub fn ifd_entry(tag: u16, field_type: u16, count: u32, value: u32) -> Vec<u8> {
    [
        tag.to_le_bytes().as_slice(),
        &field_type.to_le_bytes(),
        &count.to_le_bytes(),
        &value.to_le_bytes(),
    ]
    .concat()
}

/// A little-endian TIFF with one IFD holding `entries`, padded to `data_offset`, then `data`.
pub fn tiff(magic: u16, entries: &[Vec<u8>], data_offset: usize, data: &[u8]) -> Vec<u8> {
    let mut tiff = [b"II".as_slice(), &magic.to_le_bytes(), &8u32.to_le_bytes()].concat();
    tiff.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in entries {
        tiff.extend_from_slice(entry);
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.resize(tiff.len().max(data_offset), 0);
    tiff.extend_from_slice(data);
    tiff
}

/// TIFF whose IFD points at the JPEG through JPEGInterchangeFormat.
pub fn tiff_with_jpeg(jpeg_offset: u32, jpeg: &[u8]) -> Vec<u8> {
    let entries = [
        ifd_entry(TAG_JPEG_INTERCHANGE_FORMAT, TIFF_TYPE_LONG, 1, jpeg_offset),
        ifd_entry(
            TAG_JPEG_INTERCHANGE_FORMAT_LENGTH,
            TIFF_TYPE_LONG,
            1,
            jpeg.len() as u32,
        ),
    ];
    tiff(TIFF_MAGIC_STANDARD, &entries, jpeg_offset as usize, jpeg)
}

/// TIFF with the JPEG split into two strips at `first_offset` and `second_offset`; the strip
/// arrays are SHORT values stored inline in the IFD.
pub fn tiff_with_inline_strips(first_offset: u16, second_offset: u16, jpeg: &[u8]) -> Vec<u8> {
    let (first, second) = jpeg.split_at(jpeg.len() / 2);
    let pair = |a: u16, b: u16| u32::from(a) | (u32::from(b) << 16);
    let entries = [
        ifd_entry(TAG_COMPRESSION, TIFF_TYPE_SHORT, 1, COMPRESSION_JPEG),
        ifd_entry(
            TAG_STRIP_OFFSETS,
            TIFF_TYPE_SHORT,
            2,
            pair(first_offset, second_offset),
        ),
        ifd_entry(
            TAG_STRIP_BYTE_COUNTS,
            TIFF_TYPE_SHORT,
            2,
            pair(first.len() as u16, second.len() as u16),
        ),
    ];
    let mut tiff = tiff(TIFF_MAGIC_STANDARD, &entries, first_offset as usize, first);
    tiff.resize(tiff.len().max(second_offset as usize), 0);
    tiff.extend_from_slice(second);
    tiff
}

/// TIFF with the JPEG split into two contiguous strips listed in LONG arrays.
pub fn tiff_with_strips(jpeg: &[u8]) -> Vec<u8> {
    let (first, second) = jpeg.split_at(jpeg.len() / 2);
    let offsets_at = 64u32;
    let counts_at = offsets_at + 8;
    let first_offset = 96u32;
    let entries = [
        ifd_entry(TAG_COMPRESSION, TIFF_TYPE_SHORT, 1, COMPRESSION_JPEG),
        ifd_entry(TAG_STRIP_OFFSETS, TIFF_TYPE_LONG, 2, offsets_at),
        ifd_entry(TAG_STRIP_BYTE_COUNTS, TIFF_TYPE_LONG, 2, counts_at),
    ];
    let arrays = [
        first_offset,
        first_offset + first.len() as u32,
        first.len() as u32,
        second.len() as u32,
    ]
    .iter()
    .flat_map(|value| value.to_le_bytes())
    .collect::<Vec<_>>();
    let mut tiff = tiff(TIFF_MAGIC_STANDARD, &entries, offsets_at as usize, &arrays);
    tiff.resize(first_offset as usize, 0);
    tiff.extend_from_slice(jpeg);
    tiff
}

/// BigTIFF whose IFD points at the JPEG through JPEGInterchangeFormat.
pub fn bigtiff_with_jpeg(jpeg_offset: u64, jpeg: &[u8]) -> Vec<u8> {
    let entry = |tag: u16, value: u64| {
        [
            tag.to_le_bytes().as_slice(),
            &TIFF_TYPE_LONG8.to_le_bytes(),
            &1u64.to_le_bytes(),
            &value.to_le_bytes(),
        ]
        .concat()
    };
    let mut tiff = [
        b"II".as_slice(),
        &TIFF_MAGIC_BIGTIFF.to_le_bytes(),
        &8u16.to_le_bytes(),
        &0u16.to_le_bytes(),
        &16u64.to_le_bytes(),
        &2u64.to_le_bytes(),
        &entry(TAG_JPEG_INTERCHANGE_FORMAT, jpeg_offset),
        &entry(TAG_JPEG_INTERCHANGE_FORMAT_LENGTH, jpeg.len() as u64),
        &0u64.to_le_bytes(),
    ]
    .concat();
    tiff.resize(tiff.len().max(jpeg_offset as usize), 0);
    tiff.extend_from_slice(jpeg);
    tiff
}

/// Panasonic RW2 (TIFF with its own magic) with the JPEG in a JpgFromRaw tag.
pub fn rw2_with_jpeg(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 80u32;
    let entries = [ifd_entry(
        TAG_PANASONIC_JPG_FROM_RAW,
        TIFF_TYPE_UNDEFINED,
        jpeg.len() as u32,
        jpeg_offset,
    )];
    tiff(TIFF_MAGIC_RW2, &entries, jpeg_offset as usize, jpeg)
}

/// Fujifilm RAF whose header points at the JPEG.
pub fn raf_with_jpeg(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 92u32;
    let mut raf = vec![0u8; jpeg_offset as usize];
    raf[84..88].copy_from_slice(&jpeg_offset.to_be_bytes());
    raf[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
    raf.extend_from_slice(jpeg);
    raf
}

/// Sigma X3F with one directory entry for an image section holding the JPEG.
pub fn x3f_with_jpeg(jpeg: &[u8]) -> Vec<u8> {
    const IMAGE_HEADER_BYTES: usize = 28;
    let (width, height) = crate::jpeg::jpeg_dimensions(jpeg).expect("jpeg dimensions");
    let image_offset = 128u32;
    let mut x3f = vec![0u8; image_offset as usize];
    x3f[0..4].copy_from_slice(b"FOVb");
    for value in [0, 0, 0, 0, width, height, 0] {
        x3f.extend_from_slice(&value.to_le_bytes());
    }
    x3f.extend_from_slice(jpeg);

    let directory_offset = x3f.len() as u32;
    for value in [
        0,
        0x0002_0000,
        1,
        image_offset,
        (IMAGE_HEADER_BYTES + jpeg.len()) as u32,
    ] {
        x3f.extend_from_slice(&value.to_le_bytes());
    }
    x3f.extend_from_slice(b"IMA2");
    x3f.extend_from_slice(&directory_offset.to_le_bytes());
    x3f
}

pub fn bmff_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        ((8 + payload.len()) as u32).to_be_bytes().as_slice(),
        typ,
        payload,
    ]
    .concat()
}

fn be32(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

/// CR3 whose only chunk holds a 320-byte non-JPEG sample followed by the JPEG. With `compact`,
/// sample sizes come from an `stz2` box with 16-bit fields instead of `stsz`.
pub fn cr3_with_jpeg(jpeg: &[u8], compact: bool) -> Vec<u8> {
    let invalid_sample = vec![0u8; 320];
    let stsc = bmff_box(b"stsc", &be32(&[0, 1, 1, 2, 1]));
    let sizes = if compact {
        let mut payload = be32(&[0, 16, 2]);
        payload.extend_from_slice(&(invalid_sample.len() as u16).to_be_bytes());
        payload.extend_from_slice(&(jpeg.len() as u16).to_be_bytes());
        bmff_box(b"stz2", &payload)
    } else {
        bmff_box(
            b"stsz",
            &be32(&[0, 0, 2, invalid_sample.len() as u32, jpeg.len() as u32]),
        )
    };
    let moov = |chunk_offset: u32| {
        let stco = bmff_box(b"stco", &be32(&[0, 1, chunk_offset]));
        let stbl = bmff_box(b"stbl", &[stsc.clone(), sizes.clone(), stco].concat());
        bmff_box(
            b"moov",
            &bmff_box(b"trak", &bmff_box(b"mdia", &bmff_box(b"minf", &stbl))),
        )
    };
    let moov = moov(moov(0).len() as u32);
    [moov, invalid_sample, jpeg.to_vec()].concat()
}

/// A remote object at `path` on the mock backend.
pub fn object(path: &str) -> iris_core_domain::types::object::ObjectRef {
    use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("mock".to_string()),
            stable_id: StableId(path.to_string()),
        },
        parent_stable_id: None,
        display_path: path.to_string(),
    }
}

/// Serves `bytes` as every object, with range reads, and records the range of each read.
#[derive(Default)]
pub struct MemoryBackend {
    pub bytes: Vec<u8>,
    pub reads: std::sync::Arc<std::sync::Mutex<Vec<Option<ByteRange>>>>,
    pub content_type: Option<String>,
    /// Override the response length (including an unknown length) independently of its bytes.
    pub content_length: Option<Option<u64>>,
    pub bytes_read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::{ListCursor, ListPage};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{ObjectRef, StableId};
use iris_core_domain::types::operation::OperationTicket;
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use std::pin::Pin;
use tokio::io::AsyncRead;

fn unsupported<T>(operation: &'static str) -> Result<T, BackendError> {
    Err(BackendError::Unsupported { operation })
}

#[async_trait::async_trait]
impl BackendAdapter for MemoryBackend {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        unsupported("connect")
    }

    async fn list_names(
        &self,
        _dir: &ObjectRef,
        _cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        unsupported("list_names")
    }

    async fn stat(&self, _object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size: Some(self.bytes.len() as u64),
            mtime: None,
            ctime: None,
            content_type: self.content_type.clone(),
            etag: None,
        })
    }

    async fn read(
        &self,
        _object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        self.reads.lock().expect("lock reads").push(range.clone());
        let len = self.bytes.len();
        let (start, end) = match &range {
            Some(range) => {
                let start = (range.offset as usize).min(len);
                let end = range
                    .length
                    .map_or(len, |length| start.saturating_add(length as usize).min(len));
                (start, end)
            }
            None => (0, len),
        };
        let data = self.bytes[start..end].to_vec();
        Ok(ReadHandle {
            content_length: self.content_length.unwrap_or(Some(data.len() as u64)),
            stream: Box::pin(CountedReader {
                reader: std::io::Cursor::new(data),
                bytes_read: self.bytes_read.clone(),
            }),
            content_range: range,
        })
    }

    async fn write_object(
        &self,
        _dst: &ObjectRef,
        _data: Vec<u8>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        unsupported("write_object")
    }

    async fn write_from_reader(
        &self,
        _dst: &ObjectRef,
        _reader: Pin<Box<dyn AsyncRead + Send>>,
        _content_length: Option<u64>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        unsupported("write_from_reader")
    }

    async fn mkdir(&self, _parent: &ObjectRef, _name: &str) -> Result<ObjectRef, BackendError> {
        unsupported("mkdir")
    }

    async fn copy(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        unsupported("copy")
    }

    async fn move_object(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        unsupported("move_object")
    }

    async fn delete(
        &self,
        _object: &ObjectRef,
        _mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        unsupported("delete")
    }

    async fn get_metadata(
        &self,
        _object: &ObjectRef,
        _level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        unsupported("get_metadata")
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        Ok(object.key.stable_id.clone())
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet {
            can_range_read: true,
            ..CapabilitySet::default()
        }
    }
}

struct CountedReader {
    reader: std::io::Cursor<Vec<u8>>,
    bytes_read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl AsyncRead for CountedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.reader).poll_read(cx, buf);
        self.bytes_read.fetch_add(
            buf.filled().len() - before,
            std::sync::atomic::Ordering::Relaxed,
        );
        result
    }
}
