use iris_core_domain::types::capability::CapabilitySet;

pub fn localfs_capabilities(can_clone: bool) -> CapabilitySet {
    CapabilitySet {
        can_range_read: true,
        can_range_write: true,
        can_direct_thumbnail: false,
        can_rename: true,
        can_clone,
        can_server_side_copy: false,
        can_provide_progress: true,
        can_verify_by_checksum: true,
        can_trash: true,
        can_etag: false,
    }
}
