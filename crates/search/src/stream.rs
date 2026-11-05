use crate::filter::{CompiledFilter, FilterHit, FilterModel};
use crate::segment::{Segment, SegmentRecord};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterStats {
    pub scanned: usize,
    pub matched: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterEvent<'a> {
    Started { total_hint: Option<usize> },
    Hits(Vec<FilterHit<'a>>),
    Progress(FilterStats),
    Finished(FilterStats),
    Cancelled(FilterStats),
}

pub fn filter_segment_incremental<'a, C>(
    segment: &'a Segment,
    model: FilterModel,
    batch_size: usize,
    is_cancelled: C,
) -> Vec<FilterEvent<'a>>
where
    C: FnMut() -> bool,
{
    filter_records_incremental(
        segment.records().iter(),
        Some(segment.records().len()),
        model,
        batch_size,
        is_cancelled,
    )
}

pub fn filter_records_incremental<'a, I, C>(
    records: I,
    total_hint: Option<usize>,
    model: FilterModel,
    batch_size: usize,
    mut is_cancelled: C,
) -> Vec<FilterEvent<'a>>
where
    I: IntoIterator<Item = &'a SegmentRecord>,
    C: FnMut() -> bool,
{
    let compiled = CompiledFilter::new(model);
    let batch_size = batch_size.max(1);
    let mut events = vec![FilterEvent::Started { total_hint }];
    let mut stats = FilterStats::default();
    let mut batch = Vec::with_capacity(batch_size);

    for record in records {
        if is_cancelled() {
            flush_hits(&mut events, &mut batch);
            events.push(FilterEvent::Cancelled(stats));
            return events;
        }
        stats.scanned += 1;
        if let Some(hit) = compiled.matches_record(record) {
            stats.matched += 1;
            batch.push(hit);
            if batch.len() >= batch_size {
                flush_hits(&mut events, &mut batch);
                events.push(FilterEvent::Progress(stats.clone()));
            }
        }
    }

    flush_hits(&mut events, &mut batch);
    events.push(FilterEvent::Finished(stats));
    events
}

fn flush_hits<'a>(events: &mut Vec<FilterEvent<'a>>, batch: &mut Vec<FilterHit<'a>>) {
    if !batch.is_empty() {
        batch.sort_by(|lhs, rhs| {
            rhs.score
                .cmp(&lhs.score)
                .then_with(|| lhs.record.path.cmp(&rhs.record.path))
        });
        events.push(FilterEvent::Hits(std::mem::take(batch)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{IndexedKind, SegmentRecord};

    fn record(path: &str) -> SegmentRecord {
        SegmentRecord {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            size: 1,
            mtime_secs: 1,
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
    fn filter_segment_incremental_batches_hits_and_progress() {
        let segment = Segment::new(vec![
            record("/root/camera.txt"),
            record("/root/cat.txt"),
            record("/root/dog.txt"),
        ]);

        let events = filter_segment_incremental(
            &segment,
            FilterModel {
                text: "ca".to_string(),
                ..Default::default()
            },
            1,
            || false,
        );

        assert!(matches!(
            events.first(),
            Some(FilterEvent::Started {
                total_hint: Some(3)
            })
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, FilterEvent::Hits(_)))
                .count(),
            2
        );
        assert_eq!(
            events.last(),
            Some(&FilterEvent::Finished(FilterStats {
                scanned: 3,
                matched: 2,
            }))
        );
    }

    #[test]
    fn filter_records_incremental_stops_when_cancelled() {
        let records = [record("/root/a.txt"), record("/root/b.txt")];
        let mut calls = 0;
        let events = filter_records_incremental(
            records.iter(),
            Some(records.len()),
            FilterModel::default(),
            50,
            || {
                calls += 1;
                calls > 1
            },
        );

        assert_eq!(
            events.last(),
            Some(&FilterEvent::Cancelled(FilterStats {
                scanned: 1,
                matched: 1,
            }))
        );
    }
}
