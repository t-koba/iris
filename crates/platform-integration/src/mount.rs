use crate::error::PlatformError;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Commands that talk to a server (share listing, mounting) give up after this long, so an
/// unreachable host cannot leave the connect dialog spinning forever.
const NETWORK_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Unmounting a busy volume or reading the mount table should be quick.
const LOCAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// After the OS accepted a mount request (macOS may first ask for a password in Finder), how
/// long to wait for the new volume to appear.
const MOUNT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(60);

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
    let mounted = list_mounted_volumes()?;
    if let Some(existing) = find_existing_network_mount(&mounted, request) {
        return Ok(existing);
    }
    let before: HashSet<PathBuf> = mounted
        .into_iter()
        .map(|volume| volume.mount_path)
        .collect();

    #[cfg(target_os = "macos")]
    {
        let url = network_mount_url(request);
        run_command(
            Command::new("open").arg(url),
            "network mount via open",
            NETWORK_COMMAND_TIMEOUT,
        )?;
    }

    #[cfg(target_os = "linux")]
    {
        let url = network_mount_url(request);
        run_command(
            Command::new("gio").args(["mount", &url]),
            "network mount via gio",
            NETWORK_COMMAND_TIMEOUT,
        )?;
        // gvfs serves every share below one FUSE mount, so the mount table shows nothing new;
        // ask gio where it put this one.
        if let Some(volume) = gvfs_volume_for_url(&url, request.protocol.kind()) {
            return Ok(volume);
        }
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

/// Mounting a share that is already mounted makes no new volume appear; reuse the mount.
fn find_existing_network_mount(
    mounted: &[MountedVolume],
    request: &NetworkMountRequest,
) -> Option<MountedVolume> {
    let wanted_share = request.share_path.trim_matches('/');
    mounted
        .iter()
        .filter(|volume| volume.kind == request.protocol.kind())
        .find(|volume| {
            resolve_remote_mount_info(&volume.mount_path).is_some_and(|info| {
                info.server.eq_ignore_ascii_case(request.server.trim())
                    && info.remote_path.trim_matches('/') == wanted_share
            })
        })
        .cloned()
}

pub fn unmount_volume(path: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        run_command(
            Command::new("diskutil")
                .arg("unmount")
                .arg(path.as_os_str()),
            "diskutil unmount",
            LOCAL_COMMAND_TIMEOUT,
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
            LOCAL_COMMAND_TIMEOUT,
        )
        .or_else(|_| {
            run_command(
                Command::new("umount").arg(path.as_os_str()),
                "umount",
                LOCAL_COMMAND_TIMEOUT,
            )
        })
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
            LOCAL_COMMAND_TIMEOUT,
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
    let deadline = Instant::now() + MOUNT_DISCOVERY_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));

        // Some platforms only know "a network drive" (Windows), so accept that too; anything
        // else (a USB stick plugged in meanwhile) is not the share that was requested.
        if let Some(volume) = list_mounted_volumes()?.into_iter().find(|volume| {
            !before.contains(&volume.mount_path)
                && (volume.kind == expected_kind || volume.kind == VolumeKind::OtherNetwork)
        }) {
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
    let output = command_output(
        Command::new("net").args(["view", &target]),
        "net view",
        NETWORK_COMMAND_TIMEOUT,
    )?;
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
    shares_or_command_error(shares, &output, "net view")
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
    let output = command_output(&mut cmd, "smbclient", NETWORK_COMMAND_TIMEOUT)?;
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
    shares_or_command_error(shares, &output, "smbclient")
}

/// A failed listing (unreachable host, refused login) must not look like "no shares".
fn shares_or_command_error(
    shares: Vec<String>,
    output: &Output,
    description: &str,
) -> Result<Vec<String>, PlatformError> {
    if shares.is_empty() && !output.status.success() {
        return Err(command_failure(output, description));
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
    let output = command_output(
        Command::new("showmount").args(["-e", server]),
        "showmount",
        NETWORK_COMMAND_TIMEOUT,
    )?;
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
    shares_or_command_error(exports, &output, "showmount")
}

#[cfg(not(target_os = "windows"))]
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

fn run_command(
    command: &mut Command,
    description: &str,
    timeout: Duration,
) -> Result<(), PlatformError> {
    let output = command_output(command, description, timeout)?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_failure(&output, description))
}

