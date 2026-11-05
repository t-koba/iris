//! Remote transfer mappings: a user-editable table linking SMB/NFS mount points to SSH hosts.
//!
//! Copying through a mount is much slower than SFTP / rsync over SSH to the same server.
//! Auto-detection from the OS mount table only seeds suggestions; the user-edited table is the
//! source of truth, since mount tables differ per OS, are unavailable on Windows and lack SSH ports.

use std::path::{Path, PathBuf};

mod sftp;

pub use sftp::{
    execute_sftp_plan, SftpTransferDirection, SftpTransferEntry, SftpTransferPlan,
    DEFAULT_SFTP_PIPELINE_DEPTH, MAX_SFTP_PIPELINE_DEPTH,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteTransportKind {
    /// Try SFTP, then rsync over SSH.
    Auto,
    /// Native SFTP (pure Rust `russh` + `russh-sftp`); no external command needed.
    Sftp,
    /// External `rsync -e 'ssh -p PORT' user@host:/path/`, e.g. for Synology DSM or Linux servers.
    RsyncSsh,
    /// Mapping kept but not used for transfers.
    Disabled,
}

impl RemoteTransportKind {
    pub const ALL: [RemoteTransportKind; 4] = [
        RemoteTransportKind::Auto,
        RemoteTransportKind::Sftp,
        RemoteTransportKind::RsyncSsh,
        RemoteTransportKind::Disabled,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RemoteTransportKind::Auto => "Auto (SFTP -> rsync)",
            RemoteTransportKind::Sftp => "SFTP (native)",
            RemoteTransportKind::RsyncSsh => "rsync over SSH",
            RemoteTransportKind::Disabled => "Disabled",
        }
    }

    pub fn as_config_str(self) -> &'static str {
        match self {
            RemoteTransportKind::Auto => "Auto",
            RemoteTransportKind::Sftp => "Sftp",
            RemoteTransportKind::RsyncSsh => "RsyncSsh",
            RemoteTransportKind::Disabled => "Disabled",
        }
    }

    pub fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "Auto" => Some(RemoteTransportKind::Auto),
            "Sftp" => Some(RemoteTransportKind::Sftp),
            "RsyncSsh" => Some(RemoteTransportKind::RsyncSsh),
            "Disabled" => Some(RemoteTransportKind::Disabled),
            _ => None,
        }
    }
}

impl std::fmt::Display for RemoteTransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshAuthKind {
    /// SSH agent (SSH_AUTH_SOCK); recommended.
    Agent,
    /// Picks a key from `~/.ssh/id_*`.
    DefaultKey,
    /// Explicit OpenSSH identity file.
    IdentityFile,
    Password,
}

impl SshAuthKind {
    pub const ALL: [SshAuthKind; 4] = [
        SshAuthKind::Agent,
        SshAuthKind::DefaultKey,
        SshAuthKind::IdentityFile,
        SshAuthKind::Password,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SshAuthKind::Agent => "SSH Agent",
            SshAuthKind::DefaultKey => "Default private key (~/.ssh/id_*)",
            SshAuthKind::IdentityFile => "identity file",
            SshAuthKind::Password => "Password",
        }
    }

    pub fn as_config_str(self) -> &'static str {
        match self {
            SshAuthKind::Agent => "Agent",
            SshAuthKind::DefaultKey => "DefaultKey",
            SshAuthKind::IdentityFile => "IdentityFile",
            SshAuthKind::Password => "Password",
        }
    }

    pub fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "Agent" => Some(SshAuthKind::Agent),
            "DefaultKey" => Some(SshAuthKind::DefaultKey),
            "IdentityFile" => Some(SshAuthKind::IdentityFile),
            "Password" => Some(SshAuthKind::Password),
            _ => None,
        }
    }
}

