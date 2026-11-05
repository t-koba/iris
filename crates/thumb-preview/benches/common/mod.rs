#![allow(dead_code)]

use image::ImageFormat;
use iris_thumb_preview::raw;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const TIFF_MAGIC_STANDARD: u16 = 42;
const TIFF_MAGIC_BIGTIFF: u16 = 43;
const BIGTIFF_OFFSET_SIZE: u16 = 8;
const BOX_TYPE_MOOV: [u8; 4] = *b"moov";
const BOX_TYPE_TRAK: [u8; 4] = *b"trak";
const BOX_TYPE_MDIA: [u8; 4] = *b"mdia";
const BOX_TYPE_MINF: [u8; 4] = *b"minf";
const BOX_TYPE_STBL: [u8; 4] = *b"stbl";
const BOX_TYPE_STSC: [u8; 4] = *b"stsc";
const BOX_TYPE_STCO: [u8; 4] = *b"stco";
const BOX_TYPE_STSZ: [u8; 4] = *b"stsz";
const TAG_JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;
const TIFF_TYPE_LONG: u16 = 4;
const TIFF_TYPE_LONG8: u16 = 16;

pub fn encoded_test_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut jpeg = Vec::new();
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
        .expect("jpeg encode");
    jpeg
}

pub fn unique_temp_path(ext: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("iris-thumb-preview-bench-{nanos}.{ext}"))
}

pub fn write_fixture(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write fixture");
}

pub fn bench_extract_raw_thumbnail(path: &Path, max_dimension: u32) {
    let result = raw::extract_raw_thumbnail(path, max_dimension).expect("raw thumbnail");
    std::hint::black_box(result);
}

pub fn bigtiff_with_embedded_jpeg(jpeg_offset: u64, jpeg: &[u8]) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&TIFF_MAGIC_BIGTIFF.to_le_bytes());
    tiff.extend_from_slice(&BIGTIFF_OFFSET_SIZE.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&16u64.to_le_bytes());

    tiff.extend_from_slice(&2u64.to_le_bytes());

    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG8.to_le_bytes());
    tiff.extend_from_slice(&1u64.to_le_bytes());
    tiff.extend_from_slice(&jpeg_offset.to_le_bytes());

    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT_LENGTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG8.to_le_bytes());
    tiff.extend_from_slice(&1u64.to_le_bytes());
    tiff.extend_from_slice(&(jpeg.len() as u64).to_le_bytes());

    tiff.extend_from_slice(&0u64.to_le_bytes());
    if tiff.len() < jpeg_offset as usize {
        tiff.resize(jpeg_offset as usize, 0);
    }
    tiff.extend_from_slice(jpeg);
    tiff
}

fn bmff_box(typ: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    bytes.extend_from_slice(&typ);
    bytes.extend_from_slice(&payload);
    bytes
}

pub fn synthetic_cr3_with_second_sample_preview(jpeg: &[u8]) -> Vec<u8> {
    let invalid_sample = vec![0u8; 320];

    let stsc = bmff_box(
        BOX_TYPE_STSC,
        [
            0u32.to_be_bytes().to_vec(),
            1u32.to_be_bytes().to_vec(),
            1u32.to_be_bytes().to_vec(),
            2u32.to_be_bytes().to_vec(),
            1u32.to_be_bytes().to_vec(),
        ]
        .concat(),
    );
    let stsz = bmff_box(
        BOX_TYPE_STSZ,
        [
            0u32.to_be_bytes().to_vec(),
            0u32.to_be_bytes().to_vec(),
            2u32.to_be_bytes().to_vec(),
            (invalid_sample.len() as u32).to_be_bytes().to_vec(),
            (jpeg.len() as u32).to_be_bytes().to_vec(),
        ]
        .concat(),
    );

    let stco_with_offset = |chunk_offset: u32| {
        bmff_box(
            BOX_TYPE_STCO,
            [
                0u32.to_be_bytes().to_vec(),
                1u32.to_be_bytes().to_vec(),
                chunk_offset.to_be_bytes().to_vec(),
            ]
            .concat(),
        )
    };

    let build_moov = |stco: Vec<u8>| {
        bmff_box(
            BOX_TYPE_MOOV,
            bmff_box(
                BOX_TYPE_TRAK,
                bmff_box(
                    BOX_TYPE_MDIA,
                    bmff_box(
                        BOX_TYPE_MINF,
                        bmff_box(BOX_TYPE_STBL, [stsc.clone(), stsz.clone(), stco].concat()),
                    ),
                ),
            ),
        )
    };

    let moov = build_moov(stco_with_offset(0));
    let chunk_offset = moov.len() as u32;
    let moov = build_moov(stco_with_offset(chunk_offset));

    [moov, invalid_sample, jpeg.to_vec()].concat()
}

pub fn structured_tiff_with_embedded_jpeg(jpeg_offset: u32, jpeg: &[u8]) -> Vec<u8> {
    let jpeg_len = jpeg.len() as u32;
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&TIFF_MAGIC_STANDARD.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_offset.to_le_bytes());
    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT_LENGTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_len.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    if tiff.len() < jpeg_offset as usize {
        tiff.resize(jpeg_offset as usize, 0);
    }
    tiff.extend_from_slice(jpeg);
    tiff
}
