//! TIFF / BigTIFF directory reader shared by the preview, dimension and metadata extractors.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct Tiff {
    endian: Endian,
    big: bool,
    first_ifd: u64,
}

pub(super) struct IfdEntry {
    pub tag: u16,
    field_type: u16,
    count: u64,
    /// The raw value field: the value itself when it fits, otherwise its offset.
    value: [u8; 8],
}

impl Tiff {
    /// Parses a standard, Panasonic RW2 or BigTIFF header.
    pub(super) fn parse(source: &impl BlockingBinaryReader) -> Option<Self> {
        if source.len() < 8 {
            return None;
        }
        let header = source.read_range(0, source.len().min(16) as usize)?;
        let endian = parse_tiff_endian(&header)?;
        let (big, first_ifd) = match read_u16_endian(&header[2..4], endian)? {
            TIFF_MAGIC_STANDARD | TIFF_MAGIC_RW2 => {
                (false, u64::from(read_u32_endian(&header[4..8], endian)?))
            }
            TIFF_MAGIC_BIGTIFF => {
                if header.len() < 16
                    || read_u16_endian(&header[4..6], endian)? != BIGTIFF_OFFSET_SIZE
                    || read_u16_endian(&header[6..8], endian)? != 0
                {
                    return None;
                }
                (true, read_u64_endian(&header[8..16], endian)?)
            }
            _ => return None,
        };
        (first_ifd != 0 && first_ifd < source.len()).then_some(Self {
            endian,
            big,
            first_ifd,
        })
    }

    /// Size of the entry count, of an offset and of the inline value field.
    fn word(self) -> usize {
        if self.big {
            8
        } else {
            4
        }
    }

    fn read_word(self, bytes: &[u8]) -> Option<u64> {
        if self.big {
            read_u64_endian(bytes, self.endian)
        } else {
            read_u32_endian(bytes, self.endian).map(u64::from)
        }
    }

    /// Entries of the IFD at `offset` and the offset of the next IFD.
    fn read_ifd(
        self,
        source: &impl BlockingBinaryReader,
        offset: u64,
    ) -> Option<(Vec<IfdEntry>, u64)> {
        let (count_len, entry_len) = if self.big {
            (8, BIGTIFF_ENTRY_BYTES)
        } else {
            (2, 12)
        };
        if offset.checked_add(count_len as u64)? > source.len() {
            return None;
        }
        let count_bytes = source.read_range(offset, count_len)?;
        let entry_count = if self.big {
            usize::try_from(read_u64_endian(&count_bytes, self.endian)?).ok()?
        } else {
            usize::from(read_u16_endian(&count_bytes, self.endian)?)
        };
        if entry_count == 0 || entry_count > MAX_IFD_ENTRIES {
            return None;
        }
        let table_len = count_len + entry_count * entry_len + self.word();
        if offset.checked_add(table_len as u64)? > source.len() {
            return None;
        }
        let table = source.read_range(offset, table_len)?;
        let word = self.word();
        let entries = table[count_len..count_len + entry_count * entry_len]
            .chunks_exact(entry_len)
            .filter_map(|entry| {
                let count = if self.big {
                    read_u64_endian(&entry[4..12], self.endian)?
                } else {
                    u64::from(read_u32_endian(&entry[4..8], self.endian)?)
                };
                let mut value = [0; 8];
                value[..word].copy_from_slice(&entry[entry_len - word..]);
                Some(IfdEntry {
                    tag: read_u16_endian(&entry[0..2], self.endian)?,
                    field_type: read_u16_endian(&entry[2..4], self.endian)?,
                    count,
                    value,
                })
            })
            .collect();
        let next = self.read_word(&table[table_len - word..])?;
        Some((entries, next))
    }

    /// Visits every IFD reachable through next-IFD links and SubIFDs (and the EXIF IFD when
    /// `follow_exif` is set), each once, up to `MAX_IFD_VISITS`.
    pub(super) fn walk(
        self,
        source: &impl BlockingBinaryReader,
        follow_exif: bool,
        mut visit: impl FnMut(&[IfdEntry]),
    ) {
        let mut stack = vec![self.first_ifd];
        let mut visited = HashSet::new();
        while let Some(offset) = stack.pop() {
            if offset == 0 || !visited.insert(offset) {
                continue;
            }
            if visited.len() > MAX_IFD_VISITS {
                break;
            }
            let Some((entries, next)) = self.read_ifd(source, offset) else {
                continue;
            };
            for entry in &entries {
                match entry.tag {
                    TAG_SUB_IFDS => stack.extend(self.values(source, entry)),
                    TAG_EXIF_IFD_POINTER if follow_exif => {
                        stack.extend(self.first_value(source, entry));
                    }
                    _ => {}
                }
            }
            visit(&entries);
            stack.push(next);
        }
    }

