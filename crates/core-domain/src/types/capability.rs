/// Operations a backend supports; used to pick a transfer method. The default has every
/// capability off; adapters enable what they support.
#[derive(Clone, Debug, Default)]
pub struct CapabilitySet {
    pub can_range_read: bool,
    pub can_range_write: bool,
    pub can_direct_thumbnail: bool,
    pub can_rename: bool,
    pub can_clone: bool,
    pub can_server_side_copy: bool,
    pub can_provide_progress: bool,
    pub can_verify_by_checksum: bool,
    /// Whether `DeleteMode::Trash` is available. Without it deletes are permanent and the UI warns.
    pub can_trash: bool,
    /// Whether the backend returns an ETag, letting VerifyLight compare it.
    pub can_etag: bool,
}
