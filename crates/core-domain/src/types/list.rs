use crate::types::metadata::MetadataLevel;
use crate::types::object::ObjectRef;
use std::path::Path;
use std::time::SystemTime;

/// Opaque pagination cursor: a continuation token for cloud backends, an offset for LocalFS.
#[derive(Clone, Debug)]
pub struct ListCursor(pub String);

/// Stage A listing entry: only the cheap fields needed for the first paint.
#[derive(Clone, Debug)]
pub struct EntryNameOnly {
    pub object_ref: ObjectRef,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_inaccessible: bool,
}

/// Entry for backends whose list response already includes metadata.
#[derive(Clone, Debug)]
pub struct EntryWithMeta {
    pub name: EntryNameOnly,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
}

/// Local and OS-mounted backends return `NameOnly`; cloud backends with metadata in list responses return `WithMeta`.
#[derive(Clone, Debug)]
pub enum ListedEntry {
    NameOnly(EntryNameOnly),
    WithMeta(EntryWithMeta),
}

impl ListedEntry {
    pub fn name(&self) -> &EntryNameOnly {
        match self {
            Self::NameOnly(entry) => entry,
            Self::WithMeta(entry) => &entry.name,
        }
    }

    pub fn metadata_level(&self) -> MetadataLevel {
        match self {
            Self::NameOnly(_) => MetadataLevel::L0,
            Self::WithMeta(_) => MetadataLevel::L1,
        }
    }
}

/// Listing entry; L0/L1 data alone is enough to start rendering.
#[derive(Clone, Debug)]
pub struct ListEntry {
    pub object_ref: ObjectRef,
    pub name: String,
    /// Lowercase key used for repeated name sorting without reallocating.
    pub sort_name_key: String,
    /// Lowercase extension key used for repeated file type sorting without path parsing.
    pub sort_extension_key: String,
    pub is_dir: bool,
    /// Metadata completeness for the fields below.
    pub metadata_level: MetadataLevel,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
    pub is_symlink: bool,
    /// Symlink target, when readable.
    pub symlink_target: Option<String>,
    /// Metadata could not be read (e.g. permission denied); is_dir / size / mtime may be inaccurate.
    pub is_inaccessible: bool,
    /// Extra display text added by filters and search.
    pub annotation: Option<String>,
}

impl ListEntry {
    /// An entry known only by name (metadata level L0).
    pub fn new(object_ref: ObjectRef, name: String, is_dir: bool) -> Self {
        Self {
            object_ref,
            sort_name_key: Self::sort_name_key_for(&name),
            sort_extension_key: Self::sort_extension_key_for(&name),
            name,
            is_dir,
            metadata_level: MetadataLevel::L0,
            size: None,
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
            annotation: None,
        }
    }

    pub fn from_listed_entry(entry: ListedEntry) -> Self {
        match entry {
            ListedEntry::NameOnly(entry) => Self::from_name_entry(entry),
            ListedEntry::WithMeta(entry) => Self::from_entry_with_meta(entry),
        }
    }

    pub fn from_name_entry(entry: EntryNameOnly) -> Self {
        let EntryNameOnly {
            object_ref,
            name,
            is_dir,
            is_symlink,
            is_inaccessible,
        } = entry;
        Self {
            is_symlink,
            is_inaccessible,
            ..Self::new(object_ref, name, is_dir)
        }
    }

    pub fn from_entry_with_meta(entry: EntryWithMeta) -> Self {
        let EntryWithMeta {
            name,
            size,
            mtime,
            ctime,
        } = entry;
        let mut list_entry = Self::from_name_entry(name);
        list_entry.metadata_level = MetadataLevel::L1;
        list_entry.size = size;
        list_entry.mtime = mtime;
        list_entry.ctime = ctime;
        list_entry
    }

    pub fn sort_name_key_for(name: &str) -> String {
        name.to_lowercase()
    }

    pub fn sort_extension_key_for(name: &str) -> String {
        Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default()
    }

    pub fn metadata_pending(&self) -> bool {
        self.metadata_level == MetadataLevel::L0 && !self.is_inaccessible
    }
}

#[derive(Debug)]
pub struct ListPage {
    pub entries: Vec<ListedEntry>,
    /// `None` means this is the last page.
    pub next_cursor: Option<ListCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::object::{BackendId, ObjectKey, StableId};

    fn object_ref(path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("test".to_string()),
                stable_id: StableId(path.to_string()),
            },
            parent_stable_id: None,
            display_path: path.to_string(),
        }
    }

    fn name_entry(path: &str, is_dir: bool) -> EntryNameOnly {
        EntryNameOnly {
            object_ref: object_ref(path),
            name: path.to_string(),
            is_dir,
            is_symlink: false,
            is_inaccessible: false,
        }
    }

    #[test]
    fn name_only_converts_to_pending_display_entry() {
        let entry =
            ListEntry::from_listed_entry(ListedEntry::NameOnly(name_entry("image.JPG", false)));

        assert_eq!(entry.metadata_level, MetadataLevel::L0);
        assert!(entry.metadata_pending());
        assert_eq!(entry.size, None);
        assert_eq!(entry.sort_name_key, "image.jpg");
        assert_eq!(entry.sort_extension_key, "jpg");
    }

    #[test]
    fn entry_with_meta_converts_to_ready_display_entry() {
        let mtime = SystemTime::UNIX_EPOCH;
        let entry = ListEntry::from_listed_entry(ListedEntry::WithMeta(EntryWithMeta {
            name: name_entry("folder", true),
            size: Some(42),
            mtime: Some(mtime),
            ctime: None,
        }));

        assert_eq!(entry.metadata_level, MetadataLevel::L1);
        assert!(!entry.metadata_pending());
        assert_eq!(entry.size, Some(42));
        assert_eq!(entry.mtime, Some(mtime));
        assert!(entry.is_dir);
    }
}
