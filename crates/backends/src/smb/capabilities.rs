use iris_core_domain::types::capability::CapabilitySet;

pub fn smb_capabilities() -> CapabilitySet {
    CapabilitySet {
        can_range_read: true,
        can_rename: true,
        can_server_side_copy: true,
        can_provide_progress: true,
        can_verify_by_checksum: true,
        ..CapabilitySet::default()
    }
}
