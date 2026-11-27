//! Correctness smoke plus bounded fixture-backed JPEG/CR3/video/PDF diagnostics.

use iris_core_domain::types::thumbnail::{
    ImageFormat, ThumbnailHint, ThumbnailResult, ThumbnailSource,
};
use iris_thumb_preview::{encoded, image_generic, pdf, raw, video};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{self, File},
    io::Read,
    path::Path,
    process::Command,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const MAX_DIMENSION: u32 = 256;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct LatencyConfig {
    warmups: usize,
    samples: usize,
}

fn latency_config() -> Result<LatencyConfig, Box<dyn Error>> {
    let warmups = std::env::var("IRIS_PREVIEW_LATENCY_WARMUPS")?.parse()?;
    let samples = std::env::var("IRIS_PREVIEW_LATENCY_SAMPLES")?.parse()?;
    if samples == 0 {
        return Err("IRIS_PREVIEW_LATENCY_SAMPLES must be greater than zero".into());
    }
    Ok(LatencyConfig { warmups, samples })
}

fn check(
    name: &str,
    result: Option<ThumbnailResult>,
    start: Instant,
    require_nonuniform_pixels: bool,
) -> ThumbnailResult {
    let thumb = result.unwrap_or_else(|| panic!("{name}: preview extraction failed"));
    assert_eq!(thumb.format, ImageFormat::Jpeg, "{name}: output format");
    let decoded = image::load_from_memory_with_format(&thumb.data, image::ImageFormat::Jpeg)
        .unwrap_or_else(|err| panic!("{name}: invalid JPEG: {err}"));
    assert_eq!(
        (decoded.width(), decoded.height()),
        (thumb.width, thumb.height)
    );
    assert!(thumb.width > 0 && thumb.height > 0, "{name}: empty output");
    assert!(thumb.width <= MAX_DIMENSION && thumb.height <= MAX_DIMENSION);
    // Reject blank output as well as header-only or dimension-only successes.
    if require_nonuniform_pixels {
        let pixels = decoded.to_rgb8();
        let first = pixels.get_pixel(0, 0);
        assert!(
            pixels.pixels().any(|pixel| pixel != first),
            "{name}: blank output"
        );
    }
    println!(
        "{name}: {}x{}, {} bytes, {:?}, single-run correctness diagnostic {:?}",
        thumb.width,
        thumb.height,
        thumb.data.len(),
        thumb.source,
        start.elapsed()
    );
    thumb
}

fn compiled_build_settings() -> Result<(&'static str, Option<&'static str>), Box<dyn Error>> {
    let profile = option_env!("IRIS_BUILD_PROFILE")
        .filter(|value| !value.is_empty())
        .ok_or("IRIS_BUILD_PROFILE was not compiled into the smoke")?;
    let opt_level = option_env!("IRIS_BUILD_OPT_LEVEL")
        .filter(|value| !value.is_empty())
        .ok_or("IRIS_BUILD_OPT_LEVEL was not compiled into the smoke")?;
    Ok((profile, (profile == "release").then_some(opt_level)))
}

fn digest_hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_pinned_fixture(dir: &Path, filename: &str) -> Result<(String, u64), Box<dyn Error>> {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/preview-corpus/fixtures.json");
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let entry = manifest["files"]
        .as_array()
        .and_then(|files| {
            files
                .iter()
                .find(|entry| entry["filename"].as_str() == Some(filename))
        })
        .ok_or("fixture is absent from the pinned manifest")?;
    let expected_size = entry["size"]
        .as_u64()
        .ok_or("fixture manifest has no valid size")?;
    let expected_digest = entry["sha256"]
        .as_str()
        .ok_or("fixture manifest has no SHA-256")?;

    let path = dir.join(filename);
    let actual_size = fs::metadata(&path)?.len();
    if actual_size != expected_size {
        return Err(format!(
            "{filename}: size {actual_size} does not match pinned size {expected_size}"
        )
        .into());
    }

    // Stream in a fixed-size buffer so verifying the 29 MiB CR3 does not add a
    // second whole-file allocation to the preview measurement process.
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or("fixture byte count overflow")?;
        if total > expected_size {
            return Err(format!("{filename}: input grew beyond its pinned size").into());
        }
        hasher.update(&buffer[..count]);
    }
    if total != expected_size {
        return Err(format!("{filename}: input ended before its pinned size").into());
    }
    let actual_digest = digest_hex(&hasher.finalize());
    if actual_digest != expected_digest {
        return Err(format!("{filename}: SHA-256 does not match the pinned manifest").into());
    }
    Ok((actual_digest, actual_size))
}