impl std::fmt::Display for SshAuthKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteTransportValidationError {
    EmptyLocalMountPath,
    LocalMountPathNotAbsolute,
    EmptyHost,
    InvalidSshPort,
    EmptyRemoteBasePath,
    SshRemoteBasePathNotAbsolute,
    RemoteBasePathNotAbsolute,
    EmptyIdentityFile,
    IdentityFileNotAbsolute,
    EmptySshPassword,
}

impl std::fmt::Display for RemoteTransportValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RemoteTransportValidationError::EmptyLocalMountPath => "local mount path is empty",
            RemoteTransportValidationError::LocalMountPathNotAbsolute => {
                "local mount path must be absolute"
            }
            RemoteTransportValidationError::EmptyHost => "host is empty",
            RemoteTransportValidationError::InvalidSshPort => "SSH port must be at least 1",
            RemoteTransportValidationError::EmptyRemoteBasePath => "remote base path is empty",
            RemoteTransportValidationError::SshRemoteBasePathNotAbsolute => {
                "remote base path must start with `/` in SSH mode"
            }
            RemoteTransportValidationError::RemoteBasePathNotAbsolute => {
                "remote base path must start with `/`"
            }
            RemoteTransportValidationError::EmptyIdentityFile => "identity file is empty",
            RemoteTransportValidationError::IdentityFileNotAbsolute => {
                "identity file must be absolute"
            }
            RemoteTransportValidationError::EmptySshPassword => "SSH password is empty",
        })
    }
}

impl std::error::Error for RemoteTransportValidationError {}

/// Maps a local mount path to a remote SSH / rsync target.
#[derive(Clone, Debug)]
pub struct RemoteTransportMapping {
    /// Stable id used by the UI to edit or delete the mapping.
    pub id: String,
    /// For example `/Volumes/photos`.
    pub local_mount_path: String,
    pub host: String,
    /// Default 22; NAS devices such as Synology often use another port (e.g. 2222).
    pub ssh_port: u16,
    /// Empty means the current local user.
    pub ssh_username: String,
    pub remote_base_path: String,
    pub transport: RemoteTransportKind,
    pub auth: SshAuthKind,
    /// Used with `IdentityFile` authentication.
    pub ssh_identity_file: String,
    /// Password for `Password` authentication, or the key passphrase for `IdentityFile`.
    pub ssh_password: String,
    /// Disabled mappings are ignored at runtime.
    pub enabled: bool,
    /// Whether the entry came from auto-detection (shown as an icon in the UI).
    pub auto_detected: bool,
    /// Free-form note, e.g. the NAS model.
    pub notes: String,
}

