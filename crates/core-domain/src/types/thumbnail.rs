#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    WebP,
    Png,
}

#[derive(Clone, Debug)]
pub struct ThumbnailHint {
    /// Requested size of the longer edge, in pixels.
    pub max_dimension: u32,
    /// Accepted formats in order of preference.
    pub preferred_formats: Vec<ImageFormat>,
}

/// Where a thumbnail came from; drives progressive display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbnailSource {
    DirectApi,
    OriginalEncoded,
    EmbeddedPreview,
    PartialDecode,
    FullDecode,
}

#[derive(Clone, Debug)]
pub struct ThumbnailResult {
    pub data: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub source: ThumbnailSource,
}
