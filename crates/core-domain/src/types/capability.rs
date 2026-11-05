/// Operations a backend supports; used to pick a transfer method.
#[derive(Clone, Debug)]
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

impl CapabilitySet {
    /// All capabilities off; adapters enable what they support.
    pub fn none() -> Self {
        Self {
            can_range_read: false,
            can_range_write: false,
            can_direct_thumbnail: false,
            can_rename: false,
            can_clone: false,
            can_server_side_copy: false,
            can_provide_progress: false,
            can_verify_by_checksum: false,
            can_trash: false,
            can_etag: false,
        }
    }
}
