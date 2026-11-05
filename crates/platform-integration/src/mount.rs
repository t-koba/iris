use crate::error::PlatformError;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkMountProtocol {
    Smb,
    Nfs,
}

impl NetworkMountProtocol {
    fn kind(self) -> VolumeKind {
        match self {
            Self::Smb => VolumeKind::Smb,
            Self::Nfs => VolumeKind::Nfs,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    Local,
    Smb,
    Nfs,
    OtherNetwork,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountedVolume {
    pub name: String,
    pub mount_path: PathBuf,
    pub ejectable: bool,
    pub kind: VolumeKind,
}

#[derive(Clone, Debug)]
pub struct NetworkMountRequest {
    pub protocol: NetworkMountProtocol,
    pub server: String,
    pub share_path: String,
    pub username: Option<String>,
}

/// Remote server behind a mount point, used for direct rsync-over-SSH transfers.
#[derive(Clone, Debug)]
pub struct RemoteMountInfo {
    pub server: String,
    /// NFS: export path, SMB: share name.
    pub remote_path: String,
    /// Only when it could be determined.
    pub username: Option<String>,
    pub kind: VolumeKind,
}

/// Resolves the remote server of a mount point from the OS mount table; `None` unless SMB / NFS.
pub fn resolve_remote_mount_info(mount_path: &Path) -> Option<RemoteMountInfo> {
    #[cfg(target_os = "macos")]
    {
        resolve_remote_mount_info_macos(mount_path)
    }

    #[cfg(target_os = "linux")]
    {
        resolve_remote_mount_info_linux(mount_path)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = mount_path;
        None
    }
}

pub fn list_mounted_volumes() -> Result<Vec<MountedVolume>, PlatformError> {
    #[cfg(target_os = "macos")]
    {
        list_mounted_volumes_macos()
    }

    #[cfg(target_os = "linux")]
    {
        list_mounted_volumes_linux()
    }

    #[cfg(target_os = "windows")]
    {
        list_mounted_volumes_windows()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(Vec::new())
    }
}

fn sort_mounted_volumes(volumes: &mut [MountedVolume]) {
    volumes.sort_by(|left, right| left.mount_path.cmp(&right.mount_path));
}

pub fn mount_network_share(request: &NetworkMountRequest) -> Result<MountedVolume, PlatformError> {
    let before: HashSet<PathBuf> = list_mounted_volumes()?
        .into_iter()
        .map(|volume| volume.mount_path)
        .collect();

    #[cfg(target_os = "macos")]
    {
        let url = network_mount_url(request);
        run_command(Command::new("open").arg(url), "network mount via open")?;
    }

    #[cfg(target_os = "linux")]
    {
        let url = network_mount_url(request);
        run_command(
            Command::new("gio").args(["mount", &url]),
            "network mount via gio",
        )?;
    }

    #[cfg(target_os = "windows")]
    {
        mount_network_share_windows(request)?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = request;
        return Err(PlatformError::Mount {
            message: "network mounts are unsupported on this platform".to_string(),
        });
    }

    discover_new_volume(&before, request.protocol.kind())
}

pub fn unmount_volume(path: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        run_command(
            Command::new("diskutil")
                .arg("unmount")
                .arg(path.as_os_str()),
            "diskutil unmount",
        )
    }

    #[cfg(target_os = "linux")]
    {
        run_command(
            Command::new("gio")
                .arg("mount")
                .arg("-u")
                .arg(path.as_os_str()),
            "gio mount -u",
        )
        .or_else(|_| run_command(Command::new("umount").arg(path.as_os_str()), "umount"))
    }

    #[cfg(target_os = "windows")]
    {
        let drive = path
            .to_string_lossy()
            .trim_end_matches('\\')
            .trim_end_matches('/')
            .to_string();
        run_command(
            Command::new("net").args(["use", &drive, "/delete", "/y"]),
            "net use /delete",
        )
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = path;
        Err(PlatformError::Mount {
            message: "unmount is unsupported on this platform".to_string(),
        })
    }
}

fn discover_new_volume(
    before: &HashSet<PathBuf>,
    expected_kind: VolumeKind,
) -> Result<MountedVolume, PlatformError> {
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(500));

        let mut candidates = list_mounted_volumes()?;
        candidates.retain(|volume| !before.contains(&volume.mount_path));

        if let Some(volume) = candidates
            .iter()
            .find(|volume| volume.kind == expected_kind)
            .cloned()
        {
            return Ok(volume);
        }

        if let Some(volume) = candidates.pop() {
            return Ok(volume);
        }
    }

