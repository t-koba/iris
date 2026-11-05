use std::time::SystemTime;

/// How much metadata has been fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetadataLevel {
    /// Listing result (first paint).
    L0,
    /// stat result: size, mtime, kind.
    L1,
    /// Basic metadata: basic EXIF, orientation, attributes.
    L2,
    /// Deep analysis: embedded preview, RAW header, extras.
    L3,
}

#[derive(Clone, Debug)]
pub struct MetadataResult {
    pub level: MetadataLevel,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
}

/// Media metadata used by filter/index pipelines.
///
/// This is intentionally typed and compact. Display-oriented EXIF rows can
/// continue to live next to it, but filter predicates should depend on this
/// structure instead of parsing localized strings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub capture_naive_secs: Option<i64>,
    pub capture_offset_min: Option<i16>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
}
