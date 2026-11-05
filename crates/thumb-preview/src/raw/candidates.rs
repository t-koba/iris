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

pub(super) fn panasonic_image_dimension(start: Option<u32>, end: Option<u32>) -> Option<u32> {
    match (start, end) {
        (Some(start), Some(end)) if end > start => Some(end - start),
        _ => None,
    }
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
