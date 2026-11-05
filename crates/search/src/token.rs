use crate::filter::{FilterModel, GpsBounds, GpsFilter, RangeFilter, Scope};
use crate::segment::IndexedKind;
use chrono::{NaiveDate, TimeZone, Utc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseWarning {
    pub token: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedFilter {
    pub model: FilterModel,
    pub warnings: Vec<ParseWarning>,
}

pub fn parse_filter_tokens(input: &str) -> ParsedFilter {
    let mut model = FilterModel::default();
    let mut text_terms = Vec::new();
    let mut warnings = Vec::new();

    for token in split_tokens(input) {
        if token.is_empty() {
            continue;
        }
        if let Some((key, value)) = token.split_once(':') {
            match key.to_ascii_lowercase().as_str() {
                "text" | "name" => {
                    if !value.is_empty() {
                        text_terms.push(value.to_string());
                    }
                }
                "scope" => match value.to_ascii_lowercase().as_str() {
                    "recursive" | "rec" | "all" => model.scope = Scope::Recursive,
                    "folder" | "this" | "current" => model.scope = Scope::ThisFolder,
                    _ => warn(&mut warnings, &token, "unknown scope"),
                },
                "kind" | "type" => match parse_kind(value) {
                    Some(kind) => model.kinds.push(kind),
                    None => warn(&mut warnings, &token, "unknown kind"),
                },
                "before" => match parse_date_end(value) {
                    Some(max) => merge_date_max(&mut model, max),
                    None => warn(&mut warnings, &token, "invalid date"),
                },
                "after" => match parse_date_start(value) {
                    Some(min) => merge_date_min(&mut model, min),
                    None => warn(&mut warnings, &token, "invalid date"),
                },
                "date" => match parse_date_range(value) {
                    Some(range) => model.date = Some(range),
                    None => warn(&mut warnings, &token, "invalid date range"),
                },
                "size" => match parse_size_filter(value) {
                    Some(range) => model.size = Some(range),
                    None => warn(&mut warnings, &token, "invalid size"),
                },
                "camera" => set_non_empty(&mut model.camera, value),
                "lens" => set_non_empty(&mut model.lens, value),
                "gps" => match parse_gps_filter(value) {
                    Some(gps) => model.gps = gps,
                    None => warn(&mut warnings, &token, "invalid gps filter"),
                },
                "has" if value.eq_ignore_ascii_case("gps") => model.gps = GpsFilter::Present,
                "no" if value.eq_ignore_ascii_case("gps") => model.gps = GpsFilter::Absent,
                "content" => match parse_bool(value) {
                    Some(enabled) => model.content = enabled,
                    None => warn(&mut warnings, &token, "invalid content flag"),
                },
                _ => {
                    text_terms.push(token);
                }
            }
        } else {
            match token.to_ascii_lowercase().as_str() {
                "recursive" => model.scope = Scope::Recursive,
                "content" => model.content = true,
                "has:gps" => model.gps = GpsFilter::Present,
                "no:gps" => model.gps = GpsFilter::Absent,
                _ => text_terms.push(token),
            }
        }
    }

    model.kinds.sort();
    model.kinds.dedup();
    model.text = text_terms.join(" ");
    ParsedFilter { model, warnings }
}

pub fn format_filter_tokens(model: &FilterModel) -> String {
    let mut tokens = Vec::new();
    if !model.text.is_empty() {
        tokens.push(format!("text:{}", quote_if_needed(&model.text)));
    }
    if model.scope == Scope::Recursive {
        tokens.push("scope:recursive".to_string());
    }
    for kind in &model.kinds {
        tokens.push(format!("kind:{}", format_kind(*kind)));
    }
    if let Some(range) = &model.date {
        if let Some(min) = range.min {
            tokens.push(format!("after:{}", format_date(min)));
        }
        if let Some(max) = range.max {
            tokens.push(format!("before:{}", format_date(max)));
        }
    }
    if let Some(range) = &model.size {
        match (range.min, range.max) {
            (Some(min), Some(max)) => tokens.push(format!("size:{}..{}", min, max)),
            (Some(min), None) => tokens.push(format!("size:>{min}")),
            (None, Some(max)) => tokens.push(format!("size:<{max}")),
            (None, None) => {}
        }
    }
    if let Some(camera) = model.camera.as_deref() {
        tokens.push(format!("camera:{}", quote_if_needed(camera)));
    }
    if let Some(lens) = model.lens.as_deref() {
        tokens.push(format!("lens:{}", quote_if_needed(lens)));
    }
    match &model.gps {
        GpsFilter::Any => {}
        GpsFilter::Present => tokens.push("gps:present".to_string()),
        GpsFilter::Absent => tokens.push("gps:absent".to_string()),
        GpsFilter::Bounds(bounds) => tokens.push(format!(
            "gps:{},{},{},{}",
            trim_float(bounds.min_lat),
            trim_float(bounds.max_lat),
            trim_float(bounds.min_lon),
            trim_float(bounds.max_lon)
        )),
    }
    if model.content {
        tokens.push("content:true".to_string());
    }
    tokens.join(" ")
}

fn split_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(active), '\\') if chars.peek().is_some() => {
                current.push(chars.next().unwrap_or('\\'));
                if active == '\0' {
                    quote = None;
                }
            }
            (Some(active), value) if value == active => {
                quote = None;
            }
            (None, '"' | '\'') => quote = Some(ch),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            (Some(_), value) | (None, value) => current.push(value),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn parse_kind(value: &str) -> Option<IndexedKind> {
    Some(match value.to_ascii_lowercase().as_str() {
        "file" => IndexedKind::File,
        "dir" | "directory" | "folder" => IndexedKind::Directory,
        "image" | "photo" => IndexedKind::Image,
        "video" | "movie" => IndexedKind::Video,
        "pdf" => IndexedKind::Pdf,
        "other" => IndexedKind::Other,
        _ => return None,
    })
}

fn format_kind(kind: IndexedKind) -> &'static str {
    match kind {
        IndexedKind::File => "file",
        IndexedKind::Directory => "directory",
        IndexedKind::Image => "image",
        IndexedKind::Video => "video",
        IndexedKind::Pdf => "pdf",
        IndexedKind::Other => "other",
    }
}

