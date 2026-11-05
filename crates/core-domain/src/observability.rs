use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub fn stable_hash(bytes: impl AsRef<[u8]>) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.as_ref().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn path_hash(path: impl AsRef<str>) -> String {
    stable_hash(path.as_ref().as_bytes())
}