fn command_failure(output: &Output, description: &str) -> PlatformError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };

    PlatformError::Mount {
        message: format!("{} failed: {}", description, details),
    }
}

/// Like `Command::output`, but kills the command after `timeout`. Stdin is closed so a tool
/// that wants to prompt for a password fails instead of waiting for input nobody can give.
fn command_output(
    command: &mut Command,
    description: &str,
    timeout: Duration,
) -> Result<Output, PlatformError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| PlatformError::Mount {
            message: format!("failed to execute {}: {}", description, error),
        })?;
    // Drain both pipes while waiting so a chatty command cannot block on a full pipe.
    let stdout = read_pipe_in_background(child.stdout.take());
    let stderr = read_pipe_in_background(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PlatformError::Mount {
                    message: format!(
                        "{} did not finish within {} seconds",
                        description,
                        timeout.as_secs()
                    ),
                });
            }
            Err(error) => {
                return Err(PlatformError::Mount {
                    message: format!("failed to wait for {}: {}", description, error),
                })
            }
        }
    };

    // A helper the command left running may keep the pipe open; do not wait for it.
    let collect = |pipe: mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(Duration::from_secs(1))
            .unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn read_pipe_in_background(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
    }
    receiver
}

#[cfg(target_os = "macos")]
fn list_mounted_volumes_macos() -> Result<Vec<MountedVolume>, PlatformError> {
    let boot_volume_path = std::fs::canonicalize("/").unwrap_or_else(|_| PathBuf::from("/"));
    let mount_table = macos_mount_table();
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
        let kind = mount_table
            .iter()
            .find(|(path, _)| *path == mount_path)
            .map(|(_, kind)| *kind)
            .unwrap_or(VolumeKind::Other);
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

/// Mount points and their kinds from one run of `mount` (not one run per volume).
#[cfg(target_os = "macos")]
fn macos_mount_table() -> Vec<(PathBuf, VolumeKind)> {
    let Ok(output) = command_output(&mut Command::new("mount"), "mount", LOCAL_COMMAND_TIMEOUT)
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_macos_mount_line)
        .collect()
}

/// `//user@server/share on /Volumes/share (smbfs, nodev, ...)`
#[cfg(any(target_os = "macos", test))]
fn parse_macos_mount_line(line: &str) -> Option<(PathBuf, VolumeKind)> {
    let (_, rest) = line.split_once(" on ")?;
    let (mount_point, options) = rest.rsplit_once(" (")?;
    let fs_type = options.split([',', ')']).next()?.trim();
    let kind = match fs_type {
        "smbfs" => VolumeKind::Smb,
        "nfs" => VolumeKind::Nfs,
        "afpfs" | "webdav" => VolumeKind::OtherNetwork,
        _ => VolumeKind::Local,
    };
    Some((PathBuf::from(mount_point), kind))
}

/// `server/share[/subdir]`, the source of an SMB mount without its leading `//`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn smb_mount_info(source: &str, username: Option<String>) -> Option<RemoteMountInfo> {
    let (server, share) = source.split_once('/').unwrap_or((source, ""));
    (!server.is_empty()).then(|| RemoteMountInfo {
        server: server.to_string(),
        remote_path: share.to_string(),
        username,
        kind: VolumeKind::Smb,
    })
}

/// `server:/export`, the source of an NFS mount.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn nfs_mount_info(source: &str) -> Option<RemoteMountInfo> {
    let (server, export) = source.split_once(':').unwrap_or((source, ""));
    let server = server.trim();
    (!server.is_empty()).then(|| RemoteMountInfo {
        server: server.to_string(),
        remote_path: export.to_string(),
        username: None,
        kind: VolumeKind::Nfs,
    })
}

