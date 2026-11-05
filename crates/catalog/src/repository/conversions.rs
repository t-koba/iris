use super::*;

pub(super) fn system_time_to_secs(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(super) fn system_time_to_parts(t: Option<SystemTime>) -> (Option<i64>, Option<i64>) {
    match t {
        Some(t) => {
            let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
            (Some(d.as_secs() as i64), Some(d.subsec_nanos() as i64))
        }
        None => (None, None),
    }
}

pub(super) fn parts_to_system_time(secs: Option<i64>, nanos: Option<i64>) -> Option<SystemTime> {
    let s = secs?;
    let n = nanos.unwrap_or(0);
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(s as u64, n as u32))
}

pub(super) fn secs_to_system_time(secs: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
}

pub(super) fn metadata_level_to_i32(level: &MetadataLevel) -> i32 {
    match level {
        MetadataLevel::L0 => 0,
        MetadataLevel::L1 => 1,
        MetadataLevel::L2 => 2,
        MetadataLevel::L3 => 3,
    }
}

pub(super) fn i32_to_metadata_level(i: i32) -> MetadataLevel {
    match i {
        1 => MetadataLevel::L1,
        2 => MetadataLevel::L2,
        3 => MetadataLevel::L3,
        _ => MetadataLevel::L0,
    }
}

pub(super) fn image_format_to_str(format: &ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::WebP => "webp",
        ImageFormat::Png => "png",
    }
}

pub(super) fn str_to_image_format(s: &str) -> ImageFormat {
    match s {
        "webp" => ImageFormat::WebP,
        "png" => ImageFormat::Png,
        _ => ImageFormat::Jpeg,
    }
}

pub(super) fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
