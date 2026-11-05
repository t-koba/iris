//! Builders shared by the unit tests.

use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};

pub(crate) fn local_object_ref(path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("local".to_string()),
            stable_id: StableId(path.to_string()),
        },
        parent_stable_id: None,
        display_path: path.to_string(),
    }
}

/// An L1 entry for a local file or directory; the name is the last path component.
pub(crate) fn local_entry(path: &str, is_dir: bool) -> ListEntry {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    ListEntry {
        metadata_level: MetadataLevel::L1,
        ..ListEntry::new(local_object_ref(path), name, is_dir)
    }
}

/// A file entry in `/tmp`.
pub(crate) fn tmp_entry(name: &str) -> ListEntry {
    local_entry(&format!("/tmp/{name}"), false)
}
