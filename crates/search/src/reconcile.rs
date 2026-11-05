use crate::segment::{Segment, SegmentRecord};
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
    let existing_by_path = existing
        .records()
        .iter()
        .map(|record| (record.path.as_str(), record))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let mut stats = ReconcileStats::default();

    for record in scanned {
        seen.insert(record.path.clone());
        match existing_by_path.get(record.path.as_str()) {
            Some(previous) if record_is_fresh(previous, &record) => {
                records.push((*previous).clone());
                stats.unchanged += 1;
            }
            Some(_) => {
                records.push(record);
                stats.updated += 1;
            }
            None => {
                records.push(record);
                stats.inserted += 1;
            }
        }
    }

    stats.removed = existing
        .records()
        .iter()
        .filter(|record| !seen.contains(&record.path))
        .count();
    records.sort_by(|a, b| a.path.cmp(&b.path));

    ReconcileResult {
        segment: Segment::new(records),
        stats,
    }
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
}