/// Parses the output of `mount` on macOS:
/// SMB: `//user@server/share on /Volumes/share (smbfs, ...)`
/// NFS: `server:/export on /Volumes/nfs (nfs, ...)`
#[cfg(target_os = "macos")]
fn resolve_remote_mount_info_macos(mount_path: &Path) -> Option<RemoteMountInfo> {
    let output = command_output(&mut Command::new("mount"), "mount", LOCAL_COMMAND_TIMEOUT).ok()?;
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
            let (username, rest) = match source.split_once('@') {
                Some((user, rest)) => (Some(user.to_string()), rest),
                None => (None, source),
            };
            return smb_mount_info(rest, username);
        }

        if line.contains("(nfs,") || line.contains("(nfs)") {
            return nfs_mount_info(line.split(" on ").next()?);
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
                let username = options
                    .split(',')
                    .find_map(|opt| opt.strip_prefix("username="))
                    .map(|u| u.to_string());
                return smb_mount_info(source.trim_start_matches("//"), username);
            }
            "nfs" | "nfs4" => {
                return nfs_mount_info(source);
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
            ejectable: mount_path != Path::new("/"),
            kind: linux_volume_kind(fs_type),
            mount_path,
        });
    }
    volumes.extend(gvfs_volumes());

    sort_mounted_volumes(&mut volumes);
    Ok(volumes)
}

/// Shares mounted through GIO (`gio mount`, the desktop file manager) live as directories of
/// the single gvfs FUSE mount, which the mount table hides below `/run`.
#[cfg(target_os = "linux")]
fn gvfs_volumes() -> Vec<MountedVolume> {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(Path::new(&runtime_dir).join("gvfs")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let (kind, name) = parse_gvfs_mount_name(&entry.file_name().to_string_lossy())?;
            Some(MountedVolume {
                name,
                mount_path: entry.path(),
                ejectable: true,
                kind,
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn gvfs_volume_for_url(url: &str, kind: VolumeKind) -> Option<MountedVolume> {
    let output = command_output(
        Command::new("gio").args(["info", url]),
        "gio info",
        NETWORK_COMMAND_TIMEOUT,
    )
    .ok()?;
    let local_path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("local path:"))
        .map(|path| PathBuf::from(path.trim()))?;
    let name = local_path
        .file_name()
        .and_then(|name| parse_gvfs_mount_name(&name.to_string_lossy()))
        .map(|(_, name)| name)
        .unwrap_or_else(|| local_path.to_string_lossy().into_owned());
    Some(MountedVolume {
        name,
        mount_path: local_path,
        ejectable: true,
        kind,
    })
}

/// `smb-share:server=nas,share=photos` -> (Smb, "photos"); `nfs:host=nas,prefix=%2Fexport` ->
/// (Nfs, "export"). Local devices (MTP, cameras) are not network volumes and are skipped.
#[cfg(any(target_os = "linux", test))]
fn parse_gvfs_mount_name(file_name: &str) -> Option<(VolumeKind, String)> {
    let (scheme, params) = file_name.split_once(':')?;
    let value = |key: &str| {
        params.split(',').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key).then(|| percent_decode(value))
        })
    };
    match scheme {
        "smb-share" => Some((VolumeKind::Smb, value("share")?)),
        "nfs" => {
            let host = value("host")?;
            let export = value("prefix").unwrap_or_default();
            let name = export
                .trim_end_matches('/')
                .rsplit('/')
                .find(|segment| !segment.is_empty())
                .map(str::to_string)
                .unwrap_or(host);
            Some((VolumeKind::Nfs, name))
        }
        "sftp" | "ftp" | "ftps" | "dav" | "davs" | "afp-volume" => Some((
            VolumeKind::OtherNetwork,
            value("volume").or_else(|| value("host"))?,
        )),
        _ => None,
    }
}

#[cfg(any(target_os = "linux", test))]
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
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
    // `wmic` is no longer installed by default on current Windows 11; PowerShell's CIM query
    // returns the same columns.
    let output = command_output(
        Command::new("cmd").args(["/C", "wmic logicaldisk get Name,DriveType"]),
        "wmic logicaldisk",
        LOCAL_COMMAND_TIMEOUT,
    )
    .ok()
    .filter(|output| output.status.success())
    .map(Ok)
    .unwrap_or_else(|| {
        command_output(
            Command::new("powershell").args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_LogicalDisk | ForEach-Object { \"$($_.DriveType) $($_.DeviceID)\" }",
            ]),
            "Get-CimInstance Win32_LogicalDisk",
            LOCAL_COMMAND_TIMEOUT,
        )
    })?;
    if !output.status.success() {
        return Err(command_failure(&output, "logical drive query"));
    }

    let mut volumes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_windows_logical_disk_line)
        .collect::<Vec<_>>();
    sort_mounted_volumes(&mut volumes);
    Ok(volumes)
}