#[cfg(target_os = "linux")]
fn cpu_model() -> Option<String> {
    let contents = fs::read_to_string("/proc/cpuinfo").ok()?;
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        matches!(key.trim(), "model name" | "Hardware").then(|| value.trim().to_owned())
    })
}

#[cfg(target_os = "macos")]
fn cpu_model() -> Option<String> {
    for key in ["machdep.cpu.brand_string", "hw.model"] {
        let Ok(output) = Command::new("sysctl").args(["-n", key]).output() else {
            continue;
        };
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn cpu_model() -> Option<String> {
    std::env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn cpu_model() -> Option<String> {
    None
}

#[cfg(feature = "video")]
fn ffmpeg_runtime() -> serde_json::Value {
    serde_json::json!({
        "codec_version": ffmpeg_next::codec::version(),
        "format_version": ffmpeg_next::format::version(),
        "util_version": ffmpeg_next::util::version(),
        "configuration": ffmpeg_next::codec::configuration(),
        "license": ffmpeg_next::codec::license(),
    })
}

#[cfg(not(feature = "video"))]
fn ffmpeg_runtime() -> serde_json::Value {
    serde_json::Value::Null
}

fn runtime_identity() -> Result<serde_json::Value, Box<dyn Error>> {
    let rustc = Command::new("rustc")
        .arg("--version")
        .arg("--verbose")
        .output()?;
    if !rustc.status.success() {
        return Err("rustc --version --verbose did not succeed".into());
    }
    let rustc = String::from_utf8(rustc.stdout)?.trim().to_owned();
    let lockfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let lockfile_digest = digest_hex(&Sha256::digest(fs::read(lockfile)?));
    let (build_profile, release_opt_level) = compiled_build_settings()?;
    Ok(serde_json::json!({
        "identity_schema": "iris.preview_identity.v2",
        "source_revision": std::env::var("GITHUB_SHA").ok(),
        "workflow_run_id": std::env::var("GITHUB_RUN_ID").ok(),
        "crate": env!("CARGO_PKG_NAME"),
        "crate_version": env!("CARGO_PKG_VERSION"),
        "cargo_lock_sha256": lockfile_digest,
        "rustc": rustc,
        "target_os": std::env::consts::OS,
        "target_arch": std::env::consts::ARCH,
        "runner_name": std::env::var("RUNNER_NAME").ok(),
        "runner_os": std::env::var("RUNNER_OS").ok(),
        "runner_arch": std::env::var("RUNNER_ARCH").ok(),
        "runner_image_os": std::env::var("ImageOS").ok(),
        "runner_image_version": std::env::var("ImageVersion").ok(),
        "cpu_model": cpu_model(),
        "available_parallelism": std::thread::available_parallelism().ok().map(|n| n.get()),
        "build_profile": build_profile,
        "release_opt_level": release_opt_level,
        "features": {
            "video": cfg!(feature = "video"),
            "pdf": cfg!(feature = "pdf"),
            "raw_full": cfg!(feature = "raw-full"),
        },
        "ffmpeg_runtime": ffmpeg_runtime(),
        "jpeg_cr3_runtime": "Iris Rust preview code; dependency versions are bound by cargo_lock_sha256; FFmpeg and PDFium are not invoked by these two operations"
    }))
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    sorted[(sorted.len() - 1) * percent / 100]
}

struct MeasurementSpec<'a> {
    operation: &'a str,
    filename: &'a str,
    fixture_digest: &'a str,
    fixture_size: u64,
    output: &'a ThumbnailResult,
    identity: &'a serde_json::Value,
    uses_ffmpeg: bool,
    expected_pixels: Option<&'a image::RgbImage>,
}

fn latency_report<F>(
    spec: &MeasurementSpec<'_>,
    config: LatencyConfig,
    mut extract: F,
) -> Result<serde_json::Value, Box<dyn Error>>
where
    F: FnMut() -> Option<ThumbnailResult>,
{
    let expected_dimensions = (spec.output.width, spec.output.height);
    for _ in 0..config.warmups {
        let thumb = extract().ok_or("preview failed during latency warmup")?;
        validate_timed_output(
            spec.filename,
            &thumb,
            expected_dimensions,
            spec.expected_pixels,
        );
        std::hint::black_box(thumb);
    }

    let mut samples = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let start = Instant::now();
        let thumb = extract().ok_or("preview failed during latency measurement")?;
        let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        validate_timed_output(
            spec.filename,
            &thumb,
            expected_dimensions,
            spec.expected_pixels,
        );
        std::hint::black_box(thumb);
        samples.push(elapsed_ns);
    }

    samples.sort_unstable();
    let median_ns = percentile(&samples, 50);
    let mut deviations: Vec<u64> = samples
        .iter()
        .map(|sample| sample.abs_diff(median_ns))
        .collect();
    deviations.sort_unstable();
    let mut report = serde_json::json!({
        "record": "iris.preview_latency.v1",
        "operation": spec.operation,
        "uses_ffmpeg": spec.uses_ffmpeg,
        "input": {
            "filename": spec.filename,
            "sha256": spec.fixture_digest,
            "size_bytes": spec.fixture_size,
        },
        "output": {
            "format": "jpeg",
            "width": spec.output.width,
            "height": spec.output.height,
            "bytes": spec.output.data.len(),
            "source": format!("{:?}", spec.output.source),
        },
        "timing": {
            "unit": "nanoseconds",
            "warmup_iterations": config.warmups,
            "sample_count": samples.len(),
            "min": samples[0],
            "median": median_ns,
            "p90": percentile(&samples, 90),
            "median_absolute_deviation": percentile(&deviations, 50),
            "max": samples[samples.len() - 1],
        },
        "identity": spec.identity,
        "measurement_scope": "crate-level local-file extraction, not UI/end-to-end latency; CR3 measures embedded-preview extraction, not sensor demosaic",
        "comparison": "diagnostic only; pairing requires matching input, output dimensions and build/runner/source identity",
    });
    if let Some(pixels) = spec.expected_pixels {
        report["pdf_runtime"] = pdf_runtime();
        report["output"]["decoded_rgb_sha256"] =
            digest_hex(&Sha256::digest(pixels.as_raw())).into();
        report["repeated_output_validation"] = "decoded RGB equality to correctness baseline on every warmup/sample; validation excluded from timing".into();
        report["comparison"] =
            "renderer-specific diagnostic only; not an FFmpeg effect or cross-platform comparison"
                .into();
    }
    Ok(report)
}

/// Attribute the actually loaded PDFium library for opt-in gates; no token is printed.
fn print_pdfium_runtime() {
    // Only the opt-in gate emits a record; the path is the module actually
    // loaded by this process, never the env var value, so a system fallback
    // cannot be misattributed to the staged file.
    let required = std::env::var_os("IRIS_PDFIUM_REQUIRE_LIBRARY")
        .is_some_and(|value| !value.is_empty());
    if !required {
        return;
    }
    let Some(path) = loaded_pdfium_path() else {
        return;
    };
    println!(
        "PREVIEW_PDFIUM_RUNTIME {}",
        serde_json::json!({"path": path, "required": true})
    );
}

#[cfg(target_os = "linux")]
/// Path of the PDFium module actually loaded by this process.
///
/// `libloading` opens PDFium with `RTLD_LOCAL`, so its symbols are absent from
/// the default loader scope; the process' mappings are the source of truth.
fn loaded_pdfium_path() -> Option<String> {
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines().find_map(|line| {
        // `address perms offset dev inode pathname`; the pathname is the last
        // field and can itself contain spaces.
        let path = line.splitn(6, char::is_whitespace).nth(5)?;
        let name = Path::new(path).file_name()?.to_str()?;
        (name == "libpdfium.so" || name.starts_with("libpdfium.so.")).then(|| path.to_owned())
    })
}

#[cfg(target_os = "macos")]
extern "C" {
    fn _dyld_get_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const std::os::raw::c_char;
}

#[cfg(target_os = "macos")]
/// Path of the PDFium module actually loaded by this process.
///
/// dyld's image list is the source of truth; both queries live in libSystem.
fn loaded_pdfium_path() -> Option<String> {
    use std::ffi::CStr;

    // SAFETY: dyld only reports images loaded for the lifetime of this process
    // and each name is a NUL-terminated C string owned by dyld.
    let count = unsafe { _dyld_get_image_count() };
    (0..count).find_map(|index| {
        let image = unsafe { _dyld_get_image_name(index) };
        if image.is_null() {
            return None;
        }
        let path = unsafe { CStr::from_ptr(image) }.to_str().ok()?;
        let name = Path::new(path).file_name()?.to_str()?;
        (name == "libpdfium.so" || name.starts_with("libpdfium.so.")).then(|| path.to_owned())
    })
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
    fn GetModuleFileNameW(
        handle: *mut std::ffi::c_void,
        buf: *mut u16,
        size: u32,
    ) -> u32;
}

/// Path of the PDFium module actually loaded by this process, via the loader.
///
/// `GetModuleHandleW` finds `pdfium.dll` only when it is already loaded, and
/// `GetModuleFileNameW` reports the file that provided it; a system fallback
/// reports its own path instead of the staged file.
#[cfg(windows)]
fn loaded_pdfium_path() -> Option<String> {
    use std::os::windows::ffi::OsStringExt;

    fn wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let name = wide_null("pdfium.dll");
        // SAFETY: GetModuleHandleW only reads loader state; null means not loaded.
        let handle = GetModuleHandleW(name.as_ptr());
        if handle.is_null() {
            return None;
        }
        let mut capacity: u32 = 1024;
        loop {
            let mut buf = vec![0u16; capacity as usize];
            // SAFETY: buf is valid for `capacity` u16s.
            let len = GetModuleFileNameW(handle, buf.as_mut_ptr(), capacity);
            if len == 0 {
                return None;
            }
            if len < capacity - 1 {
                buf.truncate(len as usize);
                let path = std::ffi::OsString::from_wide(&buf).into_string().ok()?;
                if path.is_empty() {
                    return None;
                }
                return Some(path);
            }
            capacity = capacity.checked_mul(2)?;
            if capacity > 32768 {
                return None;
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn loaded_pdfium_path() -> Option<String> {
    None
}

fn pdf_runtime() -> serde_json::Value {
    if cfg!(target_os = "macos") {
        serde_json::json!({"renderer": "CoreGraphics", "runtime": "macOS system framework"})
    } else {
        serde_json::json!({"renderer": "PDFium", "runtime": "dynamically loaded PDFium; this diagnostic does not establish provenance or attestation"})
    }
}

fn validate_timed_output(
    name: &str,
    thumb: &ThumbnailResult,
    expected: (u32, u32),
    expected_pixels: Option<&image::RgbImage>,
) {
    assert_eq!(
        thumb.format,
        ImageFormat::Jpeg,
        "{name}: timed output format"
    );
    assert_eq!(
        (thumb.width, thumb.height),
        expected,
        "{name}: timed output dimensions changed"
    );
    assert!(!thumb.data.is_empty(), "{name}: timed output is empty");
    if let Some(pixels) = expected_pixels {
        let decoded = image::load_from_memory_with_format(&thumb.data, image::ImageFormat::Jpeg)
            .unwrap_or_else(|err| panic!("{name}: invalid repeated JPEG: {err}"))
            .to_rgb8();
        assert!(
            &decoded == pixels,
            "{name}: repeated rendered pixels changed"
        );
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = std::env::args_os()
        .nth(1)
        .ok_or("expected corpus directory")?;
    let dir = Path::new(&dir);
    let config = latency_config()?;
    assert!(video::supports_video_thumbnails());
    assert!(pdf::supports_pdf_thumbnails());
    // CR3 alone is also supported by raw-core; check full RAW feature availability explicitly.
    assert!(raw::supports_raw_extension("raf") && raw::supports_raw_extension("x3f"));
    let hint = ThumbnailHint {
        max_dimension: MAX_DIMENSION,
        preferred_formats: vec![ImageFormat::Jpeg],
    };

    // Verify every input before correctness or timing work, including video/PDF.
    let filenames = [
        "landscape.jpg",
        "sample.jpg",
        "canon-eos-r.cr3",
        "bunny.mp4",
        "one-page.pdf",
        "hevc.mp4",
    ];
    let pins = filenames
        .iter()
        .map(|name| verify_pinned_fixture(dir, name))
        .collect::<Result<Vec<_>, _>>()?;
    let jpeg_pin = &pins[0];
    let cr3_pin = &pins[2];
    let video_pin = &pins[3];
    let pdf_pin = &pins[4];
    let hevc_pin = &pins[5];
    let jpeg = dir.join("landscape.jpg");
    let info = encoded::probe_encoded_image_path(&jpeg).ok_or("missing JPEG dimensions")?;
    assert_eq!((info.width, info.height), (4000, 3000));
    let start = Instant::now();
    let jpeg_thumb = check(
        "high-res JPEG",
        image_generic::extract_image_thumbnail(&jpeg, &hint),
        start,
        true,
    );
    assert_eq!((jpeg_thumb.width, jpeg_thumb.height), (256, 192));

    let small_jpeg = dir.join("sample.jpg");
    let start = Instant::now();
    let small_thumb = check(
        "small JPEG passthrough",
        image_generic::extract_image_thumbnail(&small_jpeg, &hint),
        start,
        false,
    );
    assert_eq!((small_thumb.width, small_thumb.height), (200, 200));
    assert_eq!(small_thumb.source, ThumbnailSource::OriginalEncoded);
    assert_eq!(small_thumb.data, std::fs::read(&small_jpeg)?);

    let cr3 = dir.join("canon-eos-r.cr3");
    let start = Instant::now();
    let cr3_thumb = check(
        "CR3",
        raw::extract_raw_thumbnail(&cr3, MAX_DIMENSION, &CancellationToken::new()),
        start,
        true,
    );
    assert_eq!(cr3_thumb.source, ThumbnailSource::EmbeddedPreview);

    let video = dir.join("bunny.mp4");
    let start = Instant::now();
    let video_thumb = check(
        "H.264 MP4",
        video::extract_video_thumbnail(&video, MAX_DIMENSION),
        start,
        false, // The pinned clip's first keyframe is an intentional black fade-in.
    );
    assert_eq!((video_thumb.width, video_thumb.height), (256, 144));
    assert_eq!(video_thumb.source, ThumbnailSource::PartialDecode);

    let hevc = dir.join("hevc.mp4");
    let start = Instant::now();
    let hevc_thumb = check(
        "H.265/HEVC MP4",
        video::extract_video_thumbnail(&hevc, MAX_DIMENSION),
        start,
        false, // First-frame uniformity of this clip is not established; check decode and dimensions.
    );
    assert_eq!((hevc_thumb.width, hevc_thumb.height), (256, 144));
    assert_eq!(hevc_thumb.source, ThumbnailSource::PartialDecode);

    let path = dir.join("one-page.pdf");
    assert_eq!(pdf::pdf_page_count(&path), Some(1));
    let start = Instant::now();
    let pdf_thumb = check(
        "PDF",
        pdf::extract_pdf_thumbnail(&path, MAX_DIMENSION),
        start,
        true,
    );
    print_pdfium_runtime();

    // Decode the correctness baseline once, outside all timed intervals.
    let pdf_pixels =
        image::load_from_memory_with_format(&pdf_thumb.data, image::ImageFormat::Jpeg)?.to_rgb8();
    let identity = runtime_identity()?;
    let outputs = [
        &jpeg_thumb,
        &small_thumb,
        &cr3_thumb,
        &video_thumb,
        &pdf_thumb,
        &hevc_thumb,
    ];
    let fixtures: Vec<_> = filenames
        .iter()
        .zip(&pins)
        .zip(outputs)
        .map(|((filename, pin), thumb)| {
            let mut fixture = serde_json::json!({
                "filename": filename, "sha256": pin.0, "size_bytes": pin.1,
                "output": {"width": thumb.width, "height": thumb.height, "format": "jpeg",
                           "source": format!("{:?}", thumb.source)},
            });
            if *filename == "one-page.pdf" {
                fixture["output"]["decoded_rgb_sha256"] =
                    digest_hex(&Sha256::digest(pdf_pixels.as_raw())).into();
            }
            fixture
        })
        .collect();
    println!(
        "PREVIEW_CORRECTNESS {}",
        serde_json::json!({
            "record": "iris.preview_correctness.v1", "passed": true,
            "fixtures": fixtures, "identity": identity,
            "scope": "JPEG resize/passthrough, CR3 embedded preview, H.264 and H.265/HEVC decode, PDF page count/render; decoded JPEG dimensions and nonuniform photo/RAW/PDF pixels checked; HEVC decoder path and dimensions checked without asserting first-frame uniformity; not sensor demosaic or UI parity",
        })
    );
    let jpeg_report = latency_report(
        &MeasurementSpec {
            operation: "image_generic::extract_image_thumbnail",
            filename: "landscape.jpg",
            fixture_digest: &jpeg_pin.0,
            fixture_size: jpeg_pin.1,
            output: &jpeg_thumb,
            identity: &identity,
            uses_ffmpeg: false,
            expected_pixels: None,
        },
        config,
        || image_generic::extract_image_thumbnail(&jpeg, &hint),
    )?;
    let cr3_report = latency_report(
        &MeasurementSpec {
            operation: "raw::extract_raw_thumbnail",
            filename: "canon-eos-r.cr3",
            fixture_digest: &cr3_pin.0,
            fixture_size: cr3_pin.1,
            output: &cr3_thumb,
            identity: &identity,
            uses_ffmpeg: false,
            expected_pixels: None,
        },
        config,
        || raw::extract_raw_thumbnail(&cr3, MAX_DIMENSION, &CancellationToken::new()),
    )?;
    let video_report = latency_report(
        &MeasurementSpec {
            operation: "video::extract_video_thumbnail",
            filename: "bunny.mp4",
            fixture_digest: &video_pin.0,
            fixture_size: video_pin.1,
            output: &video_thumb,
            identity: &identity,
            uses_ffmpeg: true,
            expected_pixels: None,
        },
        config,
        || video::extract_video_thumbnail(&video, MAX_DIMENSION),
    )?;
    let pdf_report = latency_report(
        &MeasurementSpec {
            operation: "pdf::extract_pdf_thumbnail",
            filename: "one-page.pdf",
            fixture_digest: &pdf_pin.0,
            fixture_size: pdf_pin.1,
            output: &pdf_thumb,
            identity: &identity,
            uses_ffmpeg: false,
            expected_pixels: Some(&pdf_pixels),
        },
        config,
        || pdf::extract_pdf_thumbnail(&path, MAX_DIMENSION),
    )?;
    let hevc_report = latency_report(
        &MeasurementSpec {
            operation: "video::extract_hevc_thumbnail",
            filename: "hevc.mp4",
            fixture_digest: &hevc_pin.0,
            fixture_size: hevc_pin.1,
            output: &hevc_thumb,
            identity: &identity,
            uses_ffmpeg: true,
            expected_pixels: None,
        },
        config,
        || video::extract_video_thumbnail(&hevc, MAX_DIMENSION),
    )?;
    println!("PREVIEW_LATENCY {}", serde_json::to_string(&jpeg_report)?);
    println!("PREVIEW_LATENCY {}", serde_json::to_string(&cr3_report)?);
    println!("PREVIEW_LATENCY {}", serde_json::to_string(&video_report)?);
    println!("PREVIEW_LATENCY {}", serde_json::to_string(&pdf_report)?);
    println!("PREVIEW_LATENCY {}", serde_json::to_string(&hevc_report)?);
    println!("Corpus correctness smoke passed; latency reports are bounded diagnostics, not parity evidence.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumbnail(value: u8) -> ThumbnailResult {
        let rgb = image::RgbImage::from_pixel(2, 2, image::Rgb([value; 3]));
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(rgb)
            .write_to(&mut data, image::ImageFormat::Jpeg)
            .unwrap();
        ThumbnailResult {
            data: data.into_inner(),
            format: ImageFormat::Jpeg,
            width: 2,
            height: 2,
            source: ThumbnailSource::PartialDecode,
        }
    }

    #[test]
    fn pdf_repetitions_validate_pixels_in_warmups_and_samples() {
        let baseline = thumbnail(0);
        let pixels = image::load_from_memory(&baseline.data).unwrap().to_rgb8();
        let identity = serde_json::json!({"target_os": std::env::consts::OS});
        let spec = MeasurementSpec {
            operation: "pdf::extract_pdf_thumbnail",
            filename: "one-page.pdf",
            fixture_digest: "synthetic",
            fixture_size: 1,
            output: &baseline,
            identity: &identity,
            uses_ffmpeg: false,
            expected_pixels: Some(&pixels),
        };
        let config = LatencyConfig {
            warmups: 1,
            samples: 2,
        };
        let mut calls = 0;
        let report = latency_report(&spec, config, || {
            calls += 1;
            Some(thumbnail(0))
        })
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(report["timing"]["sample_count"], 2);
        assert_eq!(report["timing"]["warmup_iterations"], 1);
        assert_eq!(report["pdf_runtime"], pdf_runtime());
        assert_eq!(
            report["output"]["decoded_rgb_sha256"],
            digest_hex(&Sha256::digest(pixels.as_raw()))
        );

        // Same dimensions and valid JPEG bytes are insufficient: reject a pixel
        // change at each warmup/measured position, and reject invalid JPEG data.
        for bad_call in 1..=3 {
            for malformed in [false, true] {
                let mut calls = 0;
                assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = latency_report(&spec, config, || {
                        calls += 1;
                        let mut thumb = thumbnail(if calls == bad_call { 255 } else { 0 });
                        if malformed && calls == bad_call {
                            thumb.data = vec![1, 2, 3];
                        }
                        Some(thumb)
                    });
                }))
                .is_err());
            }
        }
    }
}