fn parse_date_start(value: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(
        Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
            .timestamp(),
    )
}

fn parse_date_end(value: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(
        Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59)?)
            .timestamp(),
    )
}

fn parse_date_range(value: &str) -> Option<RangeFilter<i64>> {
    let (start, end) = value.split_once("..")?;
    Some(RangeFilter {
        min: (!start.is_empty())
            .then(|| parse_date_start(start))
            .flatten(),
        max: (!end.is_empty()).then(|| parse_date_end(end)).flatten(),
    })
    .filter(|range| range.min.is_some() || range.max.is_some())
}

fn format_date(timestamp: i64) -> String {
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map(|datetime| datetime.date_naive().format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn parse_size_filter(value: &str) -> Option<RangeFilter<u64>> {
    if let Some(rest) = value.strip_prefix('>') {
        return parse_size_bytes(rest).map(|min| RangeFilter {
            min: Some(min),
            max: None,
        });
    }
    if let Some(rest) = value.strip_prefix('<') {
        return parse_size_bytes(rest).map(|max| RangeFilter {
            min: None,
            max: Some(max),
        });
    }
    if let Some((min, max)) = value.split_once("..") {
        return Some(RangeFilter {
            min: (!min.is_empty()).then(|| parse_size_bytes(min)).flatten(),
            max: (!max.is_empty()).then(|| parse_size_bytes(max)).flatten(),
        })
        .filter(|range| range.min.is_some() || range.max.is_some());
    }
    parse_size_bytes(value).map(|exact| RangeFilter {
        min: Some(exact),
        max: Some(exact),
    })
}

fn parse_size_bytes(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    let split_at = trimmed
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split_at);
    let number = number.parse::<f64>().ok()?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = number * multiplier;
    bytes.is_finite().then_some(bytes.round() as u64)
}

fn parse_gps_filter(value: &str) -> Option<GpsFilter> {
    match value.to_ascii_lowercase().as_str() {
        "present" | "yes" | "true" => Some(GpsFilter::Present),
        "absent" | "no" | "false" => Some(GpsFilter::Absent),
        "any" => Some(GpsFilter::Any),
        _ => {
            let parts = value
                .split(',')
                .map(str::trim)
                .map(str::parse::<f32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            (parts.len() == 4).then_some(GpsFilter::Bounds(GpsBounds {
                min_lat: parts[0],
                max_lat: parts[1],
                min_lon: parts[2],
                max_lon: parts[3],
            }))
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn merge_date_min(model: &mut FilterModel, min: i64) {
    let range = model.date.get_or_insert(RangeFilter {
        min: None,
        max: None,
    });
    range.min = Some(min);
}

fn merge_date_max(model: &mut FilterModel, max: i64) {
    let range = model.date.get_or_insert(RangeFilter {
        min: None,
        max: None,
    });
    range.max = Some(max);
}

fn set_non_empty(target: &mut Option<String>, value: &str) {
    if !value.is_empty() {
        *target = Some(value.to_string());
    }
}

fn quote_if_needed(value: &str) -> String {
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\''))
    {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

fn trim_float(value: f32) -> String {
    let formatted = format!("{value:.6}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn warn(warnings: &mut Vec<ParseWarning>, token: &str, reason: &str) {
    warnings.push(ParseWarning {
        token: token.to_string(),
        reason: reason.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_facets_ranges_and_flags() {
        let parsed = parse_filter_tokens(
            "invoice kind:pdf camera:\"Sony Alpha\" lens:FE35 after:2024-01-02 before:2024-01-31 size:>10MiB gps:present content:true scope:recursive",
        );

        assert!(parsed.warnings.is_empty());
        assert_eq!(parsed.model.text, "invoice");
        assert_eq!(parsed.model.scope, Scope::Recursive);
        assert_eq!(parsed.model.kinds, vec![IndexedKind::Pdf]);
        assert_eq!(parsed.model.camera.as_deref(), Some("Sony Alpha"));
        assert_eq!(parsed.model.lens.as_deref(), Some("FE35"));
        assert_eq!(parsed.model.size.unwrap().min, Some(10 * 1024 * 1024));
        assert_eq!(parsed.model.gps, GpsFilter::Present);
        assert!(parsed.model.content);
        let date = parsed.model.date.as_ref().expect("date range");
        assert!(date.min.unwrap() < date.max.unwrap());
    }

    #[test]
    fn parses_bounds_and_size_range() {
        let parsed = parse_filter_tokens("gps:34.5,35.5,138.1,140.2 size:1MB..2MB");

        assert_eq!(
            parsed.model.gps,
            GpsFilter::Bounds(GpsBounds {
                min_lat: 34.5,
                max_lat: 35.5,
                min_lon: 138.1,
                max_lon: 140.2,
            })
        );
        assert_eq!(
            parsed.model.size,
            Some(RangeFilter {
                min: Some(1_000_000),
                max: Some(2_000_000),
            })
        );
    }

    #[test]
    fn formats_and_reparses_model_stably() {
        let model = FilterModel {
            text: "日本語 PDF".to_string(),
            scope: Scope::Recursive,
            kinds: vec![IndexedKind::Image, IndexedKind::Pdf],
            date: Some(RangeFilter {
                min: parse_date_start("2024-01-01"),
                max: parse_date_end("2024-12-31"),
            }),
            size: Some(RangeFilter {
                min: Some(1024),
                max: None,
            }),
            camera: Some("Canon EOS".to_string()),
            lens: None,
            gps: GpsFilter::Absent,
            content: true,
        };

        let formatted = format_filter_tokens(&model);
        let reparsed = parse_filter_tokens(&formatted);

        assert!(reparsed.warnings.is_empty(), "{:?}", reparsed.warnings);
        assert_eq!(reparsed.model, model);
    }

    #[test]
    fn formatting_preserves_text_boundary_whitespace_and_backslashes() {
        let model = FilterModel {
            text: " leading\\path \"quoted\"\n".to_string(),
            ..FilterModel::default()
        };

        let formatted = format_filter_tokens(&model);
        let reparsed = parse_filter_tokens(&formatted);

        assert!(reparsed.warnings.is_empty(), "{:?}", reparsed.warnings);
        assert_eq!(reparsed.model, model);
    }

    #[test]
    fn reports_invalid_tokens_without_dropping_plain_text() {
        let parsed = parse_filter_tokens("hello before:nope kind:unknown unknown:key");

        assert_eq!(parsed.model.text, "hello unknown:key");
        assert_eq!(parsed.warnings.len(), 2);
    }
}