/// `"<DriveType> <Name>"`, e.g. `"3 C:"`; header and blank lines yield `None`.
#[cfg(any(target_os = "windows", test))]
fn parse_windows_logical_disk_line(line: &str) -> Option<MountedVolume> {
    let mut columns = line.split_whitespace();
    let drive_type = columns.next()?.parse::<u32>().ok()?;
    let name = columns.next()?;
    if columns.next().is_some() {
        return None;
    }
    Some(MountedVolume {
        name: name.to_string(),
        mount_path: PathBuf::from(format!("{name}\\")),
        // 2: removable, 4: network drive.
        ejectable: matches!(drive_type, 2 | 4),
        kind: if drive_type == 4 {
            VolumeKind::OtherNetwork
        } else {
            VolumeKind::Local
        },
    })
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
                NETWORK_COMMAND_TIMEOUT,
            )
        }
        NetworkMountProtocol::Nfs => {
            let target = format!("{}:{}", request.server, request.share_path);
            run_command(
                Command::new("mount").args(["-o", "anon", &target, "*"]),
                "mount",
                NETWORK_COMMAND_TIMEOUT,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvfs_mount_names_map_to_network_volumes() {
        assert_eq!(
            parse_gvfs_mount_name("smb-share:server=nas.local,share=photos,user=me"),
            Some((VolumeKind::Smb, "photos".to_string()))
        );
        assert_eq!(
            parse_gvfs_mount_name("nfs:host=nas,prefix=%2Fexport%2Fmedia"),
            Some((VolumeKind::Nfs, "media".to_string()))
        );
        assert_eq!(
            parse_gvfs_mount_name("sftp:host=build.example.com,user=ci"),
            Some((VolumeKind::OtherNetwork, "build.example.com".to_string()))
        );
        assert_eq!(parse_gvfs_mount_name("mtp:host=Android_1234"), None);
        assert_eq!(parse_gvfs_mount_name("not-a-mount"), None);
    }

    #[test]
    fn percent_decoding_keeps_invalid_escapes_verbatim() {
        assert_eq!(percent_decode("My%20Share"), "My Share");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn macos_mount_lines_yield_mount_point_and_kind() {
        assert_eq!(
            parse_macos_mount_line(
                "//me@nas._smb._tcp.local/Photos on /Volumes/Photos (smbfs, nodev, nosuid, mounted by me)"
            ),
            Some((PathBuf::from("/Volumes/Photos"), VolumeKind::Smb))
        );
        assert_eq!(
            parse_macos_mount_line("nas:/export on /Volumes/My Export (nfs, nodev)"),
            Some((PathBuf::from("/Volumes/My Export"), VolumeKind::Nfs))
        );
        assert_eq!(
            parse_macos_mount_line("/dev/disk4s1 on /Volumes/USB (msdos, local, nodev)"),
            Some((PathBuf::from("/Volumes/USB"), VolumeKind::Local))
        );
        assert_eq!(parse_macos_mount_line("garbage"), None);
    }

    #[test]
    fn windows_logical_disk_lines_parse_wmic_and_powershell_output() {
        let network = parse_windows_logical_disk_line("4  Z:").expect("network drive");
        assert_eq!(network.kind, VolumeKind::OtherNetwork);
        assert!(network.ejectable);
        assert_eq!(network.name, "Z:");
        let fixed = parse_windows_logical_disk_line("3 C:").expect("fixed drive");
        assert_eq!(fixed.kind, VolumeKind::Local);
        assert!(!fixed.ejectable);
        assert!(parse_windows_logical_disk_line("DriveType  Name").is_none());
        assert!(parse_windows_logical_disk_line("").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn command_output_kills_commands_that_exceed_the_timeout() {
        let started = Instant::now();
        let result = command_output(
            Command::new("sleep").arg("5"),
            "sleep",
            Duration::from_millis(200),
        );
        assert!(
            matches!(result, Err(PlatformError::Mount { message }) if message.contains("did not finish"))
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn failed_share_listing_is_an_error_rather_than_no_shares() {
        let output = command_output(
            Command::new("sh").args(["-c", "echo 'Connection to nas failed' >&2; exit 1"]),
            "smbclient",
            Duration::from_secs(5),
        )
        .expect("run sh");
        let error = shares_or_command_error(Vec::new(), &output, "smbclient")
            .expect_err("failure must surface");
        assert!(error.to_string().contains("Connection to nas failed"));
    }
}
