use flate2::read::ZlibDecoder;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfText {
    pub text: String,
    pub pages_scanned: u32,
    pub truncated: bool,
}

pub fn extract_pdf_text(file_path: &Path, max_pages: u32) -> PdfText {
    if max_pages == 0 || !is_pdf_path(file_path) {
        return PdfText::default();
    }
    let Ok(bytes) = std::fs::read(file_path) else {
        return PdfText::default();
    };
    extract_pdf_text_from_bytes(&bytes, max_pages)
}

fn is_pdf_path(file_path: &Path) -> bool {
    file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn extract_pdf_text_from_bytes(bytes: &[u8], max_pages: u32) -> PdfText {
    let page_count = count_pdf_pages(bytes);
    let streams = extract_content_streams(bytes);
    let mut text = String::new();
    for stream in streams {
        extract_text_operators(&stream, &mut text);
    }

    PdfText {
        text: normalize_extracted_text(&text),
        pages_scanned: page_count.min(max_pages).max((!text.is_empty()) as u32),
        truncated: page_count > max_pages,
    }
}

fn count_pdf_pages(bytes: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(bytes);
    text.match_indices("/Type")
        .filter(|(idx, _)| text[*idx..].starts_with("/Type /Page"))
        .filter(|(idx, _)| !text[*idx..].starts_with("/Type /Pages"))
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn extract_content_streams(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut streams = Vec::new();
    let mut cursor = 0;
    while let Some(stream_pos) = find_bytes(&bytes[cursor..], b"stream") {
        let stream_start_token = cursor + stream_pos;
        let data_start = skip_stream_line_end(bytes, stream_start_token + b"stream".len());
        let Some(end_rel) = find_bytes(&bytes[data_start..], b"endstream") else {
            break;
        };
        let data_end = trim_stream_line_end(bytes, data_start + end_rel);
        let dict_start = bytes[..stream_start_token]
            .windows(2)
            .rposition(|window| window == b"<<")
            .unwrap_or(stream_start_token);
        let dict = &bytes[dict_start..stream_start_token];
        let raw = &bytes[data_start..data_end];
        if dict
            .windows(b"/FlateDecode".len())
            .any(|w| w == b"/FlateDecode")
        {
            let mut decoder = ZlibDecoder::new(raw);
            let mut decoded = Vec::new();
            if decoder.read_to_end(&mut decoded).is_ok() {
                streams.push(decoded);
            }
        } else {
            streams.push(raw.to_vec());
        }
        cursor = data_end + b"endstream".len();
    }
    streams
}

fn extract_text_operators(stream: &[u8], output: &mut String) {
    let mut index = 0;
    while index < stream.len() {
        match stream[index] {
            b'(' => {
                if let Some((value, next)) = read_literal_string(stream, index + 1) {
                    push_pdf_string(output, &value);
                    index = next;
                } else {
                    index += 1;
                }
            }
            b'<' if stream.get(index + 1) != Some(&b'<') => {
                if let Some((value, next)) = read_hex_string(stream, index + 1) {
                    push_pdf_string(output, &value);
                    index = next;
                } else {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
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

fn push_pdf_string(output: &mut String, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(text) = decode_utf16be(bytes) {
        output.push_str(&text);
    } else {
        output.push_str(&String::from_utf8_lossy(bytes));
    }
    output.push(' ');
}

fn decode_utf16be(bytes: &[u8]) -> Option<String> {
    let data = bytes.strip_prefix(&[0xfe, 0xff])?;
    if data.len() % 2 != 0 {
        return None;
    }
    String::from_utf16(
        &data
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    )
    .ok()
}

fn normalize_extracted_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

    #[test]
    fn ignores_non_pdf_paths() {
        let text = extract_pdf_text(Path::new("note.txt"), 8);
        assert_eq!(text, PdfText::default());
    }

    #[test]
    fn zero_page_limit_skips_work() {
        let text = extract_pdf_text(Path::new("scan.pdf"), 0);
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
        let text = extract_pdf_text_from_bytes(pdf, 8);
        assert_eq!(text.pages_scanned, 1);
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

        let text = extract_pdf_text_from_bytes(&pdf, 8);
        assert_eq!(text.text, "Compressed text");
    }
}
