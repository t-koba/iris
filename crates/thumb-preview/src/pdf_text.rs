use flate2::read::ZlibDecoder;
use std::io::Read;
use std::path::Path;

/// Per-document search budgets. Policy/defaults belong to the caller's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfTextLimits {
    pub input_bytes: usize,
    pub decoded_bytes: usize,
    pub text_bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfText {
    pub text: String,
    /// This stream parser cannot associate text with pages; always None.
    pub pages_scanned: Option<u32>,
    /// A budget, I/O error, or undecodable stream prevented a full stream scan.
    /// False still means best-effort text extraction, not complete PDF interpretation.
    pub truncated: bool,
}

pub fn extract_pdf_text(file_path: &Path, limits: PdfTextLimits) -> PdfText {
    if !is_pdf_path(file_path) {
        return PdfText::default();
    }
    match std::fs::File::open(file_path) {
        Ok(file) => extract_pdf_text_from_reader(file, limits),
        Err(_) => PdfText {
            truncated: true,
            ..PdfText::default()
        },
    }
}

fn is_pdf_path(file_path: &Path) -> bool {
    file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

// Never reads beyond the budget, even if a file grows after opening. At an exact
// boundary EOF is unknown: conservatively report truncation without a probe read.
fn read_budgeted(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    while bytes.len() < limit {
        let count = buffer.len().min(limit - bytes.len());
        match reader.read(&mut buffer[..count]) {
            Ok(0) => return (bytes, false),
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return (bytes, true),
        }
    }
    (bytes, true)
}

fn extract_pdf_text_from_reader(reader: impl Read, limits: PdfTextLimits) -> PdfText {
    let (bytes, mut truncated) = read_budgeted(reader, limits.input_bytes);
    let mut output = TextOutput {
        text: String::new(),
        pending_space: false,
        limit: limits.text_bytes,
    };
    truncated |= limits.decoded_bytes == 0 || limits.text_bytes == 0;
    let mut remaining = limits.decoded_bytes;
    let mut cursor = 0;
    while limits.text_bytes > 0 && remaining > 0 {
        let Some(stream_pos) = find_bytes(&bytes[cursor..], b"stream") else {
            break;
        };
        let stream_start_token = cursor + stream_pos;
        let data_start = skip_stream_line_end(&bytes, stream_start_token + b"stream".len());
        let Some(end_rel) = find_bytes(&bytes[data_start..], b"endstream") else {
            truncated = true;
            break;
        };
        let end_token = data_start + end_rel;
        let data_end = trim_stream_line_end(&bytes, end_token).max(data_start);
        let dict_start = bytes[cursor..stream_start_token]
            .windows(2)
            .rposition(|window| window == b"<<")
            .map_or(stream_start_token, |pos| cursor + pos);
        let dict = &bytes[dict_start..stream_start_token];
        let raw = &bytes[data_start..data_end];
        let decoded;
        let stream;
        let stopped;
        match stream_uses_flate(dict) {
            Some(true) => {
                (decoded, stopped) = read_budgeted(ZlibDecoder::new(raw), remaining);
                stream = decoded.as_slice();
            }
            Some(false) => {
                stream = &raw[..raw.len().min(remaining)];
                stopped = raw.len() >= remaining;
            }
            None => {
                truncated = true;
                cursor = end_token + b"endstream".len();
                continue;
            }
        }
        remaining -= stream.len();
        let complete = extract_text_operators(stream, &mut output);
        truncated |= stopped || !complete;
        if stopped || !complete {
            break;
        }
        cursor = end_token + b"endstream".len();
    }
    PdfText {
        text: output.text,
        pages_scanned: None,
        truncated,
    }
}

// Only an absent Filter or a single direct /FlateDecode name is supported.
// Arrays (even single-element ones), indirect values, duplicate keys and decode
// parameters are not interpreted: a partial decode must never produce a negative
// search signature. Nested dictionaries are conservatively rejected.
fn stream_uses_flate(dict: &[u8]) -> Option<bool> {
    let dict = dict.trim_ascii().strip_prefix(b"<<")?.strip_suffix(b">>")?;
    // Selection starts at the last <<. Any remaining dictionary delimiter means
    // that this is not an unambiguous, flat stream dictionary (e.g. DecodeParms
    // hid the enclosing Filter). Never treat its encoded bytes as plain text.
    if dict
        .windows(2)
        .any(|token| token == b"<<" || token == b">>")
    {
        return None;
    }
    let is_boundary = |byte: u8| byte.is_ascii_whitespace() || b"\0()<>[]{}/%".contains(&byte);
    if dict
        .windows(b"/DecodeParms".len())
        .enumerate()
        .any(|(pos, token)| {
            token == b"/DecodeParms"
                && dict
                    .get(pos + b"/DecodeParms".len())
                    .is_none_or(|byte| is_boundary(*byte))
        })
    {
        return None;
    }
    let mut filters = dict
        .windows(b"/Filter".len())
        .enumerate()
        .filter(|(pos, token)| {
            *token == b"/Filter"
                && dict
                    .get(pos + b"/Filter".len())
                    .is_none_or(|byte| is_boundary(*byte))
        });
    let Some((pos, _)) = filters.next() else {
        return Some(false);
    };
    if filters.next().is_some() {
        return None;
    }
    let value = dict[pos + b"/Filter".len()..]
        .trim_ascii_start()
        .strip_prefix(b"/FlateDecode")?;
    value
        .first()
        .is_none_or(|byte| is_boundary(*byte))
        .then_some(true)
}

fn extract_text_operators(stream: &[u8], output: &mut TextOutput) -> bool {
    let mut index = 0;
    while index < stream.len() {
        let parsed = match stream[index] {
            b'(' => Some(read_literal_string(stream, index + 1)),
            b'<' if stream.get(index + 1) != Some(&b'<') => {
                Some(read_hex_string(stream, index + 1))
            }
            _ => None,
        };
        if let Some(parsed) = parsed {
            let Some((value, next)) = parsed else {
                return false;
            };
            if !output.push_pdf_string(&value) {
                return false;
            }
            index = next;
        } else {
            index += 1;
        }
    }
    true
}

fn read_literal_string(stream: &[u8], mut index: usize) -> Option<(Vec<u8>, usize)> {
    let mut value = Vec::new();
    let mut depth = 1usize;
    while index < stream.len() {
        let byte = stream[index];
        match byte {
            b'\\' => {
                index += 1;
                let escaped = *stream.get(index)?;
                value.push(match escaped {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'(' | b')' | b'\\' => escaped,
                    other => other,
                });
            }
            b'(' => {
                depth += 1;
                value.push(byte);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((value, index + 1));
                }
                value.push(byte);
            }
            _ => value.push(byte),
        }
        index += 1;
    }
    None
}

