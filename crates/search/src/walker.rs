use crate::segment::{IndexedKind, SegmentRecord};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkOptions {
    pub recursive: bool,
    pub include_hidden: bool,
    pub max_entries: Option<usize>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            recursive: true,
            include_hidden: false,
            max_entries: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkStats {
    pub visited_dirs: usize,
    pub skipped_hidden: usize,
    pub skipped_symlinks: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WalkResult {
    pub records: Vec<SegmentRecord>,
    pub stats: WalkStats,
}

pub fn walk_local<C>(
    root: &Path,
    options: &WalkOptions,
    mut is_cancelled: C,
) -> std::io::Result<WalkResult>
where
    C: FnMut() -> bool,
{
    let mut result = WalkResult {
        records: Vec::new(),
        stats: WalkStats::default(),
    };
    walk_dir(root, options, &mut result, &mut is_cancelled)?;
    result.records.sort_by(|lhs, rhs| lhs.path.cmp(&rhs.path));
    Ok(result)
}

fn walk_dir<C>(
    root: &Path,
    options: &WalkOptions,
    result: &mut WalkResult,
    is_cancelled: &mut C,
) -> std::io::Result<()>
where
    C: FnMut() -> bool,
{
    if result.stats.truncated || result.stats.cancelled {
        return Ok(());
    }
    if is_cancelled() {
        result.stats.cancelled = true;
        return Ok(());
    }
    result.stats.visited_dirs += 1;
    for entry in fs::read_dir(root)? {
        if is_cancelled() {
            result.stats.cancelled = true;
            return Ok(());
        }
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        if !options.include_hidden && is_hidden(&file_name.to_string_lossy()) {
            result.stats.skipped_hidden += 1;
            continue;
        }

        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            result.stats.skipped_symlinks += 1;
            continue;
        }

        let is_dir = metadata.is_dir();
        result.records.push(record_from_metadata(&path, &metadata));
        if reached_limit(options, result) {
            result.stats.truncated = true;
            return Ok(());
        }
        if is_dir && options.recursive {
            walk_dir(&path, options, result, is_cancelled)?;
        }
    }
    Ok(())
}

fn reached_limit(options: &WalkOptions, result: &WalkResult) -> bool {
    options
        .max_entries
        .is_some_and(|max_entries| result.records.len() >= max_entries)
}

fn record_from_metadata(path: &Path, metadata: &fs::Metadata) -> SegmentRecord {
    let path_string = path.to_string_lossy().to_string();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path_string.clone());
    let kind = IndexedKind::for_path(path, metadata.is_dir());
    SegmentRecord {
        path: path_string,
        name,
        size: if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
        mtime_secs: system_time_to_secs(metadata.modified().unwrap_or(UNIX_EPOCH)),
        kind,
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

pub fn system_time_to_secs(value: SystemTime) -> i64 {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().min(i64::MAX as u64) as i64,
        Err(error) => -(error.duration().as_secs().min(i64::MAX as u64) as i64),
    }
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_local_classifies_and_sorts_recursive_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("nested")).expect("nested");
        fs::write(dir.path().join("b.pdf"), b"pdf").expect("pdf");
        fs::write(dir.path().join("a.jpg"), b"jpg").expect("jpg");
        fs::write(dir.path().join("nested/c.mp4"), b"video").expect("video");

        let result = walk_local(dir.path(), &WalkOptions::default(), || false).expect("walk");
        let names = result
            .records
            .iter()
            .map(|record| (record.name.as_str(), record.kind))
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                ("a.jpg", IndexedKind::Image),
                ("b.pdf", IndexedKind::Pdf),
                ("nested", IndexedKind::Directory),
                ("c.mp4", IndexedKind::Video),
            ]
        );
        assert_eq!(result.stats.visited_dirs, 2);
    }

    #[test]
    fn walk_local_classifies_modern_images() {
        let dir = tempfile::tempdir().expect("tempdir");
        for extension in ["avif", "heic", "jp2", "jxr", "jxl"] {
            fs::write(dir.path().join(format!("sample.{extension}")), b"fixture")
                .expect("modern image");
        }

        let result = walk_local(dir.path(), &WalkOptions::default(), || false).expect("walk");
        assert_eq!(result.records.len(), 5);
        assert!(result
            .records
            .iter()
            .all(|record| record.kind == IndexedKind::Image));
    }

    #[test]
    fn walk_local_skips_hidden_and_respects_non_recursive_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("nested")).expect("nested");
        fs::write(dir.path().join(".secret.pdf"), b"hidden").expect("hidden");
        fs::write(dir.path().join("nested/c.pdf"), b"nested").expect("nested file");

        let result = walk_local(
            dir.path(),
            &WalkOptions {
                recursive: false,
                include_hidden: false,
                max_entries: None,
            },
            || false,
        )
        .expect("walk");

        assert_eq!(
            result
                .records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            vec!["nested"]
        );
        assert_eq!(result.stats.skipped_hidden, 1);
        assert_eq!(result.stats.visited_dirs, 1);
    }

    #[test]
    fn walk_local_stops_at_entry_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.txt"), b"a").expect("a");
        fs::write(dir.path().join("b.txt"), b"b").expect("b");

        let result = walk_local(
            dir.path(),
            &WalkOptions {
                max_entries: Some(1),
                ..Default::default()
            },
            || false,
        )
        .expect("walk");

        assert_eq!(result.records.len(), 1);
        assert!(result.stats.truncated);
        assert!(!result.stats.cancelled);
    }

    #[test]
    fn walk_local_can_cancel_before_reading_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.txt"), b"a").expect("a");

        let result = walk_local(dir.path(), &WalkOptions::default(), || true).expect("walk");

        assert!(result.records.is_empty());
        assert_eq!(result.stats.visited_dirs, 0);
        assert!(result.stats.cancelled);
        assert!(!result.stats.truncated);
    }

    #[test]
    fn walk_local_can_cancel_between_directory_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.txt"), b"a").expect("a");
        fs::write(dir.path().join("b.txt"), b"b").expect("b");
        let mut checks = 0;

        let result = walk_local(dir.path(), &WalkOptions::default(), || {
            checks += 1;
            checks > 1
        })
        .expect("walk");

        assert!(result.records.is_empty());
        assert_eq!(result.stats.visited_dirs, 1);
        assert!(result.stats.cancelled);
        assert!(!result.stats.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn walk_local_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        fs::write(outside.path().join("outside.pdf"), b"outside").expect("outside");
        symlink(outside.path(), dir.path().join("outside-link")).expect("symlink");

        let result = walk_local(dir.path(), &WalkOptions::default(), || false).expect("walk");

        assert!(result.records.is_empty());
        assert_eq!(result.stats.skipped_symlinks, 1);
    }
}
