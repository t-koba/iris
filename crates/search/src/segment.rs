use crate::bloom::{BloomSignature, BLOOM_BYTES};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"IRISIDX1";
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 48;
const RECORD_BYTES: usize = 96;
const FLAG_HAS_CONTENT_SIGNATURE: u32 = 1 << 0;
const FLAG_HAS_GPS: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum IndexedKind {
    File = 0,
    Directory = 1,
    Image = 2,
    Video = 3,
    Pdf = 4,
    Other = 255,
}

impl IndexedKind {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::File,
            1 => Self::Directory,
            2 => Self::Image,
            3 => Self::Video,
            4 => Self::Pdf,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SegmentRecord {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime_secs: i64,
    pub kind: IndexedKind,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub capture_secs: Option<i64>,
    pub capture_offset_min: Option<i16>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub gps_lat: Option<f32>,
    pub gps_lon: Option<f32>,
    pub content_signature: Option<BloomSignature>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    records: Vec<SegmentRecord>,
}

impl Segment {
    pub fn new(records: Vec<SegmentRecord>) -> Self {
        Self { records }
    }

    pub fn records(&self) -> &[SegmentRecord] {
        &self.records
    }

    pub fn into_records(self) -> Vec<SegmentRecord> {
        self.records
    }

    pub fn scan<F>(&self, mut predicate: F) -> Vec<&SegmentRecord>
    where
        F: FnMut(&SegmentRecord) -> bool,
    {
        self.records
            .iter()
            .filter(|record| predicate(record))
            .collect()
    }

