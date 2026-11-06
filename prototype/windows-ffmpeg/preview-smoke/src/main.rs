//! Prototype-only installed-preview smoke: video, generated PDF, synthetic RAW.
//!
//! Test-only; never a release payload. It exercises the product default preview
//! APIs (`video`, `pdf`, `raw-full`) against a freshly installed prototype app
//! directory. PDF construction mirrors
//! `crates/thumb-preview/examples/pdf_thumbnail_smoke.rs`; the RAW fixture
//! reuses `iris_thumb_preview::test_fixtures` builders.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

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

fn check_jpeg(label: &str, data: &[u8], width: u32, height: u32) -> Result<(), String> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(format!("{label}: thumbnail is not JPEG-encoded"));
    }
    if width == 0 || height == 0 {
        return Err(format!("{label}: thumbnail has empty dimensions"));
    }
    println!("{label} passed: {width}x{height}, {} bytes", data.len());
    Ok(())
}

fn smoke_pdf(work: &Path) -> Result<(), String> {
    let pdf = build_pdf();
    let path = work.join(format!("iris-prototype-pdf-{}.pdf", std::process::id()));
    std::fs::write(&path, &pdf).map_err(|e| format!("pdf write failed: {e}"))?;
    let result = iris_thumb_preview::pdf::extract_pdf_thumbnail(&path, 256);
    let _ = std::fs::remove_file(&path);
    let thumbnail = result.ok_or_else(|| {
        "pdf thumbnail extraction failed; is the pinned pdfium.dll present and compatible?"
            .to_string()
    })?;
    check_jpeg("pdf", &thumbnail.data, thumbnail.width, thumbnail.height)
}

fn smoke_raw(work: &Path) -> Result<(), String> {
    // Default raw-full fixture (never minimal raw-core): same builder shape as
    // `raw/tests.rs`, embedded JPEG via TIFF IFD decoded through the default
    // `raw-full` preview path.
    let jpeg = iris_thumb_preview::test_fixtures::jpeg(48, 32);
    let tiff = iris_thumb_preview::test_fixtures::tiff_with_jpeg(64, &jpeg);
    let path = work.join(format!("iris-prototype-raw-{}.dng", std::process::id()));
    std::fs::write(&path, &tiff).map_err(|e| format!("raw write failed: {e}"))?;
    let result =
        iris_thumb_preview::raw::extract_raw_thumbnail(&path, 256, &CancellationToken::new());
    let _ = std::fs::remove_file(&path);
    let thumbnail = result.ok_or_else(|| "raw thumbnail extraction failed".to_string())?;
    if (thumbnail.width, thumbnail.height) != (48, 32) {
        return Err(format!(
            "raw thumbnail has unexpected dimensions {}x{} (want 48x32)",
            thumbnail.width, thumbnail.height
        ));
    }
    check_jpeg("raw", &thumbnail.data, thumbnail.width, thumbnail.height)
}

fn smoke_video(work: &Path, video_arg: Option<PathBuf>) -> Result<(), String> {
    let caller_supplied = video_arg.is_some() || std::env::var("IRIS_PROTOTYPE_TEST_VIDEO").is_ok();
    let path = if let Some(p) = video_arg {
        p
    } else if let Ok(env) = std::env::var("IRIS_PROTOTYPE_TEST_VIDEO") {
        PathBuf::from(env)
    } else {
        // Generate a 1s test pattern with the prototype FFmpeg executable only.
        // No PATH fallback to an unknown ffmpeg: the harness fails closed when
        // IRIS_PROTOTYPE_FFMPEG_EXE (the pinned prototype tool) is absent.
        let generated = work.join(format!("iris-prototype-{}.mp4", std::process::id()));
        let ffmpeg = match std::env::var("IRIS_PROTOTYPE_FFMPEG_EXE") {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
            _ => {
                return Err(
                    "video generator missing: IRIS_PROTOTYPE_FFMPEG_EXE (pinned prototype ffmpeg.exe)                      is not set and no --video/IRIS_PROTOTYPE_TEST_VIDEO sample was supplied"
                        .to_string(),
                );
            }
        };
        if !ffmpeg.is_file() {
            return Err(format!(
                "video generator missing: prototype ffmpeg.exe not found: {}",
                ffmpeg.display()
            ));
        }
        println!("video generator: {}", ffmpeg.display());
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=10:duration=1",
                "-c:v",
                "mpeg4",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&generated)
            .status()
            .map_err(|e| {
                format!(
                    "video generation failed: {} is unavailable ({e}); set IRIS_PROTOTYPE_TEST_VIDEO or IRIS_PROTOTYPE_FFMPEG_EXE",
                    ffmpeg.display()
                )
            })?;
        if !status.success() || !generated.is_file() {
            return Err(
                "video generation failed; set IRIS_PROTOTYPE_TEST_VIDEO to a pre-generated sample"
                    .to_string(),
            );
        }
        generated
    };
    if !path.is_file() {
        return Err(format!("test video missing: {}", path.display()));
    }
    let result = iris_thumb_preview::video::extract_video_thumbnail(&path, 256);
    // Clean up only files this smoke generated; caller-supplied samples are kept.
    if !caller_supplied {
        let _ = std::fs::remove_file(&path);
    }
    let thumbnail = result.ok_or_else(|| {
        "video thumbnail extraction failed; is the pinned FFmpeg DLL closure present and compatible?"
            .to_string()
    })?;
    check_jpeg("video", &thumbnail.data, thumbnail.width, thumbnail.height)
}

