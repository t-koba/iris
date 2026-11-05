//! Renders a generated PDF to verify that a packaged PDFium runtime is usable.

use std::fmt::Write as _;

fn build_pdf() -> Vec<u8> {
    let content = "0 0 0 rg\n20 20 160 160 re f\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
         /Contents 4 0 R /Resources << >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
    ];

    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        writeln!(out, "{} 0 obj\n{body}\nendobj", index + 1).expect("writing to a String");
    }
    let xref_start = out.len();
    out.push_str("xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        writeln!(out, "{offset:010} 00000 n ").expect("writing to a String");
    }
    write!(
        out,
        "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
    )
    .expect("writing to a String");
    out.into_bytes()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pdf = build_pdf();
    let path = std::env::temp_dir().join(format!("iris-pdf-smoke-{}.pdf", std::process::id()));
    std::fs::write(&path, &pdf)?;

    let thumbnail = iris_thumb_preview::pdf::extract_pdf_thumbnail(&path, 256);
    let _ = std::fs::remove_file(&path);

    let thumbnail = thumbnail.ok_or(
        "PDF thumbnail extraction failed; is the packaged PDFium runtime present and compatible?",
    )?;
    if !thumbnail.data.starts_with(&[0xFF, 0xD8]) {
        return Err("PDF thumbnail is not JPEG-encoded".into());
    }
    if thumbnail.width == 0 || thumbnail.height == 0 {
        return Err("PDF thumbnail has empty dimensions".into());
    }

    println!(
        "PDFium smoke passed: {}x{}, {} bytes",
        thumbnail.width,
        thumbnail.height,
        thumbnail.data.len()
    );
    Ok(())
}
