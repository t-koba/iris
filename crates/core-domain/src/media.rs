//! File formats recognised by extension, shared by thumbnails, previews and search.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaFormat {
    Jpeg,
    /// Formats the `image` crate decodes quickly.
    Image,
    /// AVIF, HEIF, JPEG 2000, JPEG XR and JPEG XL, decoded by dedicated codecs.
    ModernImage,
    Raw,
    Video,
    Pdf,
}

impl MediaFormat {
    /// Format for a file extension, ignoring case.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_ascii_lowercase();
        Some(match ext.as_str() {
            "jpg" | "jpeg" => Self::Jpeg,
            "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" => Self::Image,
            "avif" | "heic" | "heif" | "hif" | "jp2" | "j2k" | "j2c" | "jpc" | "jpf" | "jpx"
            | "jpm" | "jxr" | "hdp" | "wdp" | "jxl" => Self::ModernImage,
            "arw" | "cr2" | "cr3" | "dng" | "nef" | "nrw" | "orf" | "raf" | "rw2" | "x3f" => {
                Self::Raw
            }
            "mp4" | "mov" | "m4v" | "mkv" | "avi" | "webm" | "mpg" | "mpeg" | "mj2" => Self::Video,
            "pdf" => Self::Pdf,
            _ => return None,
        })
    }

    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        Self::from_extension(path.as_ref().extension()?.to_str()?)
    }

    /// Still images, RAW included.
    pub fn is_image(self) -> bool {
        matches!(
            self,
            Self::Jpeg | Self::Image | Self::ModernImage | Self::Raw
        )
    }
}
