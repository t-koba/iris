use super::*;

#[cfg(feature = "raw-full")]
pub(super) fn extract_raf_embedded_jpeg(
    source: &impl BlockingBinaryReader,
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
    if source.len() < 92 {
        return None;
    }

    let header = source.read_range(0, 92)?;
    let jpeg_offset = read_u32_be(&header[84..88])? as usize;
    let jpeg_len = read_u32_be(&header[88..92])? as usize;
    choose_best_candidate(
        candidate_from_range(source, jpeg_offset as u64, jpeg_len)
            .into_iter()
            .collect(),
        max_dimension,
    )
}

#[cfg(feature = "raw-full")]
struct X3fImage {
    payload_offset: u64,
    payload_len: usize,
    width: u32,
    height: u32,
}

/// Image entries of the X3F directory whose dimensions could be read.
#[cfg(feature = "raw-full")]
fn x3f_images(source: &impl BlockingBinaryReader) -> Option<Vec<X3fImage>> {
    if source.len() < 8 || source.read_range(0, 4)?.as_slice() != X3F_SIGNATURE {
        return None;
    }
    let directory_offset = read_u32_le(&source.read_range(source.len() - 4, 4)?)? as u64;
    if directory_offset + 12 > source.len() {
        return None;
    }
    let directory_header = source.read_range(directory_offset, 12)?;
    if read_u32_le(&directory_header[4..8])? < X3F_DIRECTORY_MIN_VERSION {
        return None;
    }
    let entry_count = read_u32_le(&directory_header[8..12])? as u64;

    let images = (0..entry_count)
        .filter_map(|idx| {
            let entry = source.read_range(directory_offset + 12 + idx * 12, 12)?;
            let image_offset = read_u32_le(&entry[0..4])? as u64;
            let image_len = read_u32_le(&entry[4..8])? as usize;
            if &entry[8..12] != X3F_IMAGE_DIRECTORY_ID || image_len <= X3F_IMAGE_HEADER_BYTES {
                return None;
            }
            let image_header = source.read_range(image_offset, X3F_IMAGE_HEADER_BYTES)?;
            let width = read_u32_le(&image_header[16..20]).unwrap_or(0);
            let height = read_u32_le(&image_header[20..24]).unwrap_or(0);
            (width != 0 && height != 0).then_some(X3fImage {
                payload_offset: image_offset + X3F_IMAGE_HEADER_BYTES as u64,
                payload_len: image_len - X3F_IMAGE_HEADER_BYTES,
                width,
                height,
            })
        })
        .collect();
    Some(images)
}

#[cfg(feature = "raw-full")]
pub(super) fn extract_x3f_embedded_jpeg(
    source: &impl BlockingBinaryReader,
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
    let candidates = x3f_images(source)?
        .into_iter()
        .filter_map(|image| candidate_from_range(source, image.payload_offset, image.payload_len))
        .collect();
    choose_best_candidate(candidates, max_dimension)
}

#[cfg(feature = "raw-full")]
pub(super) fn extract_x3f_primary_dimensions(
    source: &impl BlockingBinaryReader,
) -> Option<(u32, u32)> {
    let candidates = x3f_images(source)?
        .into_iter()
        .map(|image| PrimaryImageDimensionsCandidate {
            width: image.width,
            height: image.height,
            reduced_resolution: false,
        })
        .collect();
    choose_primary_dimensions(candidates)
}

pub(super) fn extract_cr3_embedded_jpeg(
    source: &impl BlockingBinaryReader,
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
    let file_len = source.len();
    let moov = find_box_in_range(source, 0, file_len, BOX_TYPE_MOOV)?;
    let tracks = child_boxes(source, moov, BOX_TYPE_TRAK);
    if tracks.is_empty() {
        return None;
    }

    let mut candidates = Vec::new();
    for track in tracks {
        let mdia = match find_box_in_range(source, track.data_start(), track.end(), BOX_TYPE_MDIA) {
            Some(v) => v,
            None => continue,
        };
        let minf = match find_box_in_range(source, mdia.data_start(), mdia.end(), BOX_TYPE_MINF) {
            Some(v) => v,
            None => continue,
        };
        let stbl = match find_box_in_range(source, minf.data_start(), minf.end(), BOX_TYPE_STBL) {
            Some(v) => v,
            None => continue,
        };

        for sample in stbl_sample_ranges(source, stbl) {
            if let Some(candidate) = candidate_from_range(source, sample.0, sample.1) {
                candidates.push(candidate);
            }
        }
    }

    choose_best_candidate(candidates, max_dimension)
}