impl RemoteTransportMapping {
    /// Timestamp + counter based id.
    pub fn new_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("m{:016x}{:04x}", ts, n & 0xFFFF)
    }

    /// Empty mapping for the UI's Add button.
    pub fn new_user_defined() -> Self {
        Self {
            id: Self::new_id(),
            local_mount_path: String::new(),
            host: String::new(),
            ssh_port: 22,
            ssh_username: String::new(),
            remote_base_path: String::new(),
            transport: RemoteTransportKind::Auto,
            auth: SshAuthKind::Agent,
            ssh_identity_file: String::new(),
            ssh_password: String::new(),
            enabled: false,
            auto_detected: false,
            notes: String::new(),
        }
    }

    /// Whether `path` is inside `local_mount_path`.
    pub fn covers(&self, path: &Path) -> bool {
        if self.local_mount_path.is_empty() {
            return false;
        }
        let root = Path::new(&self.local_mount_path);
        path == root || path.starts_with(root)
    }

    /// Path of `local` relative to the mount, or `None` if the mapping does not cover it.
    pub fn relative_for(&self, local: &Path) -> Option<PathBuf> {
        let root = Path::new(&self.local_mount_path);
        if local == root {
            Some(PathBuf::new())
        } else {
            local.strip_prefix(root).ok().map(|p| p.to_path_buf())
        }
    }

    /// Remote absolute path: `remote_base_path` + the relative part.
    pub fn remote_path_for(&self, local: &Path) -> Option<String> {
        let relative = self.relative_for(local)?;
        let base = self.remote_base_path.trim_end_matches('/');
        let rel_str = relative.to_string_lossy();
        if rel_str.is_empty() {
            Some(base.to_string())
        } else if base.is_empty() {
            Some(format!("/{}", rel_str))
        } else {
            Some(format!("{}/{}", base, rel_str))
        }
    }

    /// `user@host`, or just `host` when no user name is set.
    pub fn ssh_target(&self) -> String {
        if self.ssh_username.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.ssh_username, self.host)
        }
    }

    /// rsync argument `user@host:path`.
    pub fn rsync_ssh_spec_for(&self, local: &Path) -> Option<String> {
        let remote = self.remote_path_for(local)?;
        Some(format!("{}:{}", self.ssh_target(), remote))
    }

    /// SSH command for rsync's `-e`; `None` on port 22 so the flag is omitted.
    pub fn rsync_ssh_remote_shell(&self) -> Option<String> {
        if self.ssh_port == 22 {
            None
        } else {
            Some(format!("ssh -p {}", self.ssh_port))
        }
    }

    pub fn validate(&self) -> Result<(), RemoteTransportValidationError> {
        if self.local_mount_path.trim().is_empty() {
            return Err(RemoteTransportValidationError::EmptyLocalMountPath);
        }
        if !Path::new(&self.local_mount_path).is_absolute() {
            return Err(RemoteTransportValidationError::LocalMountPathNotAbsolute);
        }
        if self.host.trim().is_empty() {
            return Err(RemoteTransportValidationError::EmptyHost);
        }

        match self.transport {
            RemoteTransportKind::Sftp | RemoteTransportKind::RsyncSsh => {
                if self.ssh_port == 0 {
                    return Err(RemoteTransportValidationError::InvalidSshPort);
                }
                if self.remote_base_path.trim().is_empty() {
                    return Err(RemoteTransportValidationError::EmptyRemoteBasePath);
                }
                if !self.remote_base_path.starts_with('/') {
                    return Err(RemoteTransportValidationError::SshRemoteBasePathNotAbsolute);
                }
                self.validate_auth_fields()?;
            }
            RemoteTransportKind::Auto => {
                // Auto prefers SSH-based transports, so it needs SSH settings.
                if self.ssh_port == 0 {
                    return Err(RemoteTransportValidationError::InvalidSshPort);
                }
                if self.remote_base_path.trim().is_empty() {
                    return Err(RemoteTransportValidationError::EmptyRemoteBasePath);
                }
                if !self.remote_base_path.starts_with('/') {
                    return Err(RemoteTransportValidationError::RemoteBasePathNotAbsolute);
                }
                self.validate_auth_fields()?;
            }
            RemoteTransportKind::Disabled => {}
        }
        Ok(())
    }

    fn validate_auth_fields(&self) -> Result<(), RemoteTransportValidationError> {
        match self.auth {
            SshAuthKind::Agent | SshAuthKind::DefaultKey => Ok(()),
            SshAuthKind::IdentityFile => {
                if self.ssh_identity_file.trim().is_empty() {
                    return Err(RemoteTransportValidationError::EmptyIdentityFile);
                }
                if !Path::new(&self.ssh_identity_file).is_absolute() {
                    return Err(RemoteTransportValidationError::IdentityFileNotAbsolute);
                }
                Ok(())
            }
            SshAuthKind::Password => {
                if self.ssh_password.is_empty() {
                    return Err(RemoteTransportValidationError::EmptySshPassword);
                }
                Ok(())
            }
        }
    }
}

/// Text fields of the edit dialog, so the update handler can use a single `match`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditField {
    LocalPath,
    Host,
    SshUser,
    SshPort,
    SshIdentityFile,
    SshPassword,
    RemoteBase,
    Notes,
}

