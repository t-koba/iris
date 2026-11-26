//! Pinned JPEG 2000 correctness and bounded latency smoke for paired source reports.
//!
//! Uses only APIs available in the rebuilt reference source revision, so the same
//! file can run there as an untracked example without changing that revision.

use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailHint, ThumbnailResult};
use iris_thumb_preview::image_generic;
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::Path, process::Command, time::Instant};

const MAX_DIMENSION: u32 = 128;
const EXPECTED_SIZE: u64 = 393;
const EXPECTED_SHA256: &str = "e44d3480025d87433287c64775c0486cc2e6f7369c6df5b8a249d89f6d2a2caa";
const OPERATION: &str = "image_generic::extract_image_thumbnail_from_bytes";

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

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_pin(path: &Path) -> Result<String, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != EXPECTED_SIZE {
        return Err(format!(
            "JP2 size {} does not match pinned size {EXPECTED_SIZE}",
            bytes.len()
        )
        .into());
    }
    let digest = digest_hex(&Sha256::digest(&bytes));
    if digest != EXPECTED_SHA256 {
        return Err("JP2 SHA-256 does not match the pinned fixture".into());
    }
    Ok(digest)
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

fn identity() -> Result<serde_json::Value, Box<dyn Error>> {
    let rustc = Command::new("rustc")
        .args(["--version", "--verbose"])
        .output()?;
    if !rustc.status.success() {
        return Err("rustc --version --verbose did not succeed".into());
    }
    Ok(serde_json::json!({
        "identity_schema": "iris.preview_identity.v2",
        "source_revision": std::env::var("GITHUB_SHA").ok(),
        "workflow_run_id": std::env::var("GITHUB_RUN_ID").ok(),
        "rustc": String::from_utf8(rustc.stdout)?.trim().to_owned(),
        "target_os": std::env::consts::OS,
        "target_arch": std::env::consts::ARCH,
        "runner_name": std::env::var("RUNNER_NAME").ok(),
        "runner_os": std::env::var("RUNNER_OS").ok(),
        "runner_arch": std::env::var("RUNNER_ARCH").ok(),
        "runner_image_os": std::env::var("ImageOS").ok(),
        "runner_image_version": std::env::var("ImageVersion").ok(),
        "cpu_model": cpu_model(),
        "available_parallelism": std::thread::available_parallelism().ok().map(|n| n.get()),
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "release_opt_level": if cfg!(debug_assertions) {
            None
        } else {
            Some(std::env::var("CARGO_PROFILE_RELEASE_OPT_LEVEL").unwrap_or_else(|_| "3".into()))
        },
        "features": {
            "video": cfg!(feature = "video"),
            "pdf": cfg!(feature = "pdf"),
            "raw_full": cfg!(feature = "raw-full"),
        },
    }))
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    sorted[(sorted.len() - 1) * percent / 100]
}

fn validate_timed_output(name: &str, thumb: &ThumbnailResult, expected: (u32, u32)) {
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
}

