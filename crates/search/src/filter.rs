use crate::bloom::BloomQuery;
use crate::ngram::normalize_text;
use crate::segment::{IndexedKind, Segment, SegmentRecord};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    ThisFolder,
    Recursive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeFilter<T> {
    pub min: Option<T>,
    pub max: Option<T>,
}

impl<T: Ord + Copy> RangeFilter<T> {
    pub fn contains(&self, value: T) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpsBounds {
    pub min_lat: f32,
    pub max_lat: f32,
    pub min_lon: f32,
    pub max_lon: f32,
}

impl GpsBounds {
    pub fn contains(&self, lat: f32, lon: f32) -> bool {
        let lat_match = lat >= self.min_lat && lat <= self.max_lat;
        let lon_match = if self.min_lon <= self.max_lon {
            lon >= self.min_lon && lon <= self.max_lon
        } else {
            lon >= self.min_lon || lon <= self.max_lon
        };
        lat_match && lon_match
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum GpsFilter {
    #[default]
    Any,
    Present,
    Absent,
    Bounds(GpsBounds),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FilterModel {
    pub text: String,
    pub scope: Scope,
    pub kinds: Vec<IndexedKind>,
    pub date: Option<RangeFilter<i64>>,
    pub size: Option<RangeFilter<u64>>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub gps: GpsFilter,
    pub content: bool,
}

impl Default for FilterModel {
    fn default() -> Self {
        Self {
            text: String::new(),
            scope: Scope::ThisFolder,
            kinds: Vec::new(),
            date: None,
            size: None,
            camera: None,
            lens: None,
            gps: GpsFilter::Any,
            content: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FilterHit<'a> {
    pub record: &'a SegmentRecord,
    pub score: u32,
    pub content_candidate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentSnippet {
    pub snippet: String,
    pub match_byte_range: std::ops::Range<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facets {
    pub kinds: BTreeMap<IndexedKind, usize>,
    pub cameras: BTreeMap<String, usize>,
    pub lenses: BTreeMap<String, usize>,
    pub gps_present: usize,
    pub gps_absent: usize,
    pub gps_points: Vec<(f32, f32)>,
}

#[derive(Clone, Debug)]
pub struct CompiledFilter {
    model: FilterModel,
    normalized_text: String,
    normalized_camera: Option<String>,
    normalized_lens: Option<String>,
    content_query: BloomQuery,
}

impl CompiledFilter {
    pub fn new(model: FilterModel) -> Self {
        let normalized_text = normalize_text(&model.text);
        let normalized_camera = model.camera.as_deref().map(normalize_text);
        let normalized_lens = model.lens.as_deref().map(normalize_text);
        let content_query = BloomQuery::new(&model.text);
        Self {
            model,
            normalized_text,
            normalized_camera,
            normalized_lens,
            content_query,
        }
    }

    pub fn matches_record<'a>(&self, record: &'a SegmentRecord) -> Option<FilterHit<'a>> {
        if !self.matches_non_text_predicates(record) {
            return None;
        }

        let mut score = 0;
        let mut content_candidate = false;
        if !self.normalized_text.is_empty() {
            let name_score =
                fuzzy_score_normalized(&normalize_text(&record.name), &self.normalized_text);
            let content_match = self.model.content
                && record
                    .content_signature
                    .as_ref()
                    .is_some_and(|signature| signature.might_contain(&self.content_query));

            match (name_score, content_match) {
                (Some(name_score), true) => {
                    score = name_score.saturating_add(80);
                    content_candidate = true;
                }
                (Some(name_score), false) => {
                    score = name_score;
                }
                (None, true) => {
                    score = 120;
                    content_candidate = true;
                }
                (None, false) => return None,
            }
        }

        Some(FilterHit {
            record,
            score,
            content_candidate,
        })
    }

    fn matches_non_text_predicates(&self, record: &SegmentRecord) -> bool {
        if !self.model.kinds.is_empty() && !self.model.kinds.contains(&record.kind) {
            return false;
        }
        if let Some(range) = &self.model.date {
            let date_secs = record.capture_secs.unwrap_or(record.mtime_secs);
            if !range.contains(date_secs) {
                return false;
            }
        }
        if let Some(range) = &self.model.size {
            if !range.contains(record.size) {
                return false;
            }
        }
        if let Some(camera) = &self.normalized_camera {
            if record
                .camera
                .as_deref()
                .is_none_or(|value| normalize_text(value) != *camera)
            {
                return false;
            }
        }
        if let Some(lens) = &self.normalized_lens {
            if record
                .lens
                .as_deref()
                .is_none_or(|value| normalize_text(value) != *lens)
            {
                return false;
            }
        }
        matches_gps_filter(record, &self.model.gps)
    }
}

pub fn filter_segment<'a>(segment: &'a Segment, model: FilterModel) -> Vec<FilterHit<'a>> {
    let compiled = CompiledFilter::new(model);
    let mut hits = segment
        .records()
        .iter()
        .filter_map(|record| compiled.matches_record(record))
        .collect::<Vec<_>>();
    hits.sort_by(|lhs, rhs| {
        rhs.score
            .cmp(&lhs.score)
            .then_with(|| lhs.record.path.cmp(&rhs.record.path))
    });
    hits
}

pub fn fuzzy_name_score(candidate: &str, query: &str) -> Option<u32> {
    fuzzy_score_normalized(&normalize_text(candidate), &normalize_text(query))
}

pub fn facets_for_records<'a>(records: impl IntoIterator<Item = &'a SegmentRecord>) -> Facets {
    let mut facets = Facets::default();
    for record in records {
        *facets.kinds.entry(record.kind).or_insert(0) += 1;
        if let Some(camera) = record.camera.as_deref().filter(|value| !value.is_empty()) {
            *facets.cameras.entry(camera.to_string()).or_insert(0) += 1;
        }
        if let Some(lens) = record.lens.as_deref().filter(|value| !value.is_empty()) {
            *facets.lenses.entry(lens.to_string()).or_insert(0) += 1;
        }
        match (record.gps_lat, record.gps_lon) {
            (Some(lat), Some(lon)) => {
                facets.gps_present += 1;
                facets.gps_points.push((lat, lon));
            }
            _ => facets.gps_absent += 1,
        }
    }
    facets
}

pub fn verify_content_match(
    text: &str,
    query: &str,
    context_chars: usize,
) -> Option<ContentSnippet> {
    let normalized_query = normalize_text(query);
    if normalized_query.is_empty() {
        return None;
    }

    find_case_insensitive(text, query)
        .or_else(|| find_case_insensitive(text, &normalized_query))
        .or_else(|| {
            let normalized_text = normalize_text(text);
            normalized_text
                .contains(&normalized_query)
                .then_some(0..text.len().min(first_n_chars_end(text, context_chars * 2)))
        })
        .map(|range| ContentSnippet {
            snippet: snippet_around(text, range.clone(), context_chars),
            match_byte_range: range,
        })
}

fn matches_gps_filter(record: &SegmentRecord, filter: &GpsFilter) -> bool {
    let gps = record.gps_lat.zip(record.gps_lon);
    match filter {
        GpsFilter::Any => true,
        GpsFilter::Present => gps.is_some(),
        GpsFilter::Absent => gps.is_none(),
        GpsFilter::Bounds(bounds) => gps.is_some_and(|(lat, lon)| bounds.contains(lat, lon)),
    }
}

fn fuzzy_score_normalized(candidate: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    if candidate == query {
        return Some(1_000);
    }
    if let Some(byte_pos) = candidate.find(query) {
        let char_pos = candidate[..byte_pos].chars().count() as u32;
        return Some(850u32.saturating_sub(char_pos.min(300)));
    }

    let mut score = 0u32;
    let mut query_chars = query.chars().peekable();
    let mut previous_match_index = None;
    for (index, ch) in candidate.chars().enumerate() {
        if query_chars.peek().is_some_and(|query_ch| *query_ch == ch) {
            score = score.saturating_add(40);
            if previous_match_index.is_some_and(|previous| previous + 1 == index) {
                score = score.saturating_add(25);
            }
            if index == 0 {
                score = score.saturating_add(20);
            }
            previous_match_index = Some(index);
            query_chars.next();
            if query_chars.peek().is_none() {
                let length_penalty = candidate
                    .chars()
                    .count()
                    .saturating_sub(query.chars().count())
                    as u32;
                return Some(score.saturating_sub(length_penalty.min(80)));
            }
        }
    }
    None
}

fn find_case_insensitive(text: &str, query: &str) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    text_lower
        .find(&query_lower)
        .map(|start| start..start + query_lower.len())
}

fn snippet_around(text: &str, range: std::ops::Range<usize>, context_chars: usize) -> String {
    let start = previous_char_boundary(text, range.start, context_chars);
    let end = next_char_boundary(text, range.end, context_chars);
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(text[start..end].trim());
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

fn previous_char_boundary(text: &str, byte_index: usize, max_chars: usize) -> usize {
    text[..byte_index]
        .char_indices()
        .rev()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, byte_index: usize, max_chars: usize) -> usize {
    text[byte_index..]
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| byte_index + index)
        .unwrap_or(text.len())
}

fn first_n_chars_end(text: &str, max_chars: usize) -> usize {
    text.char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloom::BloomSignature;

    fn record(path: &str) -> SegmentRecord {
        SegmentRecord {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            size: 100,
            mtime_secs: 1_700_000_000,
            kind: IndexedKind::File,
            width: None,
            height: None,
            capture_secs: None,
            capture_offset_min: None,
            camera: None,
            lens: None,
            gps_lat: None,
            gps_lon: None,
            content_signature: None,
        }
    }

    #[test]
    fn fuzzy_score_matches_subsequence_with_kana_folding() {
        let segment = Segment::new(vec![
            record("/root/カメラ写真.jpg"),
            record("/root/memo.txt"),
        ]);
        let hits = filter_segment(
            &segment,
            FilterModel {
                text: "かめら".to_string(),
                ..Default::default()
            },
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.name, "カメラ写真.jpg");
    }

    #[test]
    fn fuzzy_name_score_normalizes_width_case_and_kana() {
        assert!(fuzzy_name_score("Ｆｏｏカメラ.jpg", "fooかめ").is_some());
        assert!(fuzzy_name_score("invoice.pdf", "ivp").is_some());
        assert!(fuzzy_name_score("invoice.pdf", "zzz").is_none());
    }

    #[test]
    fn filters_by_metadata_predicates_and_falls_back_to_mtime_date() {
        let mut jpg = record("/root/photo.jpg");
        jpg.kind = IndexedKind::Image;
        jpg.size = 10_000;
        jpg.mtime_secs = 100;
        jpg.capture_secs = Some(50);
        jpg.camera = Some("Canon".to_string());
        jpg.lens = Some("RF 35mm".to_string());
        jpg.gps_lat = Some(35.0);
        jpg.gps_lon = Some(139.0);
        let mut pdf = record("/root/doc.pdf");
        pdf.kind = IndexedKind::Pdf;
        pdf.mtime_secs = 75;
        let segment = Segment::new(vec![jpg, pdf]);

        let hits = filter_segment(
            &segment,
            FilterModel {
                kinds: vec![IndexedKind::Image],
                date: Some(RangeFilter {
                    min: Some(40),
                    max: Some(60),
                }),
                size: Some(RangeFilter {
                    min: Some(1_000),
                    max: Some(20_000),
                }),
                camera: Some("canon".to_string()),
                lens: Some("rf 35mm".to_string()),
                gps: GpsFilter::Bounds(GpsBounds {
                    min_lat: 34.0,
                    max_lat: 36.0,
                    min_lon: 138.0,
                    max_lon: 140.0,
                }),
                ..Default::default()
            },
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.name, "photo.jpg");
    }

    #[test]
    fn content_enabled_uses_bloom_signature_as_candidate() {
        let mut pdf = record("/root/scan.pdf");
        pdf.kind = IndexedKind::Pdf;
        pdf.content_signature = Some(BloomSignature::from_content("日本語 請求書 明細"));
        let segment = Segment::new(vec![pdf]);

        let hits = filter_segment(
            &segment,
            FilterModel {
                text: "請求書".to_string(),
                content: true,
                ..Default::default()
            },
        );

        assert_eq!(hits.len(), 1);
        assert!(hits[0].content_candidate);
    }

    #[test]
    fn verify_content_match_returns_context_snippet() {
        let snippet =
            verify_content_match("前文 日本語の請求書明細 後文", "請求書", 4).expect("snippet");

        assert_eq!(snippet.snippet, "…日本語の請求書明細 後…");
    }

    #[test]
    fn facets_count_available_values_and_gps_points() {
        let mut with_gps = record("/root/a.jpg");
        with_gps.kind = IndexedKind::Image;
        with_gps.camera = Some("Nikon".to_string());
        with_gps.gps_lat = Some(35.5);
        with_gps.gps_lon = Some(139.5);
        let without_gps = record("/root/b.pdf");

        let facets = facets_for_records([&with_gps, &without_gps]);

        assert_eq!(facets.kinds.get(&IndexedKind::Image), Some(&1));
        assert_eq!(facets.cameras.get("Nikon"), Some(&1));
        assert_eq!(facets.gps_present, 1);
        assert_eq!(facets.gps_absent, 1);
        assert_eq!(facets.gps_points, vec![(35.5, 139.5)]);
    }
}