fn read_hex_string(stream: &[u8], mut index: usize) -> Option<(Vec<u8>, usize)> {
    let mut nibbles = Vec::new();
    while index < stream.len() {
        let byte = stream[index];
        if byte == b'>' {
            if nibbles.len() % 2 == 1 {
                nibbles.push(b'0');
            }
            let mut value = Vec::new();
            for chunk in nibbles.chunks(2) {
                let hi = hex_value(chunk[0])?;
                let lo = hex_value(chunk[1])?;
                value.push((hi << 4) | lo);
            }
            return Some((value, index + 1));
        }
        if !byte.is_ascii_whitespace() {
            nibbles.push(byte);
        }
        index += 1;
    }
    None
}

// Normalize while emitting; no whole decoded/normalized intermediate String.
// Limits count UTF-8 bytes and never split a character.
struct TextOutput {
    text: String,
    pending_space: bool,
    limit: usize,
}

impl TextOutput {
    fn push_char(&mut self, ch: char) -> bool {
        if ch.is_whitespace() {
            self.pending_space = !self.text.is_empty();
            return true;
        }
        let extra = ch.len_utf8() + usize::from(self.pending_space);
        if extra > self.limit - self.text.len() {
            return false;
        }
        if self.pending_space {
            self.text.push(' ');
        }
        self.pending_space = false;
        self.text.push(ch);
        true
    }