    pub fn write_atomic(&self, path: &Path) -> Result<(), SegmentError> {
        let bytes = self.to_bytes()?;
        let tmp_path = tmp_path_for(path);
        {
            let mut file = File::create(&tmp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self, SegmentError> {
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        Self::from_bytes(&bytes)
    }

    pub fn read_cache(path: &Path) -> Result<Option<Self>, SegmentError> {
        match Self::read(path) {
            Ok(segment) => Ok(Some(segment)),
            Err(SegmentError::Io(_)) if !path.exists() => Ok(None),
            Err(error) if error.is_rebuildable_cache_error() => match std::fs::remove_file(path) {
                Ok(()) => Ok(None),
                Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(None)
                }
                Err(remove_error) => Err(remove_error.into()),
            },
            Err(error) => Err(error),
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, SegmentError> {
        let mut strings = StringTableBuilder::default();
        let mut encoded_records = Vec::with_capacity(self.records.len() * RECORD_BYTES);
        let mut signatures = Vec::new();

        for record in &self.records {
            let path_ref = strings.intern(&record.path)?;
            let name_ref = strings.intern(&record.name)?;
            let camera_ref = strings.intern_optional(record.camera.as_deref())?;
            let lens_ref = strings.intern_optional(record.lens.as_deref())?;
            let signature_ref = if let Some(signature) = &record.content_signature {
                let offset = signatures.len();
                signatures.extend_from_slice(signature.as_bytes());
                u32::try_from(offset).map_err(|_| SegmentError::TooLarge)?
            } else {
                u32::MAX
            };
            encode_record(
                &mut encoded_records,
                EncodedRecord {
                    path_ref,
                    name_ref,
                    size: record.size,
                    mtime_secs: record.mtime_secs,
                    kind: record.kind,
                    flags: record_flags(record),
                    width: record.width.unwrap_or(u32::MAX),
                    height: record.height.unwrap_or(u32::MAX),
                    capture_secs: record.capture_secs.unwrap_or(i64::MIN),
                    capture_offset_min: record.capture_offset_min.unwrap_or(i16::MIN),
                    camera_ref,
                    lens_ref,
                    gps_lat_bits: record.gps_lat.unwrap_or(f32::NAN).to_bits(),
                    gps_lon_bits: record.gps_lon.unwrap_or(f32::NAN).to_bits(),
                    signature_ref,
                },
            );
        }

        let string_bytes = strings.into_bytes();
        let record_count = u32::try_from(self.records.len()).map_err(|_| SegmentError::TooLarge)?;
        let string_bytes_len =
            u32::try_from(string_bytes.len()).map_err(|_| SegmentError::TooLarge)?;
        let signature_bytes_len =
            u32::try_from(signatures.len()).map_err(|_| SegmentError::TooLarge)?;

        let mut bytes = vec![0; HEADER_BYTES];
        bytes.extend_from_slice(&encoded_records);
        bytes.extend_from_slice(&string_bytes);
        bytes.extend_from_slice(&signatures);
        let checksum = checksum64(&bytes[HEADER_BYTES..]);
        encode_header(
            &mut bytes[..HEADER_BYTES],
            Header {
                record_count,
                record_bytes: RECORD_BYTES as u32,
                string_bytes: string_bytes_len,
                signature_bytes: signature_bytes_len,
                checksum,
            },
        );
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SegmentError> {
        if bytes.len() < HEADER_BYTES {
            return Err(SegmentError::Corrupt("segment header is truncated"));
        }
        let header = decode_header(&bytes[..HEADER_BYTES])?;
        if header.record_bytes != RECORD_BYTES as u32 {
            return Err(SegmentError::UnsupportedVersion);
        }
        let body = &bytes[HEADER_BYTES..];
        if checksum64(body) != header.checksum {
            return Err(SegmentError::ChecksumMismatch);
        }

        let records_len = checked_mul(header.record_count as usize, RECORD_BYTES)?;
        let strings_start = HEADER_BYTES + records_len;
        let signatures_start = strings_start + header.string_bytes as usize;
        let end = signatures_start + header.signature_bytes as usize;
        if bytes.len() != end {
            return Err(SegmentError::Corrupt("segment section lengths are invalid"));
        }

        let strings = StringTable::new(&bytes[strings_start..signatures_start])?;
        let signatures = &bytes[signatures_start..end];
        let mut records = Vec::with_capacity(header.record_count as usize);
        for chunk in bytes[HEADER_BYTES..strings_start].chunks_exact(RECORD_BYTES) {
            let encoded = decode_record(chunk)?;
            let content_signature = if encoded.signature_ref == u32::MAX {
                None
            } else {
                let start = encoded.signature_ref as usize;
                let end = start + BLOOM_BYTES;
                if end > signatures.len() {
                    return Err(SegmentError::Corrupt("signature reference is out of range"));
                }
                let mut bytes = [0; BLOOM_BYTES];
                bytes.copy_from_slice(&signatures[start..end]);
                Some(BloomSignature::from_bytes(bytes))
            };
            records.push(SegmentRecord {
                path: strings.get(encoded.path_ref)?.to_string(),
                name: strings.get(encoded.name_ref)?.to_string(),
                size: encoded.size,
                mtime_secs: encoded.mtime_secs,
                kind: encoded.kind,
                width: optional_u32(encoded.width),
                height: optional_u32(encoded.height),
                capture_secs: optional_i64(encoded.capture_secs),
                capture_offset_min: optional_i16(encoded.capture_offset_min),
                camera: strings
                    .get_optional(encoded.camera_ref)?
                    .map(ToString::to_string),
                lens: strings
                    .get_optional(encoded.lens_ref)?
                    .map(ToString::to_string),
                gps_lat: optional_f32(encoded.gps_lat_bits),
                gps_lon: optional_f32(encoded.gps_lon_bits),
                content_signature,
            });
        }
        Ok(Self { records })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SegmentError {
    Io(String),
    Corrupt(&'static str),
    ChecksumMismatch,
    UnsupportedVersion,
    TooLarge,
}

impl SegmentError {
    fn is_rebuildable_cache_error(&self) -> bool {
        matches!(
            self,
            Self::Corrupt(_) | Self::ChecksumMismatch | Self::UnsupportedVersion
        )
    }
}

impl From<std::io::Error> for SegmentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Clone, Copy)]
struct Header {
    record_count: u32,
    record_bytes: u32,
    string_bytes: u32,
    signature_bytes: u32,
    checksum: u64,
}

#[derive(Clone, Copy)]
struct EncodedRecord {
    path_ref: u32,
    name_ref: u32,
    size: u64,
    mtime_secs: i64,
    kind: IndexedKind,
    flags: u32,
    width: u32,
    height: u32,
    capture_secs: i64,
    capture_offset_min: i16,
    camera_ref: u32,
    lens_ref: u32,
    gps_lat_bits: u32,
    gps_lon_bits: u32,
    signature_ref: u32,
}

#[derive(Default)]
struct StringTableBuilder {
    bytes: Vec<u8>,
    offsets: HashMap<String, u32>,
}

impl StringTableBuilder {
    fn intern(&mut self, value: &str) -> Result<u32, SegmentError> {
        if let Some(offset) = self.offsets.get(value) {
            return Ok(*offset);
        }
        let offset = u32::try_from(self.bytes.len()).map_err(|_| SegmentError::TooLarge)?;
        self.bytes.extend_from_slice(value.as_bytes());
        self.bytes.push(0);
        self.offsets.insert(value.to_string(), offset);
        Ok(offset)
    }

    fn intern_optional(&mut self, value: Option<&str>) -> Result<u32, SegmentError> {
        match value {
            Some(value) => self.intern(value),
            None => Ok(u32::MAX),
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

struct StringTable<'a> {
    bytes: &'a [u8],
}

impl<'a> StringTable<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, SegmentError> {
        if !bytes.is_empty() && *bytes.last().unwrap_or(&0) != 0 {
            return Err(SegmentError::Corrupt("string table is not nul terminated"));
        }
        Ok(Self { bytes })
    }

    fn get(&self, offset: u32) -> Result<&'a str, SegmentError> {
        if offset == u32::MAX {
            return Err(SegmentError::Corrupt(
                "required string reference is missing",
            ));
        }
        let start = offset as usize;
        if start >= self.bytes.len() {
            return Err(SegmentError::Corrupt("string reference is out of range"));
        }
        let end = self.bytes[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|relative| start + relative)
            .ok_or(SegmentError::Corrupt("string is not terminated"))?;
        std::str::from_utf8(&self.bytes[start..end])
            .map_err(|_| SegmentError::Corrupt("string is not utf-8"))
    }

    fn get_optional(&self, offset: u32) -> Result<Option<&'a str>, SegmentError> {
        if offset == u32::MAX {
            Ok(None)
        } else {
            self.get(offset).map(Some)
        }
    }
}

fn record_flags(record: &SegmentRecord) -> u32 {
    let mut flags = 0;
    if record.content_signature.is_some() {
        flags |= FLAG_HAS_CONTENT_SIGNATURE;
    }
    if record.gps_lat.is_some() && record.gps_lon.is_some() {
        flags |= FLAG_HAS_GPS;
    }
    flags
}

fn encode_header(dst: &mut [u8], header: Header) {
    dst[..8].copy_from_slice(MAGIC);
    put_u32(dst, 8, VERSION);
    put_u32(dst, 12, header.record_count);
    put_u32(dst, 16, header.record_bytes);
    put_u32(dst, 20, header.string_bytes);
    put_u32(dst, 24, header.signature_bytes);
    put_u64(dst, 28, header.checksum);
}

fn decode_header(bytes: &[u8]) -> Result<Header, SegmentError> {
    if &bytes[..8] != MAGIC {
        return Err(SegmentError::Corrupt("segment magic mismatch"));
    }
    if get_u32(bytes, 8)? != VERSION {
        return Err(SegmentError::UnsupportedVersion);
    }
    Ok(Header {
        record_count: get_u32(bytes, 12)?,
        record_bytes: get_u32(bytes, 16)?,
        string_bytes: get_u32(bytes, 20)?,
        signature_bytes: get_u32(bytes, 24)?,
        checksum: get_u64(bytes, 28)?,
    })
}

fn encode_record(dst: &mut Vec<u8>, record: EncodedRecord) {
    let start = dst.len();
    dst.resize(start + RECORD_BYTES, 0);
    let bytes = &mut dst[start..start + RECORD_BYTES];
    put_u32(bytes, 0, record.path_ref);
    put_u32(bytes, 4, record.name_ref);
    put_u64(bytes, 8, record.size);
    put_i64(bytes, 16, record.mtime_secs);
    bytes[24] = record.kind as u8;
    put_u32(bytes, 28, record.flags);
    put_u32(bytes, 32, record.width);
    put_u32(bytes, 36, record.height);
    put_i64(bytes, 40, record.capture_secs);
    put_i16(bytes, 48, record.capture_offset_min);
    put_u32(bytes, 52, record.camera_ref);
    put_u32(bytes, 56, record.lens_ref);
    put_u32(bytes, 60, record.gps_lat_bits);
    put_u32(bytes, 64, record.gps_lon_bits);
    put_u32(bytes, 68, record.signature_ref);
}

fn decode_record(bytes: &[u8]) -> Result<EncodedRecord, SegmentError> {
    let flags = get_u32(bytes, 28)?;
    let mut signature_ref = get_u32(bytes, 68)?;
    if flags & FLAG_HAS_CONTENT_SIGNATURE == 0 {
        signature_ref = u32::MAX;
    }
    let mut gps_lat_bits = get_u32(bytes, 60)?;
    let mut gps_lon_bits = get_u32(bytes, 64)?;
    if flags & FLAG_HAS_GPS == 0 {
        gps_lat_bits = f32::NAN.to_bits();
        gps_lon_bits = f32::NAN.to_bits();
    }
    Ok(EncodedRecord {
        path_ref: get_u32(bytes, 0)?,
        name_ref: get_u32(bytes, 4)?,
        size: get_u64(bytes, 8)?,
        mtime_secs: get_i64(bytes, 16)?,
        kind: IndexedKind::from_byte(bytes[24]),
        flags,
        width: get_u32(bytes, 32)?,
        height: get_u32(bytes, 36)?,
        capture_secs: get_i64(bytes, 40)?,
        capture_offset_min: get_i16(bytes, 48)?,
        camera_ref: get_u32(bytes, 52)?,
        lens_ref: get_u32(bytes, 56)?,
        gps_lat_bits,
        gps_lon_bits,
        signature_ref,
    })
}

fn optional_u32(value: u32) -> Option<u32> {
    (value != u32::MAX).then_some(value)
}

fn optional_i64(value: i64) -> Option<i64> {
    (value != i64::MIN).then_some(value)
}

fn optional_i16(value: i16) -> Option<i16> {
    (value != i16::MIN).then_some(value)
}

fn optional_f32(bits: u32) -> Option<f32> {
    let value = f32::from_bits(bits);
    (!value.is_nan()).then_some(value)
}

fn checked_mul(lhs: usize, rhs: usize) -> Result<usize, SegmentError> {
    lhs.checked_mul(rhs).ok_or(SegmentError::TooLarge)
}

fn checksum64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_i16(bytes: &mut [u8], offset: usize, value: i16) {
    put_u16(bytes, offset, value as u16);
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(bytes: &mut [u8], offset: usize, value: i64) {
    put_u64(bytes, offset, value as u64);
}

fn get_u16(bytes: &[u8], offset: usize) -> Result<u16, SegmentError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(SegmentError::Corrupt("u16 field out of range"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn get_i16(bytes: &[u8], offset: usize) -> Result<i16, SegmentError> {
    get_u16(bytes, offset).map(|value| value as i16)
}

fn get_u32(bytes: &[u8], offset: usize) -> Result<u32, SegmentError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(SegmentError::Corrupt("u32 field out of range"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn get_u64(bytes: &[u8], offset: usize) -> Result<u64, SegmentError> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or(SegmentError::Corrupt("u64 field out of range"))?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn get_i64(bytes: &[u8], offset: usize) -> Result<i64, SegmentError> {
    get_u64(bytes, offset).map(|value| value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;

    fn sample_record(index: usize) -> SegmentRecord {
        SegmentRecord {
            path: format!("/photos/2024/file-{index}.jpg"),
            name: format!("file-{index}.jpg"),
            size: 1024 + index as u64,
            mtime_secs: 1_700_000_000 + index as i64,
            kind: IndexedKind::Image,
            width: Some(4000),
            height: Some(3000),
            capture_secs: Some(1_700_010_000 + index as i64),
            capture_offset_min: Some(540),
            camera: Some("IRIS-CAM".to_string()),
            lens: Some("Prime 35".to_string()),
            gps_lat: Some(35.6812),
            gps_lon: Some(139.7671),
            content_signature: Some(BloomSignature::from_content("日本語 PDF metadata")),
        }
    }

    #[test]
    fn segment_round_trips_records_and_signatures() {
        let segment = Segment::new(vec![sample_record(1), sample_record(2)]);
        let bytes = segment.to_bytes().expect("encode segment");
        assert!(bytes.len() < 4096);
        let restored = Segment::from_bytes(&bytes).expect("decode segment");
        assert_eq!(restored, segment);
        assert!(restored.records()[0]
            .content_signature
            .as_ref()
            .unwrap()
            .might_contain_query("日本語"));
    }

    #[test]
    fn checksum_rejects_corrupt_segments() {
        let segment = Segment::new(vec![sample_record(1)]);
        let mut bytes = segment.to_bytes().expect("encode segment");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert_eq!(
            Segment::from_bytes(&bytes),
            Err(SegmentError::ChecksumMismatch)
        );
    }

    #[test]
    fn write_atomic_round_trips_from_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("photos.idx");
        let segment = Segment::new(vec![sample_record(1)]);
        segment.write_atomic(&path).expect("write segment");
        let restored = Segment::read(&path).expect("read segment");
        assert_eq!(restored, segment);
    }

    #[test]
    fn read_cache_returns_none_for_missing_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.idx");

        assert_eq!(Segment::read_cache(&path).expect("read cache"), None);
    }

    #[test]
    fn read_cache_round_trips_valid_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("photos.idx");
        let segment = Segment::new(vec![sample_record(1)]);
        segment.write_atomic(&path).expect("write segment");

        assert_eq!(
            Segment::read_cache(&path).expect("read cache"),
            Some(segment)
        );
        assert!(path.exists());
    }

    #[test]
    fn read_cache_discards_corrupt_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("photos.idx");
        let segment = Segment::new(vec![sample_record(1)]);
        let mut bytes = segment.to_bytes().expect("encode segment");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        fs::write(&path, bytes).expect("write corrupt segment");

        assert_eq!(Segment::read_cache(&path).expect("read cache"), None);
        assert!(!path.exists());
    }

    proptest! {
        #[test]
        fn proptest_segment_round_trip(names in prop::collection::vec("[a-z0-9]{1,16}", 1..64)) {
            let records = names
                .iter()
                .enumerate()
                .map(|(index, name)| SegmentRecord {
                    path: format!("/root/{name}-{index}.txt"),
                    name: format!("{name}-{index}.txt"),
                    size: index as u64,
                    mtime_secs: 1_700_000_000 + index as i64,
                    kind: IndexedKind::File,
                    width: None,
                    height: None,
                    capture_secs: None,
                    capture_offset_min: None,
                    camera: None,
                    lens: None,
                    gps_lat: None,
                    gps_lon: None,
                    content_signature: Some(BloomSignature::from_content(name)),
                })
                .collect::<Vec<_>>();
            let segment = Segment::new(records);
            let restored = Segment::from_bytes(&segment.to_bytes().expect("encode")).expect("decode");
            prop_assert_eq!(restored, segment);
        }
    }
}
