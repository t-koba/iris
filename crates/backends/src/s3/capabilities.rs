use iris_core_domain::types::capability::CapabilitySet;

pub fn s3_capabilities() -> CapabilitySet {
    CapabilitySet {
        can_range_read: true,
        can_range_write: false,
        can_direct_thumbnail: false,
        can_rename: false,
        can_clone: false,
        can_server_side_copy: true,
        can_provide_progress: false, // CopyObject reports no progress
        can_verify_by_checksum: true,
        can_trash: false,
        can_etag: true,
    }
}
