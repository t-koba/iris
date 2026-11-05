use super::*;

pub(super) fn choose_best_candidate(
    mut candidates: Vec<StructuredJpegCandidate>,
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
    if candidates.is_empty() {
        return None;
    }

    let desired = max_dimension.max(1);
    candidates.sort_by(|a, b| {
        let a_sufficient = a.longest_edge() >= desired;
        let b_sufficient = b.longest_edge() >= desired;
        match (a_sufficient, b_sufficient) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => a
                .longest_edge()
                .cmp(&b.longest_edge())
                .then(a.byte_len.cmp(&b.byte_len)),
            (false, false) => b
                .longest_edge()
                .cmp(&a.longest_edge())
                .then(b.byte_len.cmp(&a.byte_len)),
        }
    });

    candidates.into_iter().next()
}

pub(super) fn choose_primary_dimensions(
    mut candidates: Vec<PrimaryImageDimensionsCandidate>,
) -> Option<(u32, u32)> {
    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|a, b| match (a.reduced_resolution, b.reduced_resolution) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => b
            .area()
            .cmp(&a.area())
            .then(b.width.max(b.height).cmp(&a.width.max(a.height))),
    });

    candidates
        .first()
        .map(|candidate| (candidate.width, candidate.height))
}

pub(super) fn is_reduced_resolution_ifd(
    new_subfile_type: Option<u32>,
    subfile_type: Option<u32>,
) -> bool {
    new_subfile_type.is_some_and(|value| value & 0x1 != 0) || subfile_type == Some(2)
}

pub(super) fn extract_filtered_exif_metadata(
    file_path: &Path,
) -> (Option<u32>, Option<u32>, Vec<(String, String)>) {
    let Ok(file) = std::fs::File::open(file_path) else {
        return (None, None, Vec::new());
    };
    let mut reader = std::io::BufReader::new(file);
    extract_filtered_exif_metadata_from_reader(&mut reader)
}

pub(super) fn extract_filtered_exif_metadata_from_reader<R: std::io::BufRead + Seek>(
    reader: &mut R,
) -> (Option<u32>, Option<u32>, Vec<(String, String)>) {
    let Ok(exif_data) = exif::Reader::new().read_from_container(reader) else {
        return (None, None, Vec::new());
    };

    let mut width = None;
    let mut height = None;
    for field in exif_data.fields() {
        if field.tag == exif::Tag::PixelXDimension {
            width = field.value.get_uint(0);
        } else if field.tag == exif::Tag::PixelYDimension {
            height = field.value.get_uint(0);
        } else if field.tag == exif::Tag::ImageWidth && width.is_none() {
            width = field.value.get_uint(0);
        } else if field.tag == exif::Tag::ImageLength && height.is_none() {
            height = field.value.get_uint(0);
        }
    }

    let mut entries = Vec::new();
    let mut seen_tags = HashSet::new();
    for field in exif_data.fields() {
        let tag_name = format!("{}", field.tag);
        let value = field.display_value().with_unit(&exif_data).to_string();
        if value.len() < 500
            && !is_effectively_blank_exif_value(&value)
            && !tag_name.contains("Unknown")
            && !tag_name.starts_with("Tag(")
            && !should_skip_raw_exif_tag(&tag_name, field.tag)
            && seen_tags.insert(tag_name.clone())
        {
            entries.push((tag_name, value));
        }
    }

    (width, height, entries)
}

pub(super) fn panasonic_image_dimension(start: Option<u32>, end: Option<u32>) -> Option<u32> {
    match (start, end) {
        (Some(start), Some(end)) if end > start => Some(end - start),
        _ => None,
    }
}

pub(super) fn is_effectively_blank_exif_value(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_whitespace() || matches!(ch, '"' | ',' | '\0'))
}

pub(super) fn should_skip_raw_exif_tag(tag_name: &str, tag: exif::Tag) -> bool {
    matches!(
        tag,
        exif::Tag::PixelXDimension
            | exif::Tag::PixelYDimension
            | exif::Tag::ImageWidth
            | exif::Tag::ImageLength
            | exif::Tag::Compression
            | exif::Tag::JPEGInterchangeFormat
            | exif::Tag::JPEGInterchangeFormatLength
            | exif::Tag::StripOffsets
            | exif::Tag::StripByteCounts
            | exif::Tag::XResolution
            | exif::Tag::YResolution
            | exif::Tag::ResolutionUnit
            | exif::Tag::WhitePoint
            | exif::Tag::PrimaryChromaticities
            | exif::Tag::YCbCrCoefficients
            | exif::Tag::YCbCrPositioning
    ) || matches!(
        tag_name,
        "Bits Per Sample"
            | "Compressed Bits Per Pixel"
            | "Components Configuration"
            | "Flashpix Version"
            | "Color Space"
            | "Thumbnail Offset"
            | "Thumbnail Length"
            | "Preview Image Start"
            | "Preview Image Length"
            | "Preview Image Size"
            | "Encoding Process"
            | "Color Components"
            | "Y Cb Cr Sub Sampling"
            | "Subfile Type"
    )
}

pub(super) fn candidate_from_range(
    source: &impl BlockingBinaryReader,
    start: u64,
    len: usize,
) -> Option<StructuredJpegCandidate> {
    if len < MIN_JPEG_BYTES || start.checked_add(len as u64)? > source.len() {
        return None;
    }

    let probe_len = len.min(JPEG_PROBE_BYTES);
    let probe = source.read_range(start, probe_len)?;
    let (width, height) = match crate::jpeg::jpeg_dimensions(&probe) {
        Some(dimensions) => dimensions,
        None if probe_len < len => {
            let full = source.read_range(start, len)?;
            crate::jpeg::jpeg_dimensions(&full)?
        }
        None => return None,
    };

    Some(StructuredJpegCandidate {
        source: StructuredCandidateSource::FileRange { start, len },
        width,
        height,
        byte_len: len,
    })
}

/// A JPEG stored as strips or tiles: read in place when contiguous, otherwise assembled.
pub(super) fn candidate_from_segments(
    source: &impl BlockingBinaryReader,
    offsets: &[u64],
    counts: &[u64],
) -> Option<StructuredJpegCandidate> {
    if offsets.is_empty() || offsets.len() != counts.len() {
        return None;
    }
    let mut segments = Vec::with_capacity(offsets.len());
    let mut total = 0usize;
    let mut contiguous = true;
    for (&offset, &count) in offsets.iter().zip(counts) {
        let len = usize::try_from(count).ok().filter(|len| *len > 0)?;
        if offset.checked_add(count)? > source.len() {
            return None;
        }
        contiguous &= offset == offsets[0] + total as u64;
        total = total.checked_add(len)?;
        segments.push((offset, len));
    }
    if contiguous {
        return candidate_from_range(source, offsets[0], total);
    }
    let mut bytes = Vec::with_capacity(total);
    for (offset, len) in segments {
        bytes.extend_from_slice(&source.read_range(offset, len)?);
    }
    candidate_from_bytes(bytes)
}

pub(super) fn candidate_from_bytes(bytes: Vec<u8>) -> Option<StructuredJpegCandidate> {
    if bytes.len() < MIN_JPEG_BYTES {
        return None;
    }
    let (width, height) = crate::jpeg::jpeg_dimensions(&bytes)?;
    Some(StructuredJpegCandidate {
        byte_len: bytes.len(),
        source: StructuredCandidateSource::InlineBytes(bytes),
        width,
        height,
    })
}