fn print_usage(program: &str) {
    eprintln!("usage: {program} [--app-dir <dir>] [--video <file>] [--work-dir <dir>]");
    eprintln!(
        "  --app-dir  installed prototype app dir (checked for Iris.exe/pdfium.dll/FFmpeg DLLs)"
    );
    eprintln!("  --video    pre-generated sample video (else generate via $IRIS_PROTOTYPE_FFMPEG_EXE lavfi or $IRIS_PROTOTYPE_TEST_VIDEO)");
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let program = args
        .first()
        .cloned()
        .unwrap_or_else(|| "preview-smoke".to_string());
    let mut app_dir: Option<PathBuf> = None;
    let mut video: Option<PathBuf> = None;
    let mut work_dir: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--app-dir" => {
                i += 1;
                app_dir = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "missing value for --app-dir".to_string())?,
                ));
            }
            "--video" => {
                i += 1;
                video = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "missing value for --video".to_string())?,
                ));
            }
            "--work-dir" => {
                i += 1;
                work_dir = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "missing value for --work-dir".to_string())?,
                ));
            }
            "--help" | "-h" => {
                print_usage(&program);
                return Ok(());
            }
            other => {
                print_usage(&program);
                return Err(format!("unknown argument: {other}"));
            }
        }
        i += 1;
    }

    if let Some(dir) = &app_dir {
        for required in ["Iris.exe", "pdfium.dll"] {
            if !dir.join(required).is_file() {
                return Err(format!(
                    "installed app dir {} is missing {required}",
                    dir.display()
                ));
            }
        }
        let dlls: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
            })
            .collect();
        // Iris.exe + pdfium.dll + at least one FFmpeg DLL must be present.
        if dlls.len() < 2 {
            return Err(format!(
                "installed app dir {} has no FFmpeg DLL closure (found {} dlls)",
                dir.display(),
                dlls.len()
            ));
        }
        let pdfium_path = dir.join("pdfium.dll");
        let pdfium_bytes = std::fs::metadata(&pdfium_path)
            .map(|m| m.len())
            .unwrap_or(0);
        println!(
            "installed app dir {}: Iris.exe + pdfium.dll ({} bytes) + {} dlls; cwd={}",
            dir.display(),
            pdfium_bytes,
            dlls.len(),
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "?".to_string())
        );
        if pdfium_bytes == 0 {
            return Err(format!(
                "installed app dir {} has an empty pdfium.dll",
                dir.display()
            ));
        }
    }

    let work = work_dir.unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&work).map_err(|e| format!("work dir failed: {e}"))?;

    if !iris_thumb_preview::pdf::supports_pdf_thumbnails() {
        return Err(
            "product pdf feature is disabled; prototype requires default features".to_string(),
        );
    }
    if !iris_thumb_preview::video::supports_video_thumbnails() {
        return Err(
            "product video feature is disabled; prototype requires default features".to_string(),
        );
    }

    smoke_pdf(&work)?;
    smoke_raw(&work)?;
    smoke_video(&work, video)?;
    println!("prototype preview smoke passed (video, pdf, raw)");
    Ok(())
}