fn check_output(name: &str, thumb: &ThumbnailResult, expected: (u32, u32)) -> image::RgbImage {
    validate_timed_output(name, thumb, expected);
    let decoded = image::load_from_memory_with_format(&thumb.data, image::ImageFormat::Jpeg)
        .unwrap_or_else(|err| panic!("{name}: invalid JPEG: {err}"))
        .to_rgb8();
    assert_eq!(
        (decoded.width(), decoded.height()),
        expected,
        "{name}: decoded dimensions"
    );
    let first = decoded.get_pixel(0, 0);
    assert!(
        decoded.pixels().any(|pixel| pixel != first),
        "{name}: blank output"
    );
    decoded
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os().nth(1).ok_or("expected JP2 path")?;
    let path = Path::new(&path);
    let config = latency_config()?;
    let digest = verify_pin(path)?;
    let filename = path
        .file_name()
        .ok_or("JP2 path has no filename")?
        .to_string_lossy()
        .into_owned();
    let bytes = fs::read(path)?;
    let hint = ThumbnailHint {
        max_dimension: MAX_DIMENSION,
        preferred_formats: vec![ImageFormat::Jpeg],
    };
    let extract = || image_generic::extract_image_thumbnail_from_bytes(&bytes, &hint);
    let start = Instant::now();
    let thumb = extract().ok_or("JPEG 2000 preview extraction failed")?;
    let pixels = check_output(&filename, &thumb, (MAX_DIMENSION, MAX_DIMENSION));
    println!(
        "{filename}: {}x{}, {} bytes, {:?}, single-run correctness diagnostic {:?}",
        thumb.width,
        thumb.height,
        thumb.data.len(),
        thumb.source,
        start.elapsed()
    );

    let input = serde_json::json!({
        "filename": filename,
        "sha256": digest,
        "size_bytes": EXPECTED_SIZE,
    });
    let output = serde_json::json!({
        "format": "jpeg",
        "width": thumb.width,
        "height": thumb.height,
        "bytes": thumb.data.len(),
        "source": format!("{:?}", thumb.source),
        "decoded_rgb_sha256": digest_hex(&Sha256::digest(pixels.as_raw())),
    });
    let identity = identity()?;
    println!(
        "PREVIEW_CORRECTNESS {}",
        serde_json::to_string(&serde_json::json!({
            "record": "iris.preview_correctness.v1",
            "passed": true,
            "fixtures": [{
                "filename": input["filename"],
                "sha256": digest,
                "size_bytes": EXPECTED_SIZE,
                "output": output,
            }],
            "identity": identity,
        }))?
    );

    for _ in 0..config.warmups {
        let warm = extract().ok_or("JPEG 2000 preview failed during warmup")?;
        validate_timed_output(&filename, &warm, (MAX_DIMENSION, MAX_DIMENSION));
        std::hint::black_box(warm);
    }
    let mut samples = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let start = Instant::now();
        let timed = extract().ok_or("JPEG 2000 preview failed during measurement")?;
        let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        validate_timed_output(&filename, &timed, (MAX_DIMENSION, MAX_DIMENSION));
        std::hint::black_box(timed);
        samples.push(elapsed);
    }
    samples.sort_unstable();
    let median = percentile(&samples, 50);
    let mut deviations: Vec<u64> = samples
        .iter()
        .map(|sample| sample.abs_diff(median))
        .collect();
    deviations.sort_unstable();
    println!(
        "PREVIEW_LATENCY {}",
        serde_json::to_string(&serde_json::json!({
            "record": "iris.preview_latency.v1",
            "operation": OPERATION,
            "uses_ffmpeg": false,
            "input": input,
            "output": output,
            "timing": {
                "unit": "nanoseconds",
                "warmup_iterations": config.warmups,
                "sample_count": samples.len(),
                "min": samples[0],
                "median": median,
                "p90": percentile(&samples, 90),
                "median_absolute_deviation": percentile(&deviations, 50),
                "max": samples[samples.len() - 1],
            },
            "identity": identity,
            "measurement_scope": "crate-level local-byte extraction, not UI/end-to-end latency",
            "comparison": "diagnostic only; no parity claim or release-gate change",
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumbnail(rgb: &image::RgbImage) -> ThumbnailResult {
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(rgb.clone())
            .write_to(&mut data, image::ImageFormat::Jpeg)
            .unwrap();
        ThumbnailResult {
            data: data.into_inner(),
            format: ImageFormat::Jpeg,
            width: rgb.width(),
            height: rgb.height(),
            source: iris_core_domain::types::thumbnail::ThumbnailSource::FullDecode,
        }
    }

    #[test]
    fn output_check_rejects_uniform_pixels() {
        let uniform = image::RgbImage::from_pixel(8, 8, image::Rgb([7, 7, 7]));
        let thumb = thumbnail(&uniform);
        validate_timed_output("uniform", &thumb, (8, 8));
        assert!(std::panic::catch_unwind(|| check_output("uniform", &thumb, (8, 8))).is_err());
        // Lossy JPEG can flatten a two-level difference, so use a stark split.
        let varied = image::RgbImage::from_fn(8, 8, |x, _| {
            if x < 4 {
                image::Rgb([0, 0, 0])
            } else {
                image::Rgb([255, 255, 255])
            }
        });
        let pixels = check_output("varied", &thumbnail(&varied), (8, 8));
        assert_ne!(pixels.get_pixel(0, 0), pixels.get_pixel(7, 7));
        assert_eq!(percentile(&[1, 2, 3, 4], 50), 2);
        assert_eq!(digest_hex(&[0, 15, 255]), "000fff");
    }
}
