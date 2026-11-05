#![no_main]

use iris_remote_transport::{
    MappingTable, RemoteTransportKind, RemoteTransportMapping, SshAuthKind,
};
use libfuzzer_sys::fuzz_target;
use std::path::Path;

fn build_mapping(local_mount_path: &str, host: &str, remote_base_path: &str) -> RemoteTransportMapping {
    RemoteTransportMapping {
        id: RemoteTransportMapping::new_id(),
        local_mount_path: local_mount_path.to_string(),
        host: host.to_string(),
        ssh_port: 22,
        ssh_username: "bench".to_string(),
        remote_base_path: remote_base_path.to_string(),
        transport: RemoteTransportKind::Auto,
        auth: SshAuthKind::Agent,
        ssh_identity_file: String::new(),
        ssh_password: String::new(),
        enabled: true,
        auto_detected: false,
        notes: String::new(),
    }
}

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let mut mappings = Vec::new();
    for (index, line) in input.lines().take(32).enumerate() {
        let mut parts = line.split('|');
        let local = parts.next().unwrap_or("/");
        let host = parts.next().unwrap_or("localhost");
        let remote = parts.next().unwrap_or("/data");
        if local.is_empty() {
            continue;
        }
        let mut mapping = build_mapping(local, host, remote);
        if index % 3 == 0 {
            mapping.transport = RemoteTransportKind::Disabled;
        }
        mappings.push(mapping);
    }

    let candidate = input.lines().nth(32).unwrap_or("/tmp/fuzz");
    let _ = MappingTable::lookup_for_local_path(&mappings, Path::new(candidate));
    for mapping in &mappings {
        let _ = mapping.remote_path_for(Path::new(candidate));
    }
});
