use std::time::SystemTime;

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_time(time: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = time.into();
    if crate::i18n::is_japanese_locale() {
        dt.format("%Y/%m/%d %H:%M").to_string()
    } else {
        dt.format("%b %-d, %Y %H:%M").to_string()
    }
}

pub fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return "...".to_string();
    }

    let mut iter = input.char_indices();
    match iter.nth(max_chars) {
        Some((idx, _)) => format!("{}...", &input[..idx]),
        None => input.to_string(),
    }
}
