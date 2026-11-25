//! Generate the deterministic 256x256 RGB JP2 used by the paired-source smoke.

use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
const EXPECTED_BYTES: usize = 393;
const EXPECTED_SHA256: &str = "e44d3480025d87433287c64775c0486cc2e6f7369c6df5b8a249d89f6d2a2caa";
const COMMENT: &str = "Iris paired J2K fixture v1";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("expected JP2 output path")?,
    );
    let rgb = image::RgbImage::from_fn(WIDTH, HEIGHT, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let parameters = jpeg2k::EncodeParameters::new().comment(COMMENT);
    let bytes = jpeg2k::Image::from_dynamic_image(&image::DynamicImage::ImageRgb8(rgb))
        .encode_jp2(parameters)?;
    assert_eq!(bytes.len(), EXPECTED_BYTES, "JP2 byte length pin mismatch");
    let digest = hex(&Sha256::digest(&bytes));
    assert_eq!(digest, EXPECTED_SHA256, "JP2 SHA-256 pin mismatch");
    // Never overwrite an existing fixture: the caller owns a fresh temporary directory.
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    output.write_all(&bytes)?;
    println!("generated {EXPECTED_BYTES}-byte JP2 with SHA-256 {digest}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_hex_is_lowercase_and_complete() {
        assert_eq!(hex(&[0, 1, 15, 16, 255]), "00010f10ff");
    }
}