    /// The entries of the first IFD only.
    pub(super) fn root_entries(self, source: &impl BlockingBinaryReader) -> Vec<IfdEntry> {
        self.read_ifd(source, self.first_ifd)
            .map(|(entries, _)| entries)
            .unwrap_or_default()
    }

    fn value_offset(self, entry: &IfdEntry) -> Option<u64> {
        self.read_word(&entry.value)
    }

    fn byte_len(entry: &IfdEntry) -> Option<usize> {
        usize::try_from(entry.count)
            .ok()?
            .checked_mul(tiff_type_size(entry.field_type)?)
    }

    /// The raw bytes of `entry`'s value, inline or at its offset.
    fn bytes(self, source: &impl BlockingBinaryReader, entry: &IfdEntry) -> Option<Vec<u8>> {
        let len = Self::byte_len(entry)?;
        if len == 0 {
            return None;
        }
        if len <= self.word() {
            return Some(entry.value[..len].to_vec());
        }
        source.read_range(self.value_offset(entry)?, len)
    }

    fn unsigned_size(field_type: u16) -> Option<usize> {
        match field_type {
            TIFF_TYPE_SHORT => Some(2),
            TIFF_TYPE_LONG | TIFF_TYPE_IFD => Some(4),
            TIFF_TYPE_LONG8 | TIFF_TYPE_IFD8 => Some(8),
            _ => None,
        }
    }

    fn read_unsigned(self, bytes: &[u8]) -> Option<u64> {
        match bytes.len() {
            2 => read_u16_endian(bytes, self.endian).map(u64::from),
            4 => read_u32_endian(bytes, self.endian).map(u64::from),
            8 => read_u64_endian(bytes, self.endian),
            _ => None,
        }
    }

