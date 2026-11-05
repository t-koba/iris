use image::ImageReader;
use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailResult, ThumbnailSource};
use std::io::Cursor;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedImageInfo {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

pub fn probe_encoded_image_bytes(data: &[u8]) -> Option<EncodedImageInfo> {
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let format = reader.format().and_then(domain_thumbnail_format)?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(EncodedImageInfo {
        format,
        width,
        height,
    })
}

pub fn probe_encoded_image_path(path: &Path) -> Option<EncodedImageInfo> {
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let format = reader.format().and_then(domain_thumbnail_format)?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(EncodedImageInfo {
        format,
        width,
        height,
    })
}

pub fn can_passthrough_encoded_image(
    info: &EncodedImageInfo,
    max_dimension: u32,
    preferred_formats: &[ImageFormat],
) -> bool {
    info.width <= max_dimension
        && info.height <= max_dimension
        && (preferred_formats.is_empty() || preferred_formats.contains(&info.format))
}

pub fn passthrough_encoded_image(
    data: Vec<u8>,
    info: EncodedImageInfo,
    source: ThumbnailSource,
) -> ThumbnailResult {
    ThumbnailResult {
        data,
        format: info.format,
        width: info.width,
        height: info.height,
        source,
    }
}

fn domain_thumbnail_format(format: image::ImageFormat) -> Option<ImageFormat> {
    match format {
        image::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
        image::ImageFormat::Png => Some(ImageFormat::Png),
        image::ImageFormat::WebP => Some(ImageFormat::WebP),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_png_dimensions_from_bytes() {
        let image = image::DynamicImage::new_rgba8(7, 5);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode png");

        let probed = probe_encoded_image_bytes(&bytes).expect("probe png bytes");
        assert_eq!(probed.format, ImageFormat::Png);
        assert_eq!(probed.width, 7);
        assert_eq!(probed.height, 5);
    }

    #[test]
    fn passthrough_accepts_preferred_small_image() {
        let info = EncodedImageInfo {
            format: ImageFormat::WebP,
            width: 64,
            height: 48,
        };
        assert!(can_passthrough_encoded_image(
            &info,
            128,
            &[ImageFormat::Jpeg, ImageFormat::WebP]
        ));
        assert!(!can_passthrough_encoded_image(
            &info,
            32,
            &[ImageFormat::Jpeg, ImageFormat::WebP]
        ));
        assert!(!can_passthrough_encoded_image(
            &info,
            128,
            &[ImageFormat::Jpeg]
        ));
    }
}
