use super::*;

fn mk(local: &str, remote: &str, host: &str) -> RemoteTransportMapping {
    RemoteTransportMapping {
        id: RemoteTransportMapping::new_id(),
        local_mount_path: local.into(),
        host: host.into(),
        ssh_port: 22,
        ssh_username: "user".into(),
        remote_base_path: remote.into(),
        transport: RemoteTransportKind::Auto,
        auth: SshAuthKind::Agent,
        ssh_identity_file: String::new(),
        ssh_password: String::new(),
        enabled: true,
        auto_detected: false,
        notes: String::new(),
    }
}

#[test]
fn covers_direct_and_descendant() {
    let m = mk("/Volumes/share", "/data/share", "host");
    assert!(m.covers(Path::new("/Volumes/share")));
    assert!(m.covers(Path::new("/Volumes/share/photos/2024")));
    assert!(!m.covers(Path::new("/Volumes/other")));
    assert!(!m.covers(Path::new("/Users/x")));
}

#[test]
fn remote_path_builds_correctly() {
    let m = mk("/Volumes/share", "/data/share", "host");
    assert_eq!(
        m.remote_path_for(Path::new("/Volumes/share")).unwrap(),
        "/data/share"
    );
    assert_eq!(
        m.remote_path_for(Path::new("/Volumes/share/photos/2024"))
            .unwrap(),
        "/data/share/photos/2024"
    );
    assert!(m.remote_path_for(Path::new("/other")).is_none());
}

#[test]
fn remote_path_normalizes_trailing_slash_in_base() {
    let mut m = mk("/Volumes/share", "/data/share/", "host");
    m.remote_base_path = "/data/share/".into();
    assert_eq!(
        m.remote_path_for(Path::new("/Volumes/share/a/b")).unwrap(),
        "/data/share/a/b"
    );
}

#[test]
fn lookup_prefers_most_specific_mapping() {
    let mappings = vec![
        mk("/Volumes", "/mnt", "host-a"),
        mk("/Volumes/share", "/data/share", "host-b"),
    ];
    let hit =
        MappingTable::lookup_for_local_path(&mappings, Path::new("/Volumes/share/sub/file.raw"))
            .expect("should match");
    assert_eq!(hit.host, "host-b");
}

#[test]
fn lookup_ignores_disabled_entries() {
    let mut mappings = vec![mk("/Volumes/share", "/data/share", "host-b")];
    mappings[0].enabled = false;
    assert!(
        MappingTable::lookup_for_local_path(&mappings, Path::new("/Volumes/share/a")).is_none()
    );
}

#[test]
fn lookup_ignores_disabled_transport() {
    let mut mappings = vec![mk("/Volumes/share", "/data/share", "host-b")];
    mappings[0].transport = RemoteTransportKind::Disabled;
    assert!(
        MappingTable::lookup_for_local_path(&mappings, Path::new("/Volumes/share/a")).is_none()
    );
}

#[test]
fn merge_preserves_user_edits() {
    let mut existing = vec![mk("/Volumes/share", "/custom/data", "user-host")];
    let auto = vec![mk("/Volumes/share", "/auto/path", "auto-host")];
    let added = MappingTable::merge_auto_detected(&mut existing, auto);
    assert_eq!(added, 0);
    assert_eq!(existing[0].remote_base_path, "/custom/data");
    assert_eq!(existing[0].host, "user-host");
}

#[test]
fn merge_adds_new_auto_entries() {
    let mut existing = vec![mk("/Volumes/a", "/data/a", "host-a")];
    let auto = vec![
        mk("/Volumes/a", "/x", "x"),
        mk("/Volumes/b", "/data/b", "host-b"),
    ];
    let added = MappingTable::merge_auto_detected(&mut existing, auto);
    assert_eq!(added, 1);
    assert_eq!(existing.len(), 2);
    assert_eq!(existing[1].local_mount_path, "/Volumes/b");
}

#[test]
fn validate_rejects_bad_inputs() {
    let mut m = RemoteTransportMapping::new_user_defined();
    assert!(m.validate().is_err());
    m.local_mount_path = "/Volumes/share".into();
    assert!(m.validate().is_err());
    m.host = "host".into();
    m.transport = RemoteTransportKind::Sftp;
    assert!(m.validate().is_err());
    m.remote_base_path = "data/rel".into();
    assert!(m.validate().is_err());
    m.remote_base_path = "/data/share".into();
    assert!(m.validate().is_ok());
}

#[test]
fn ssh_target_formatting() {
    let m = mk("/Volumes/share", "/data", "host");
    assert_eq!(m.ssh_target(), "user@host");

    let mut m2 = m;
    m2.ssh_username = String::new();
    assert_eq!(m2.ssh_target(), "host");
}

#[test]
fn rsync_ssh_spec_with_nonstandard_port() {
    let mut m = mk("/Volumes/photo", "/volume1/photo", "nas.local");
    m.ssh_port = 2222;
    m.ssh_username = "admin".into();

    let spec = m
        .rsync_ssh_spec_for(Path::new("/Volumes/photo/2024/raw"))
        .unwrap();
    assert_eq!(spec, "admin@nas.local:/volume1/photo/2024/raw");

    let shell = m.rsync_ssh_remote_shell().unwrap();
    assert_eq!(shell, "ssh -p 2222");
}

#[test]
fn rsync_ssh_port_22_omits_remote_shell() {
    let m = mk("/Volumes/share", "/data", "host");
    assert!(m.rsync_ssh_remote_shell().is_none());
}

#[test]
fn auto_mode_requires_ssh_config() {
    let mut m = RemoteTransportMapping::new_user_defined();
    m.local_mount_path = "/Volumes/share".into();
    m.host = "nas.local".into();
    m.transport = RemoteTransportKind::Auto;
    assert!(m.validate().is_err());
    m.remote_base_path = "/volume1/share".into();
    assert!(m.validate().is_ok());
}

#[test]
fn identity_file_mode_requires_absolute_path() {
    let mut m = mk("/Volumes/share", "/data/share", "host");
    m.auth = SshAuthKind::IdentityFile;
    assert!(m.validate().is_err());
    m.ssh_identity_file = "id_ed25519".into();
    assert!(m.validate().is_err());
    m.ssh_identity_file = "/Users/test/.ssh/id_ed25519".into();
    assert!(m.validate().is_ok());
}

#[test]
fn password_mode_requires_password() {
    let mut m = mk("/Volumes/share", "/data/share", "host");
    m.auth = SshAuthKind::Password;
    assert!(m.validate().is_err());
    m.ssh_password = "secret".into();
    assert!(m.validate().is_ok());
}

#[test]
fn configured_rsync_ssh_name_parses() {
    assert_eq!(
        RemoteTransportKind::from_config_str("RsyncSsh"),
        Some(RemoteTransportKind::RsyncSsh)
    );
    assert_eq!(RemoteTransportKind::from_config_str("Rsync"), None);
}
