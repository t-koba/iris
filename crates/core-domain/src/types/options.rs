/// How to resolve a same-name conflict.
#[derive(Clone, Debug)]
pub enum ConflictResolution {
    Fail,
    Skip,
    /// Rename automatically by adding a suffix.
    Rename,
    Overwrite,
}

#[derive(Clone, Debug)]
pub struct CopyOptions {
    pub on_conflict: ConflictResolution,
}

#[derive(Clone, Debug)]
pub struct MoveOptions {
    pub on_conflict: ConflictResolution,
}

#[derive(Clone, Debug)]
pub enum DeleteMode {
    Trash,
    Permanent,
}