    Err(PlatformError::Mount {
        message: "mount completed, but no new volume was discovered".to_string(),
    })
}

/// Lists shares on a server (`showmount -e` for NFS).
pub fn list_network_shares(
    protocol: NetworkMountProtocol,
    server: &str,
    username: Option<&str>,
) -> Result<Vec<String>, PlatformError> {
    match protocol {
        NetworkMountProtocol::Smb => list_smb_shares(server, username),
        NetworkMountProtocol::Nfs => list_nfs_exports(server),
    }
}

#[cfg(target_os = "windows")]
fn list_smb_shares(server: &str, _username: Option<&str>) -> Result<Vec<String>, PlatformError> {
    let target = format!(r"\\{}", server);
    let output = Command::new("net")
        .args(["view", &target])
        .output()
        .map_err(|e| PlatformError::Mount {
            message: format!("net view failed: {}", e),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut shares = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('\\') || trimmed.starts_with('-') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == "Disk" {
            shares.push(parts[0].to_string());
        }
    }
    Ok(shares)
}

#[cfg(not(target_os = "windows"))]
fn list_smb_shares(server: &str, username: Option<&str>) -> Result<Vec<String>, PlatformError> {
    let mut cmd = Command::new("smbclient");
    cmd.arg("-L").arg(server).arg("-N");
    if let Some(user) = username {
        if !user.is_empty() {
            cmd.arg("-U").arg(user);
        }
    }
    let output = cmd.output().map_err(|e| PlatformError::Mount {
        message: format!("smbclient failed: {}", e),
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut shares = Vec::new();
    let mut in_share_section = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Sharename") || trimmed.starts_with("--------") {
            in_share_section = true;
            continue;
        }
        if in_share_section {
            if trimmed.is_empty() {
                in_share_section = false;
                continue;
            }
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "Disk" {
                shares.push(parts[0].to_string());
            }
        }
    }
    Ok(shares)
}

#[cfg(target_os = "windows")]
fn list_nfs_exports(_server: &str) -> Result<Vec<String>, PlatformError> {
    // Windows has no `showmount`, so report unsupported and let the UI fall back to manual entry.
    Err(PlatformError::Mount {
        message: "Automatic NFS export discovery is not supported on Windows. Enter the export path manually.".to_string(),
    })
}

#[cfg(not(target_os = "windows"))]
fn list_nfs_exports(server: &str) -> Result<Vec<String>, PlatformError> {
    let output = Command::new("showmount")
        .args(["-e", server])
        .output()
        .map_err(|e| PlatformError::Mount {
            message: format!("showmount failed: {}", e),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut exports = Vec::new();
    for line in stdout.lines().skip(1) {
        // Format: "/export/path  host1,host2"
        let trimmed = line.trim();
        if let Some(path) = trimmed.split_whitespace().next() {
            if path.starts_with('/') {
                exports.push(path.to_string());
            }
        }
    }
    Ok(exports)
}

fn network_mount_url(request: &NetworkMountRequest) -> String {
    let share_path = request.share_path.trim_start_matches('/');
    match request.protocol {
        NetworkMountProtocol::Smb => match request.username.as_deref() {
            Some(username) if !username.is_empty() => {
                format!("smb://{}@{}/{}", username, request.server, share_path)
            }
            _ => format!("smb://{}/{}", request.server, share_path),
        },
        NetworkMountProtocol::Nfs => format!("nfs://{}{}", request.server, request.share_path),
    }
}

fn run_command(command: &mut Command, description: &str) -> Result<(), PlatformError> {
    let output = command.output().map_err(|error| PlatformError::Mount {
        message: format!("failed to execute {}: {}", description, error),
    })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };

    Err(PlatformError::Mount {
        message: format!("{} failed: {}", description, details),
    })
}

#[cfg(target_os = "macos")]
fn list_mounted_volumes_macos() -> Result<Vec<MountedVolume>, PlatformError> {
    let boot_volume_path = std::fs::canonicalize("/").unwrap_or_else(|_| PathBuf::from("/"));
    let mut volumes = Vec::new();

    let entries = std::fs::read_dir("/Volumes").map_err(|error| PlatformError::Mount {
        message: format!("failed to read /Volumes: {}", error),
    })?;

    for entry in entries.flatten() {
        let mount_path = entry.path();
        if !mount_path.is_dir() {
            continue;
        }

        let resolved = std::fs::canonicalize(&mount_path).unwrap_or_else(|_| mount_path.clone());
        let kind = macos_mount_kind(&mount_path).unwrap_or(VolumeKind::Other);
        volumes.push(MountedVolume {
            name: entry.file_name().to_string_lossy().to_string(),
            ejectable: resolved != boot_volume_path,
            mount_path,
            kind,
        });
    }

    sort_mounted_volumes(&mut volumes);
    Ok(volumes)
}

#[cfg(target_os = "macos")]
fn macos_mount_kind(mount_path: &Path) -> Option<VolumeKind> {
    let output = Command::new("mount").output().ok()?;
    let mount_output = String::from_utf8_lossy(&output.stdout);
    let mount_str = mount_path.to_string_lossy();

    for line in mount_output.lines() {
        if !line.contains(&format!(" on {} ", mount_str)) {
            continue;
        }

        if line.contains("(smbfs,") {
            return Some(VolumeKind::Smb);
        }
        if line.contains("(nfs,") {
            return Some(VolumeKind::Nfs);
        }
        return Some(VolumeKind::Local);
    }

    None
}

/// Parses the output of `mount` on macOS:
/// SMB: `//user@server/share on /Volumes/share (smbfs, ...)`
/// NFS: `server:/export on /Volumes/nfs (nfs, ...)`
#[cfg(target_os = "macos")]
fn resolve_remote_mount_info_macos(mount_path: &Path) -> Option<RemoteMountInfo> {
    let output = Command::new("mount").output().ok()?;
    let mount_output = String::from_utf8_lossy(&output.stdout);
    let mount_str = mount_path.to_string_lossy();

    for line in mount_output.lines() {
        if !line.contains(&format!(" on {} ", mount_str)) {
            continue;
        }

        if line.contains("(smbfs,") || line.contains("(smbfs)") {
            // //user@server/share on /Volumes/... (smbfs, ...) or //server/share on ...
            let source = line.split(" on ").next()?;
            let source = source.trim_start_matches("//");
            // user@server/share or server/share
            let (username, rest) = if source.contains('@') {
                let mut parts = source.splitn(2, '@');
                let user = parts.next().unwrap_or("");
                let rest = parts.next().unwrap_or("");
                (Some(user.to_string()), rest)
            } else {
                (None, source)
            };
            // rest = "server/share" or "server/share/subdir"
            let mut path_parts = rest.splitn(2, '/');
            let server = path_parts.next()?.to_string();
            let share = path_parts.next().unwrap_or("").to_string();
            if server.is_empty() {
                return None;
            }
            return Some(RemoteMountInfo {
                server,
                remote_path: share,
                username,
                kind: VolumeKind::Smb,
            });
        }

        if line.contains("(nfs,") || line.contains("(nfs)") {
            let source = line.split(" on ").next()?;
            let mut parts = source.splitn(2, ':');
            let server = parts.next()?.trim().to_string();
            let export = parts.next().unwrap_or("").to_string();
            if server.is_empty() {
                return None;
            }
            return Some(RemoteMountInfo {
                server,
                remote_path: export,
                username: None,
                kind: VolumeKind::Nfs,
            });
        }
    }
    None
}

/// Parses `/proc/mounts` on Linux:
/// SMB: `//server/share /mnt/smb cifs rw,username=user,...`
/// NFS: `server:/export /mnt/nfs nfs rw,...`
#[cfg(target_os = "linux")]
fn resolve_remote_mount_info_linux(mount_path: &Path) -> Option<RemoteMountInfo> {
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    let canonical = mount_path
        .canonicalize()
        .unwrap_or_else(|_| mount_path.to_path_buf());

    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let source = fields.next()?;
        let mp = fields.next()?;
        let fs_type = fields.next()?;
        let options = fields.next().unwrap_or("");

        let mp_path = PathBuf::from(unescape_proc_mount_path(mp));
        if mp_path != canonical {
            continue;
        }

        match fs_type {
            "cifs" | "smb3" | "smbfs" => {
                // source = "//server/share"
                let src = source.trim_start_matches("//");
                let mut parts = src.splitn(2, '/');
                let server = parts.next()?.to_string();
                let share = parts.next().unwrap_or("").to_string();
                let username = options
                    .split(',')
                    .find_map(|opt| opt.strip_prefix("username="))
                    .map(|u| u.to_string());
                if server.is_empty() {
                    return None;
                }
                return Some(RemoteMountInfo {
                    server,
                    remote_path: share,
                    username,
                    kind: VolumeKind::Smb,
                });
            }
            "nfs" | "nfs4" => {
                // source = "server:/export"
                let mut parts = source.splitn(2, ':');
                let server = parts.next()?.to_string();
                let export = parts.next().unwrap_or("").to_string();
                if server.is_empty() {
                    return None;
                }
                return Some(RemoteMountInfo {
                    server,
                    remote_path: export,
                    username: None,
                    kind: VolumeKind::Nfs,
                });
            }
            _ => {}
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn list_mounted_volumes_linux() -> Result<Vec<MountedVolume>, PlatformError> {
    let mounts = std::fs::read_to_string("/proc/mounts").map_err(|error| PlatformError::Mount {
        message: format!("failed to read /proc/mounts: {}", error),
    })?;

    let mut volumes = Vec::new();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let _source = parts.next();
        let mount_point = parts.next();
        let fs_type = parts.next();

        let (Some(mount_point), Some(fs_type)) = (mount_point, fs_type) else {
            continue;
        };

        let mount_path = PathBuf::from(unescape_proc_mount_path(mount_point));
        if is_linux_system_mount(&mount_path, fs_type) {
            continue;
        }

        volumes.push(MountedVolume {
            name: mount_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| mount_path.to_string_lossy().to_string()),
            ejectable: mount_path != PathBuf::from("/"),
            kind: linux_volume_kind(fs_type),
            mount_path,
        });
    }

    sort_mounted_volumes(&mut volumes);
    Ok(volumes)
}

#[cfg(target_os = "linux")]
fn linux_volume_kind(fs_type: &str) -> VolumeKind {
    match fs_type {
        "cifs" | "smb3" | "smbfs" => VolumeKind::Smb,
        "nfs" | "nfs4" => VolumeKind::Nfs,
        "fuse.sshfs" | "davfs" => VolumeKind::OtherNetwork,
        _ => VolumeKind::Local,
    }
}

#[cfg(target_os = "linux")]
fn is_linux_system_mount(mount_path: &Path, fs_type: &str) -> bool {
    matches!(
        fs_type,
        "proc"
            | "sysfs"
            | "tmpfs"
            | "devtmpfs"
            | "devpts"
            | "securityfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "debugfs"
            | "tracefs"
            | "configfs"
            | "overlay"
            | "squashfs"
            | "nsfs"
            | "ramfs"
            | "autofs"
    ) || mount_path.starts_with("/proc")
        || mount_path.starts_with("/sys")
        || mount_path.starts_with("/dev")
        || mount_path.starts_with("/run")
}

#[cfg(target_os = "linux")]
fn unescape_proc_mount_path(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "windows")]
fn list_mounted_volumes_windows() -> Result<Vec<MountedVolume>, PlatformError> {
    let output = Command::new("cmd")
        .args(["/C", "wmic logicaldisk get Name,DriveType"])
        .output()
        .map_err(|error| PlatformError::Mount {
            message: format!("failed to query logical drives: {}", error),
        })?;

    if !output.status.success() {
        return Err(PlatformError::Mount {
            message: "wmic logicaldisk failed".to_string(),
        });
    }

    let mut volumes = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let columns: Vec<_> = trimmed.split_whitespace().collect();
        if columns.len() != 2 {
            continue;
        }

        let drive_type = columns[0];
        let name = columns[1];
        let kind = if drive_type == "4" {
            VolumeKind::OtherNetwork
        } else {
            VolumeKind::Local
        };

        volumes.push(MountedVolume {
            name: name.to_string(),
            mount_path: PathBuf::from(name),
            ejectable: drive_type == "2" || drive_type == "4",
            kind,
        });
    }

    sort_mounted_volumes(&mut volumes);
    Ok(volumes)
}

#[cfg(target_os = "windows")]
fn mount_network_share_windows(request: &NetworkMountRequest) -> Result<(), PlatformError> {
    match request.protocol {
        NetworkMountProtocol::Smb => {
            let target = format!(
                r"\\{}\{}",
                request.server,
                request.share_path.trim_start_matches('/')
            );
            run_command(
                Command::new("net").args(["use", "*", &target, "/persistent:no"]),
                "net use",
            )
        }
        NetworkMountProtocol::Nfs => {
            let target = format!("{}:{}", request.server, request.share_path);
            run_command(
                Command::new("mount").args(["-o", "anon", &target, "*"]),
                "mount",
            )
        }
    }
}
