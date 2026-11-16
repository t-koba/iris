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
) -> impl Iterator<Item = FilterEvent<'a>> + 'a
where
    C: FnMut() -> bool + 'a,
{
    filter_records_incremental(
        segment.records().iter(),
        Some(segment.records().len()),
        model,
        batch_size,
        is_cancelled,
    )
}

/// Scan only as events are requested, buffering at most one batch of hits.
pub fn filter_records_incremental<'a, I, C>(
    records: I,
    total_hint: Option<usize>,
    model: FilterModel,
    batch_size: usize,
    mut is_cancelled: C,
) -> impl Iterator<Item = FilterEvent<'a>> + 'a
where
    I: IntoIterator<Item = &'a SegmentRecord> + 'a,
    C: FnMut() -> bool + 'a,
{
    let mut records = records.into_iter();
    let compiled = CompiledFilter::new(model);
    let batch_size = batch_size.max(1);
    let mut started = false;
    let mut finished = false;
    let mut pending = None;
    let mut stats = FilterStats::default();

    std::iter::from_fn(move || {
        if !started {
            started = true;
            return Some(FilterEvent::Started { total_hint });
        }
        if let Some(event) = pending.take() {
            return Some(event);
        }
        if finished {
            return None;
        }

        let mut batch = Vec::with_capacity(batch_size);
        let terminal = loop {
            let Some(record) = records.next() else {
                break FilterEvent::Finished(stats.clone());
            };
            if is_cancelled() {
                break FilterEvent::Cancelled(stats.clone());
            }
            stats.scanned += 1;
            if let Some(hit) = compiled.matches_record(record) {
                stats.matched += 1;
                batch.push(hit);
                if batch.len() >= batch_size {
                    pending = Some(FilterEvent::Progress(stats.clone()));
                    return Some(sorted_hits(batch));
                }
            }
        };

        finished = true;
        if batch.is_empty() {
            Some(terminal)
        } else {
            pending = Some(terminal);
            Some(sorted_hits(batch))
        }
    })
}

fn sorted_hits(mut batch: Vec<FilterHit<'_>>) -> FilterEvent<'_> {
    batch.sort_by(|lhs, rhs| {
        rhs.score
            .cmp(&lhs.score)
            .then_with(|| lhs.record.path.cmp(&rhs.record.path))
    });
    FilterEvent::Hits(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{IndexedKind, SegmentRecord};
    use std::cell::Cell;

    fn record(path: &str) -> SegmentRecord {
        SegmentRecord {
            native_locator: None,
            local_path_is_lossy: false,
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

        let events: Vec<_> = filter_segment_incremental(
            &segment,
            FilterModel {
                text: "ca".to_string(),
                ..Default::default()
            },
            1,
            || false,
        )
        .collect();

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
    fn filter_records_incremental_yields_hits_before_exhausting_source() {
        let records = [
            record("/root/c.txt"),
            record("/root/b.txt"),
            record("/root/a.txt"),
            record("/root/d.txt"),
            record("/root/e.txt"),
        ];
        let consumed = Cell::new(0);
        let source = records.iter().inspect(|_| consumed.set(consumed.get() + 1));
        let mut events = filter_records_incremental(
            source,
            Some(records.len()),
            FilterModel::default(),
            2,
            || false,
        );

        assert_eq!(consumed.get(), 0);
        assert_eq!(
            events.next(),
            Some(FilterEvent::Started {
                total_hint: Some(5),
            })
        );
        assert_eq!(consumed.get(), 0);
        let Some(FilterEvent::Hits(hits)) = events.next() else {
            panic!("expected the first batch");
        };
        assert_eq!(
            hits.iter()
                .map(|hit| hit.record.path.as_str())
                .collect::<Vec<_>>(),
            ["/root/b.txt", "/root/c.txt"]
        );
        assert_eq!(consumed.get(), 2);
        assert_eq!(
            events.next(),
            Some(FilterEvent::Progress(FilterStats {
                scanned: 2,
                matched: 2,
            }))
        );
        assert_eq!(consumed.get(), 2);

        let remaining: Vec<_> = events.collect();
        assert_eq!(consumed.get(), records.len());
        assert!(matches!(&remaining[2], FilterEvent::Hits(hits) if hits.len() == 1));
        assert_eq!(
            remaining.last(),
            Some(&FilterEvent::Finished(FilterStats {
                scanned: 5,
                matched: 5,
            }))
        );
    }

    #[test]
    fn dropping_filter_events_stops_consuming_records() {
        let records = [record("/root/a.txt"), record("/root/b.txt")];
        for stop_after_hits in [false, true] {
            let consumed = Cell::new(0);
            let source = records.iter().inspect(|_| consumed.set(consumed.get() + 1));
            let mut events =
                filter_records_incremental(source, None, FilterModel::default(), 1, || false);
            assert!(matches!(events.next(), Some(FilterEvent::Started { .. })));
            if stop_after_hits {
                assert!(matches!(events.next(), Some(FilterEvent::Hits(_))));
            }
            drop(events);
            assert_eq!(consumed.get(), usize::from(stop_after_hits));
        }
    }

    #[test]
    fn filter_records_incremental_stops_when_cancelled() {
        let records = [record("/root/a.txt"), record("/root/b.txt")];
        let mut calls = 0;
        let events: Vec<_> = filter_records_incremental(
            records.iter(),
            Some(records.len()),
            FilterModel::default(),
            50,
            || {
                calls += 1;
                calls > 1
            },
        )
        .collect();

        assert!(matches!(&events[1], FilterEvent::Hits(hits)
            if hits.len() == 1 && hits[0].record.path == "/root/a.txt"));
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.last(),
            Some(&FilterEvent::Cancelled(FilterStats {
                scanned: 1,
                matched: 1,
            }))
        );
    }
}