impl EditField {
    /// Writes `value` into the field; an empty numeric field resets it to 0.
    pub fn apply(self, buf: &mut RemoteTransportMapping, value: String) {
        match self {
            EditField::LocalPath => buf.local_mount_path = value,
            EditField::Host => buf.host = value,
            EditField::SshUser => buf.ssh_username = value,
            EditField::SshIdentityFile => buf.ssh_identity_file = value,
            EditField::SshPassword => buf.ssh_password = value,
            EditField::RemoteBase => buf.remote_base_path = value,
            EditField::Notes => buf.notes = value,
            EditField::SshPort => {
                buf.ssh_port = value.trim().parse::<u16>().unwrap_or(0);
            }
        }
    }
}

/// Suggested mappings for the SMB/NFS mounts in the OS mount table.
///
/// Only for the initial seed and the settings "re-detect" button; transfers never call it.
///
/// Every entry has:
/// - `auto_detected = true`, `enabled = false` (unused until the user enables it), `transport = Auto`
/// - SSH port 22 and the user name from the mount table, if any
pub fn seed_from_mount_table() -> Vec<RemoteTransportMapping> {
    use iris_platform_integration::mount::{
        list_mounted_volumes, resolve_remote_mount_info, VolumeKind,
    };

    let Ok(volumes) = list_mounted_volumes() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for volume in volumes {
        if !matches!(volume.kind, VolumeKind::Smb | VolumeKind::Nfs) {
            continue;
        }
        let Some(remote) = resolve_remote_mount_info(&volume.mount_path) else {
            continue;
        };

        // Without a remote path there is no SSH destination.
        let remote_base = if remote.remote_path.is_empty() {
            "/".to_string()
        } else if remote.remote_path.starts_with('/') {
            remote.remote_path.clone()
        } else {
            format!("/{}", remote.remote_path)
        };

        let mapping = RemoteTransportMapping {
            id: RemoteTransportMapping::new_id(),
            local_mount_path: volume.mount_path.to_string_lossy().into_owned(),
            host: remote.server.clone(),
            ssh_port: 22,
            ssh_username: remote.username.clone().unwrap_or_default(),
            remote_base_path: remote_base,
            transport: RemoteTransportKind::Auto,
            auth: SshAuthKind::Agent,
            ssh_identity_file: String::new(),
            ssh_password: String::new(),
            enabled: false,
            auto_detected: true,
            notes: match volume.kind {
                VolumeKind::Smb => "Auto-detected from SMB mount".into(),
                VolumeKind::Nfs => "Auto-detected from NFS mount".into(),
                _ => String::new(),
            },
        };
        out.push(mapping);
    }
    out
}

pub struct MappingTable;

impl MappingTable {
    /// The enabled mapping covering `path`; the longest (most specific) mount path wins.
    pub fn lookup_for_local_path<'a>(
        mappings: &'a [RemoteTransportMapping],
        path: &Path,
    ) -> Option<&'a RemoteTransportMapping> {
        mappings
            .iter()
            .filter(|m| m.enabled && m.transport != RemoteTransportKind::Disabled)
            .filter(|m| m.covers(path))
            .max_by_key(|m| m.local_mount_path.len())
    }

    /// Adds newly detected mounts as disabled, auto-detected entries; existing mount paths are kept.
    pub fn merge_auto_detected(
        existing: &mut Vec<RemoteTransportMapping>,
        auto: Vec<RemoteTransportMapping>,
    ) -> usize {
        let mut added = 0;
        for entry in auto {
            if entry.local_mount_path.is_empty() {
                continue;
            }
            let dup = existing
                .iter()
                .any(|m| m.local_mount_path == entry.local_mount_path);
            if !dup {
                existing.push(entry);
                added += 1;
            }
        }
        added
    }
}

#[cfg(test)]
mod tests;
