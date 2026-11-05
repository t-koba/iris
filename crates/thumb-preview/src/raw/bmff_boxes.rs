use super::*;

pub(super) fn read_box_header(
    source: &impl BlockingBinaryReader,
    offset: u64,
    end: u64,
) -> Option<FileBoxHeader> {
    if offset.checked_add(8)? > end || end > source.len() {
        return None;
    }

    let header = source.read_range(offset, 8)?;
    let mut size = read_u32_be(&header[0..4])? as u64;
    let mut header_len = 8u64;
    let typ = [header[4], header[5], header[6], header[7]];

    if size == 1 {
        let extended = source.read_range(offset + 8, 8)?;
        size = read_u64_be(&extended)?;
        header_len = 16;
    } else if size == 0 {
        size = end - offset;
    }

    if size < header_len || offset.checked_add(size)? > end {
        return None;
    }

    Some(FileBoxHeader {
        start: offset,
        size,
        header_len,
        typ,
    })
}

pub(super) fn find_box_in_range(
    source: &impl BlockingBinaryReader,
    start: u64,
    end: u64,
    desired_type: [u8; 4],
) -> Option<FileBoxHeader> {
    let mut offset = start;
    while offset + 8 <= end {
        let header = read_box_header(source, offset, end)?;
        if header.typ == desired_type {
            return Some(header);
        }
        if header.size == 0 {
            break;
        }
        offset = header.end();
    }
    None
}

pub(super) fn child_boxes(
    source: &impl BlockingBinaryReader,
    parent: FileBoxHeader,
    desired_type: [u8; 4],
) -> Vec<FileBoxHeader> {
    let mut boxes = Vec::new();
    let mut offset = parent.data_start();
    while offset + 8 <= parent.end() {
        let Some(header) = read_box_header(source, offset, parent.end()) else {
            break;
        };
        if header.typ == desired_type {
            boxes.push(header);
        }
        if header.size == 0 {
            break;
        }
        offset = header.end();
    }
    boxes
}