    /// Every value of an unsigned integer tag, up to `MAX_TAG_ARRAY_COUNT`.
    pub(super) fn values(self, source: &impl BlockingBinaryReader, entry: &IfdEntry) -> Vec<u64> {
        let Some(unit) = Self::unsigned_size(entry.field_type) else {
            return Vec::new();
        };
        if entry.count as usize > MAX_TAG_ARRAY_COUNT {
            return Vec::new();
        }
        self.bytes(source, entry)
            .map(|bytes| {
                bytes
                    .chunks_exact(unit)
                    .filter_map(|chunk| self.read_unsigned(chunk))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The first value of an unsigned integer tag.
    pub(super) fn first_value(
        self,
        source: &impl BlockingBinaryReader,
        entry: &IfdEntry,
    ) -> Option<u64> {
        let unit = Self::unsigned_size(entry.field_type)?;
        if entry.count == 0 {
            return None;
        }
        if Self::byte_len(entry)? <= self.word() {
            return self.read_unsigned(&entry.value[..unit]);
        }
        self.read_unsigned(&source.read_range(self.value_offset(entry)?, unit)?)
    }

    pub(super) fn first_u32(
        self,
        source: &impl BlockingBinaryReader,
        entry: &IfdEntry,
    ) -> Option<u32> {
        self.first_value(source, entry)
            .and_then(|value| u32::try_from(value).ok())
    }

    pub(super) fn ascii(
        self,
        source: &impl BlockingBinaryReader,
        entry: &IfdEntry,
    ) -> Option<String> {
        parse_tiff_string_value(self.bytes(source, entry)?)
    }

    /// The value of `entry` as an embedded JPEG (Panasonic JpgFromRaw).
    pub(super) fn jpeg_candidate(
        self,
        source: &impl BlockingBinaryReader,
        entry: &IfdEntry,
    ) -> Option<StructuredJpegCandidate> {
        candidate_from_range(source, self.value_offset(entry)?, Self::byte_len(entry)?)
    }
}

fn tiff_type_size(field_type: u16) -> Option<usize> {
    Some(match field_type {
        TIFF_TYPE_BYTE | TIFF_TYPE_ASCII | TIFF_TYPE_SBYTE | TIFF_TYPE_UNDEFINED => 1,
        TIFF_TYPE_SHORT | TIFF_TYPE_SSHORT => 2,
        TIFF_TYPE_LONG | TIFF_TYPE_SLONG | TIFF_TYPE_FLOAT | TIFF_TYPE_IFD => 4,
        TIFF_TYPE_RATIONAL | TIFF_TYPE_SRATIONAL | TIFF_TYPE_DOUBLE | TIFF_TYPE_LONG8
        | TIFF_TYPE_SLONG8 | TIFF_TYPE_IFD8 => 8,
        _ => return None,
    })
}

/// Picks the best embedded JPEG among the IFD JPEG ranges, JPEG-compressed strips and tiles,
/// and Panasonic JpgFromRaw tags.
pub(super) fn extract_tiff_embedded_jpeg(
    source: &impl BlockingBinaryReader,
    max_dimension: u32,
) -> Option<StructuredJpegCandidate> {
    let tiff = Tiff::parse(source)?;
    let mut candidates = Vec::new();
    tiff.walk(source, true, |entries| {
        let mut jpeg_offset = None;
        let mut jpeg_length = None;
        let mut compression = None;
        let (mut strip_offsets, mut strip_counts) = (Vec::new(), Vec::new());
        let (mut tile_offsets, mut tile_counts) = (Vec::new(), Vec::new());
        for entry in entries {
            match entry.tag {
                TAG_COMPRESSION => compression = tiff.first_value(source, entry),
                TAG_STRIP_OFFSETS => strip_offsets = tiff.values(source, entry),
                TAG_STRIP_BYTE_COUNTS => strip_counts = tiff.values(source, entry),
                TAG_TILE_OFFSETS => tile_offsets = tiff.values(source, entry),
                TAG_TILE_BYTE_COUNTS => tile_counts = tiff.values(source, entry),
                TAG_JPEG_INTERCHANGE_FORMAT => jpeg_offset = tiff.first_value(source, entry),
                TAG_JPEG_INTERCHANGE_FORMAT_LENGTH => {
                    jpeg_length = tiff.first_value(source, entry);
                }
                TAG_PANASONIC_JPG_FROM_RAW | TAG_PANASONIC_JPG_FROM_RAW2 => {
                    candidates.extend(tiff.jpeg_candidate(source, entry));
                }
                _ => {}
            }
        }
        if let (Some(start), Some(length)) = (jpeg_offset, jpeg_length) {
            if let Ok(length) = usize::try_from(length) {
                candidates.extend(candidate_from_range(source, start, length));
            }
        }
        if matches!(
            compression,
            Some(c) if c == u64::from(COMPRESSION_JPEG_OLD) || c == u64::from(COMPRESSION_JPEG_NEW)
        ) {
            candidates.extend(candidate_from_segments(
                source,
                &strip_offsets,
                &strip_counts,
            ));
            candidates.extend(candidate_from_segments(source, &tile_offsets, &tile_counts));
        }
    });
    choose_best_candidate(candidates, max_dimension)
}

/// Dimensions of the primary (full-resolution) image.
pub(super) fn extract_tiff_primary_dimensions(
    source: &impl BlockingBinaryReader,
) -> Option<(u32, u32)> {
    let tiff = Tiff::parse(source)?;
    let mut candidates = Vec::new();
    tiff.walk(source, false, |entries| {
        let read = |tag| {
            entries
                .iter()
                .rfind(|entry| entry.tag == tag)
                .and_then(|entry| tiff.first_u32(source, entry))
        };
        // RW2 stores the image size as sensor borders.
        let width = read(TAG_IMAGE_WIDTH).or_else(|| {
            panasonic_image_dimension(
                read(TAG_PANASONIC_SENSOR_LEFT_BORDER),
                read(TAG_PANASONIC_SENSOR_RIGHT_BORDER),
            )
            .or_else(|| read(TAG_PANASONIC_SENSOR_WIDTH))
        });
        let height = read(TAG_IMAGE_LENGTH).or_else(|| {
            panasonic_image_dimension(
                read(TAG_PANASONIC_SENSOR_TOP_BORDER),
                read(TAG_PANASONIC_SENSOR_BOTTOM_BORDER),
            )
            .or_else(|| read(TAG_PANASONIC_SENSOR_HEIGHT))
        });
        if let (Some(width), Some(height)) = (width, height) {
            if width > 0 && height > 0 {
                candidates.push(PrimaryImageDimensionsCandidate {
                    width,
                    height,
                    reduced_resolution: is_reduced_resolution_ifd(
                        read(TAG_NEW_SUBFILE_TYPE),
                        read(TAG_SUBFILE_TYPE),
                    ),
                });
            }
        }
    });
    choose_primary_dimensions(candidates)
}