    fn push_pdf_string(&mut self, bytes: &[u8]) -> bool {
        if let Some(data) = bytes
            .strip_prefix(&[0xfe, 0xff])
            .filter(|data| data.len() % 2 == 0)
        {
            for ch in char::decode_utf16(
                data.chunks_exact(2)
                    .map(|p| u16::from_be_bytes([p[0], p[1]])),
            ) {
                if !self.push_char(ch.unwrap_or(char::REPLACEMENT_CHARACTER)) {
                    return false;
                }
            }
        } else {
            for chunk in bytes.utf8_chunks() {
                for ch in chunk.valid().chars() {
                    if !self.push_char(ch) {
                        return false;
                    }
                }
                if !chunk.invalid().is_empty() && !self.push_char(char::REPLACEMENT_CHARACTER) {
                    return false;
                }
            }
        }
        self.pending_space = !self.text.is_empty();
        true
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn skip_stream_line_end(bytes: &[u8], mut index: usize) -> usize {
    if bytes.get(index) == Some(&b'\r') {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\n') {
        index += 1;
    }
    index
}

fn trim_stream_line_end(bytes: &[u8], mut index: usize) -> usize {
    while index > 0 && matches!(bytes.get(index - 1), Some(b'\r' | b'\n')) {
        index -= 1;
    }
    index
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const LIMITS: PdfTextLimits = PdfTextLimits {
        input_bytes: 4096,
        decoded_bytes: 4096,
        text_bytes: 4096,
    };

    #[test]
    fn ignores_non_pdf_paths() {
        let text = extract_pdf_text(Path::new("note.txt"), LIMITS);
        assert_eq!(text, PdfText::default());
    }

    #[test]
    fn extracts_literal_and_hex_text_from_uncompressed_stream() {
        let pdf = br#"%PDF-1.4
1 0 obj << /Type /Page /Contents 2 0 R >> endobj
2 0 obj << /Length 64 >>
stream
BT (Hello\ World) Tj <FEFF65E5672C8A9E> Tj [(PDF) 120 (text)] TJ ET
endstream
endobj
%%EOF"#;
        let text = extract_pdf_text_from_reader(pdf.as_slice(), LIMITS);
        assert_eq!(text.pages_scanned, None);
        assert!(!text.truncated);
        assert_eq!(text.text, "Hello World 日本語 PDF text");
    }

    #[test]
    fn extracts_text_from_flate_stream() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(b"BT (Compressed text) Tj ET")
            .expect("write flate input");
        let encoded = encoder.finish().expect("finish flate");
        let mut pdf = b"%PDF-1.4\n1 0 obj << /Type /Page /Contents 2 0 R >> endobj\n2 0 obj << /Filter /FlateDecode >>\nstream\n".to_vec();
        pdf.extend_from_slice(&encoded);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF");

        let text = extract_pdf_text_from_reader(pdf.as_slice(), LIMITS);
        assert_eq!(text.text, "Compressed text");
    }
    #[test]
    fn filter_arrays_and_chains_are_incomplete_not_single_flate() {
        let compressed = stream_pdf(&[(b"(must not decode)", true)]);
        let header = b"%PDF-1.4\n<< /Filter /FlateDecode >>";
        for filter in [
            "/Filter [/ASCII85Decode /FlateDecode]",
            "/Filter [/FlateDecode /ASCII85Decode]",
            "/Filter [/FlateDecode /FlateDecode]",
            "/Filter [/FlateDecode]",
            "/Filter /FlateDecode /Filter /ASCII85Decode",
            "/Filter /FlateDecodeExtra",
            "/Filter /ASCII85Decode /Other /FlateDecode",
        ] {
            let mut pdf = format!("%PDF-1.4\n<< {filter} >>").into_bytes();
            pdf.extend_from_slice(&compressed[header.len()..]);
            pdf.extend_from_slice(&stream_pdf(&[(b"(later text)", false)]));
            let result = extract_pdf_text_from_reader(pdf.as_slice(), LIMITS);
            assert!(result.truncated, "{filter}");
            assert_eq!(result.text, "later text", "{filter}");
        }
        // PDF name boundaries do not require intervening spaces.
        assert_eq!(stream_uses_flate(b"<</Filter/FlateDecode>>"), Some(true));
        assert_eq!(
            stream_uses_flate(b"<</FilterOther/FlateDecode>>"),
            Some(false)
        );
    }

    #[test]
    fn nested_decode_parameters_skip_stream_and_keep_later_text() {
        for dictionary in [
            "/Filter /ASCII85Decode /DecodeParms << /Columns 5 >>",
            "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >>",
            "/DecodeParms << /Columns 5 >> /Filter /ASCII85Decode",
            "/DecodeParms << /Columns 5 >>",
            "/Filter /FlateDecode /DecodeParms 2 0 R",
        ] {
            let mut pdf =
                format!("%PDF-1.4\n<< {dictionary} >>\nstream\n(must not extract)\nendstream\n")
                    .into_bytes();
            pdf.extend_from_slice(&stream_pdf(&[(b"(later text)", false)]));
            let result = extract_pdf_text_from_reader(pdf.as_slice(), LIMITS);
            assert!(result.truncated, "{dictionary}");
            assert_eq!(result.text, "later text", "{dictionary}");
        }
    }

    fn stream_pdf(streams: &[(&[u8], bool)]) -> Vec<u8> {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;
        let mut pdf = b"%PDF-1.4\n".to_vec();
        for (data, compressed) in streams {
            pdf.extend_from_slice(if *compressed {
                b"<< /Filter /FlateDecode >>\nstream\n"
            } else {
                b"<< >>\nstream\n"
            });
            if *compressed {
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(data).unwrap();
                pdf.extend_from_slice(&encoder.finish().unwrap());
            } else {
                pdf.extend_from_slice(data);
            }
            pdf.extend_from_slice(b"\nendstream\n");
        }
        pdf
    }

    #[test]
    fn actual_file_reads_stop_at_input_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bounded.pdf");
        let mut pdf = stream_pdf(&[(b"(early)", false)]);
        let limit = pdf.len() + 1;
        pdf.extend_from_slice(&[b' '; 256]);
        pdf.extend_from_slice(&stream_pdf(&[(b"(late)", false)]));
        std::fs::write(&path, pdf).unwrap();
        let result = extract_pdf_text(
            &path,
            PdfTextLimits {
                input_bytes: limit,
                ..LIMITS
            },
        );
        assert_eq!(result.text, "early");
        assert!(result.truncated);
        assert_eq!(result.pages_scanned, None);
    }

