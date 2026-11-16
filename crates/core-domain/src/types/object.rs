use serde::{Deserialize, Serialize};
use std::fmt;

/// Identifies a connection target; different servers of the same protocol get different ids.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackendId(pub String);

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Object identity within a connection: a backend file id (inode, cloud file id, ...)
/// that survives rename / move where available. Backends without a stable file id,
/// such as S3, use the exact object key, so renaming changes identity.
/// An ETag is version metadata, not identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StableId(pub String);

impl fmt::Display for StableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `backend_id + stable_id`; used for identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectKey {
    pub backend_id: BackendId,
    pub stable_id: StableId,
}

/// Identity is `backend_id + stable_id`; `display_path` is for display only.
#[derive(Clone, Debug)]
pub struct ObjectRef {
    pub key: ObjectKey,
    pub parent_stable_id: Option<StableId>,
    pub display_path: String,
    /// The local native path could not round-trip through `display_path`. This marker
    /// must survive cloning and caching; string-addressed operations must refuse it.
    pub local_path_is_lossy: bool,
}

impl ObjectRef {
    /// Until the operation/job layer carries native paths, never address their lossy alias.
    pub fn ensure_operable(&self) -> Result<(), super::error::BackendError> {
        if self.local_path_is_lossy {
            return Err(super::error::BackendError::Unsupported {
                operation: "local path cannot be represented losslessly as Unicode",
            });
        }
        Ok(())
    }
}
