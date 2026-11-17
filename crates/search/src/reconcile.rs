use crate::segment::{Segment, SegmentRecord};
use iris_core_domain::path_identity::equality_key;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileStats {
    pub inserted: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReconcileResult {
    pub segment: Segment,
    pub stats: ReconcileStats,
}

pub fn reconcile_segment(
    existing: &Segment,
    scanned: impl IntoIterator<Item = SegmentRecord>,
) -> ReconcileResult {
    // Exact paths win so two genuinely distinct files that merely normalize
    // alike both survive. Normalized identity is only a fallback for matching
    // an original record when the same file is reported under a respelled path.
    let mut existing_by_exact = HashMap::new();
    let mut existing_by_equality = HashMap::new();
    for (index, record) in existing.records().iter().enumerate() {
        existing_by_exact
            .entry(exact_identity(record))
            .or_insert_with(Vec::new)
            .push(index);
        existing_by_equality
            .entry(normalized_identity(record))
            .or_insert_with(Vec::new)
            .push(index);
    }

    let mut matched = HashSet::new();
    let mut records = Vec::new();
    let mut stats = ReconcileStats::default();

    for record in scanned {
        let exact = exact_identity(&record);
        let normalized = normalized_identity(&record);
        let previous_index = existing_by_exact
            .get(&exact)
            .and_then(|indices| indices.iter().find(|index| !matched.contains(*index)))
            .copied()
            .or_else(|| {
                existing_by_equality
                    .get(&normalized)
                    .and_then(|indices| indices.iter().find(|index| !matched.contains(*index)))
                    .copied()
            });

        match previous_index {
            Some(index) => {
                matched.insert(index);
                let previous = &existing.records()[index];
                if record_is_fresh(previous, &record) {
                    records.push(previous.clone());
                    stats.unchanged += 1;
                } else {
                    records.push(record);
                    stats.updated += 1;
                }
            }
            None => {
                records.push(record);
                stats.inserted += 1;
            }
        }
    }

    stats.removed = existing.records().len() - matched.len();
    records.sort_by(|a, b| a.path.cmp(&b.path));

    ReconcileResult {
        segment: Segment::new(records),
        stats,
    }
}

type RecordIdentity = (String, bool, Option<String>);

fn exact_identity(record: &SegmentRecord) -> RecordIdentity {
    (
        record.path.clone(),
        record.local_path_is_lossy,
        record.native_locator.clone(),
    )
}

fn normalized_identity(record: &SegmentRecord) -> RecordIdentity {
    (
        equality_key(&record.path),
        record.local_path_is_lossy,
        record.native_locator.clone(),
    )
}

fn record_is_fresh(previous: &SegmentRecord, scanned: &SegmentRecord) -> bool {
    previous.size == scanned.size
        && previous.mtime_secs == scanned.mtime_secs
        && previous.kind == scanned.kind
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{IndexedKind, SegmentRecord};

    fn record(path: &str, size: u64, mtime_secs: i64) -> SegmentRecord {
        SegmentRecord {
            native_locator: None,
            local_path_is_lossy: false,
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            size,
            mtime_secs,
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
    fn reconcile_path_identity_preserves_original_records_and_platform_policy() {
        let original = record("/root/Café.jpg", 10, 100);
        let existing = Segment::new(vec![original.clone()]);
        let scanned = record("/root/Cafe\u{301}.jpg", 10, 100);
        let result = reconcile_segment(&existing, [scanned.clone()]);
        if cfg!(target_os = "macos") {
            assert_eq!(
                result.stats,
                ReconcileStats {
                    unchanged: 1,
                    ..Default::default()
                }
            );
            assert_eq!(result.segment.records(), &[original]);
        } else {
            assert_eq!(
                result.stats,
                ReconcileStats {
                    inserted: 1,
                    removed: 1,
                    ..Default::default()
                }
            );
            assert_eq!(result.segment.records(), &[scanned]);
        }
    }

    #[test]
    fn reconcile_reuses_fresh_records_and_reports_changes() {
        let existing = Segment::new(vec![
            record("/root/a.txt", 10, 100),
            record("/root/b.txt", 20, 200),
            record("/root/remove.txt", 30, 300),
        ]);
        let result = reconcile_segment(
            &existing,
            vec![
                record("/root/a.txt", 10, 100),
                record("/root/b.txt", 21, 201),
                record("/root/c.txt", 40, 400),
            ],
        );

        assert_eq!(
            result.stats,
            ReconcileStats {
                inserted: 1,
                updated: 1,
                removed: 1,
                unchanged: 1,
            }
        );
        assert_eq!(
            result
                .segment
                .records()
                .iter()
                .map(|record| record.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/root/a.txt", "/root/b.txt", "/root/c.txt"]
        );
    }

    #[test]
    fn reconcile_preserves_distinct_records_with_equivalent_paths() {
        // On a normalization-sensitive share mounted from macOS, both names
        // can exist at once even though their equality keys are identical.
        let first = record("/Volumes/share/Café.jpg", 10, 100);
        let second = record("/Volumes/share/Cafe\u{301}.jpg", 10, 100);
        let existing = Segment::new(vec![first.clone(), second.clone()]);

        let result = reconcile_segment(&existing, [first.clone(), second.clone()]);
        let mut expected = vec![first.clone(), second.clone()];
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(result.segment.records(), expected.as_slice());
        assert_eq!(
            result.stats,
            ReconcileStats {
                unchanged: 2,
                ..Default::default()
            }
        );

        let deleted = reconcile_segment(&existing, [first.clone()]);
        assert_eq!(deleted.segment.records(), &[first]);
        assert_eq!(
            deleted.stats,
            ReconcileStats {
                unchanged: 1,
                removed: 1,
                ..Default::default()
            }
        );
    }
}