    #[test]
    fn growing_file_cannot_exceed_read_budget() {
        use std::io::Write;
        struct GrowingFile {
            file: std::fs::File,
            writer: Option<std::fs::File>,
            read: usize,
        }
        impl Read for GrowingFile {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let count = self.file.read(buf)?;
                self.read += count;
                if let Some(mut writer) = self.writer.take() {
                    writer.write_all(&[b' '; 256])?;
                }
                Ok(count)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.pdf");
        let pdf = stream_pdf(&[(b"(early)", false)]);
        std::fs::write(&path, &pdf).unwrap();
        let mut reader = GrowingFile {
            file: std::fs::File::open(&path).unwrap(),
            writer: Some(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap(),
            ),
            read: 0,
        };
        let limit = pdf.len() + 7;
        assert!(reader.file.metadata().unwrap().len() < limit as u64);
        let result = extract_pdf_text_from_reader(
            &mut reader,
            PdfTextLimits {
                input_bytes: limit,
                ..LIMITS
            },
        );
        assert_eq!(reader.read, limit);
        assert!(result.truncated);
        assert_eq!(result.text, "early");
        // No extra EOF/overrun probe, including at an exact boundary.
        let mut cursor = std::io::Cursor::new(vec![0; 16]);
        let (bytes, stopped) = read_budgeted(&mut cursor, 16);
        assert_eq!(bytes.len(), 16);
        assert_eq!(cursor.position(), 16);
        assert!(stopped);
    }

    #[test]
    fn compressed_expansion_stops_during_decode() {
        let mut data = b"(early) (".to_vec();
        data.extend_from_slice(&[b'x'; 65536]);
        data.push(b')');
        let pdf = stream_pdf(&[(&data, true)]);
        assert!(pdf.len() < 512);
        let limits = PdfTextLimits {
            decoded_bytes: 128,
            ..LIMITS
        };
        let result = extract_pdf_text_from_reader(pdf.as_slice(), limits);
        assert_eq!(result.text, "early");
        assert!(result.truncated);
        let (decoded, stopped) = read_budgeted(
            ZlibDecoder::new(&pdf[pdf.windows(7).position(|w| w == b"stream\n").unwrap() + 7..]),
            128,
        );
        assert_eq!(decoded.len(), 128);
        assert!(stopped);
    }

    #[test]
    fn decoded_budget_is_cumulative_for_all_streams() {
        for compressed in [false, true] {
            let pdf = stream_pdf(&[
                (b"(one)", compressed),
                (b"(two)", compressed),
                (b"(hidden)", compressed),
            ]);
            let result = extract_pdf_text_from_reader(
                pdf.as_slice(),
                PdfTextLimits {
                    decoded_bytes: 8,
                    ..LIMITS
                },
            );
            assert_eq!(result.text, "one");
            assert!(result.truncated);
        }
    }

    #[test]
    fn output_budget_counts_normalized_utf8_across_strings() {
        for (data, limit, expected) in [
            ("(one) (two)".as_bytes(), 5, "one t"),
            ("(é日本)".as_bytes(), 7, "é日"),
            (b"<FEFF65E5672C8A9E>".as_slice(), 7, "日本"),
        ] {
            let pdf = stream_pdf(&[(data, false)]);
            let result = extract_pdf_text_from_reader(
                pdf.as_slice(),
                PdfTextLimits {
                    text_bytes: limit,
                    ..LIMITS
                },
            );
            assert_eq!(result.text, expected);
            assert!(result.text.len() <= limit);
            assert!(result.truncated);
        }
    }

    #[test]
    fn zero_budgets_disable_work_not_limits_and_page_count_is_unavailable() {
        let pdf = stream_pdf(&[(b"(first)", false), (b"(second)", false)]);
        let full = extract_pdf_text_from_reader(pdf.as_slice(), LIMITS);
        assert_eq!(full.text, "first second");
        assert!(!full.truncated);
        assert_eq!(full.pages_scanned, None);
        for limits in [
            PdfTextLimits {
                input_bytes: 0,
                ..LIMITS
            },
            PdfTextLimits {
                decoded_bytes: 0,
                ..LIMITS
            },
            PdfTextLimits {
                text_bytes: 0,
                ..LIMITS
            },
        ] {
            let result = extract_pdf_text_from_reader(pdf.as_slice(), limits);
            assert!(result.truncated);
            assert!(result.text.is_empty());
            assert_eq!(result.pages_scanned, None);
        }
    }

    #[test]
    fn failed_streams_are_not_reported_as_complete() {
        let corrupt = b"<< /Filter /FlateDecode >>\nstream\nnot zlib\nendstream";
        let unsupported = b"<< /Filter /ASCII85Decode >>\nstream\nabc\nendstream";
        let missing_end = b"<< >>\nstream\n(early)";
        for pdf in [
            corrupt.as_slice(),
            unsupported.as_slice(),
            missing_end.as_slice(),
        ] {
            assert!(extract_pdf_text_from_reader(pdf, LIMITS).truncated);
        }
        assert!(extract_pdf_text(Path::new("missing.pdf"), LIMITS).truncated);
    }
}
