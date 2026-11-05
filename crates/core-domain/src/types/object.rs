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

/// Immutable object id from the backend (inode, S3 ETag, cloud file id, ...).
/// Expected to survive rename / move.
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
}
