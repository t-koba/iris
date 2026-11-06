use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use russh::client;
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::keys::{self, Algorithm, HashAlg};
use russh_sftp::client::{error::Error as SftpError, RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tracing::debug;

use crate::{RemoteTransportMapping, SshAuthKind};

const KEEPALIVE_SECS: u64 = 30;
const SFTP_POOL_IDLE_TTL: Duration = Duration::from_secs(120);
const SFTP_POOL_MAX_IDLE_PER_KEY: usize = 2;
const SFTP_PIPELINE_CHUNK_BYTES: usize = 240 * 1024;
pub const DEFAULT_SFTP_PIPELINE_DEPTH: usize = 8;
pub const MAX_SFTP_PIPELINE_DEPTH: usize = 16;
const DEFAULT_KEY_CANDIDATES: [&str; 2] = ["id_ed25519", "id_ecdsa"];

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type SshHandle = client::Handle<KnownHostsHandler>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpTransferDirection {
    Upload,
    Download,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpTransferEntry {
    pub local_path: PathBuf,
    pub remote_path: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct SftpTransferPlan {
    pub mapping: RemoteTransportMapping,
    pub direction: SftpTransferDirection,
    pub delete_sources: bool,
    pub pipeline_depth: usize,
    pub entries: Vec<SftpTransferEntry>,
}

#[derive(Debug)]
struct KnownHostsHandler {
    host: String,
    port: u16,
}

#[derive(Debug)]
struct SftpHandlerError(String);

struct ConnectedSftp {
    _handle: SshHandle,
    session: SftpSession,
    transfer_session: Arc<RawSftpSession>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SftpPoolKey {
    host: String,
    port: u16,
    username: String,
    auth: &'static str,
    auth_detail: String,
}

struct PooledSftp {
    connected: ConnectedSftp,
    last_used: Instant,
}

#[derive(Default)]
struct SftpPool {
    idle: HashMap<SftpPoolKey, VecDeque<PooledSftp>>,
}

#[derive(Default)]
struct RemoteDirCreateCache {
    attempted: HashSet<String>,
}

// Only entry boundaries are retryable: an in-flight transfer or source deletion
// may have committed remotely even when its acknowledgement was lost.
#[derive(Default)]
struct SftpPlanProgress {
    completed: usize,
    entry_started: bool,
}

impl SftpPlanProgress {
    fn can_retry(&self, reused: bool, error: &str) -> bool {
        reused && !self.entry_started && sftp_error_is_connection_failure(error)
    }
}

struct SftpConnectionLease {
    key: SftpPoolKey,
    connected: Option<ConnectedSftp>,
    reused: bool,
}

impl std::fmt::Display for SftpHandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SftpHandlerError {}

impl From<russh::Error> for SftpHandlerError {
    fn from(error: russh::Error) -> Self {
        Self(error.to_string())
    }
}

impl client::Handler for KnownHostsHandler {
    type Error = SftpHandlerError;

    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // Host certificates are matched by their underlying key; CA trust is not supported.
        let server_public_key = server_key.public_key();
        keys::check_known_hosts(&self.host, self.port, &server_public_key).map_err(|error| {
            SftpHandlerError(format!(
                "known_hosts validation failed for {}:{}: {error}",
                self.host, self.port
            ))
        })
    }
}

pub async fn execute_sftp_plan(plan: &SftpTransferPlan) -> Result<(), String> {
    plan.mapping.validate().map_err(|error| error.to_string())?;
    if plan.entries.is_empty() {
        return Ok(());
    }
    debug!(
        mapping_id_hash = %iris_core_domain::observability::path_hash(&plan.mapping.id),
        host_hash = %iris_core_domain::observability::path_hash(&plan.mapping.host),
        entries = plan.entries.len(),
        direction = ?plan.direction,
        delete_sources = plan.delete_sources,
        "sftp.execute_plan"
    );

    let mut lease = SftpConnectionLease::checkout(&plan.mapping).await?;
    let pipeline_depth = normalize_pipeline_depth(plan.pipeline_depth);
    let mut progress = SftpPlanProgress::default();
    let mut result = execute_sftp_entries(
        plan,
        lease.session(),
        lease.transfer_session(),
        pipeline_depth,
        &mut progress,
    )
    .await;
    if result
        .as_ref()
        .is_err_and(|error| progress.can_retry(lease.reused, error))
    {
        lease.close().await;
        lease = SftpConnectionLease::fresh(&plan.mapping).await?;
        result = execute_sftp_entries(
            plan,
            lease.session(),
            lease.transfer_session(),
            pipeline_depth,
            &mut progress,
        )
        .await;
    }
    if result.is_ok() {
        lease.return_to_pool();
    } else {
        lease.close().await;
    }

    result
}

fn sftp_error_is_connection_failure(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "connection closed",
        "connection reset",
        "broken pipe",
        "unexpected eof",
        "timed out",
        "timeout",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn normalize_pipeline_depth(requested: usize) -> usize {
    requested.clamp(DEFAULT_SFTP_PIPELINE_DEPTH, MAX_SFTP_PIPELINE_DEPTH)
}

async fn connect_sftp(mapping: &RemoteTransportMapping) -> Result<ConnectedSftp, String> {
    debug!(
        mapping_id_hash = %iris_core_domain::observability::path_hash(&mapping.id),
        host_hash = %iris_core_domain::observability::path_hash(&mapping.host),
        port = mapping.ssh_port,
        auth = ?mapping.auth,
        "sftp.connect_start"
    );
    let config = ssh_client_config();
    let handler = KnownHostsHandler {
        host: mapping.host.clone(),
        port: mapping.ssh_port,
    };
    let mut handle = client::connect(
        Arc::new(config),
        (mapping.host.as_str(), mapping.ssh_port),
        handler,
    )
    .await
    .map_err(|error| format!("failed to connect SSH session: {error}"))?;

    let username = resolved_ssh_username(mapping)?;
    match mapping.auth {
        SshAuthKind::Agent => authenticate_with_agent(&mut handle, &username).await?,
        SshAuthKind::DefaultKey => authenticate_with_default_key(&mut handle, &username).await?,
        SshAuthKind::IdentityFile => {
            authenticate_with_identity_file(&mut handle, &username, mapping).await?
        }
        SshAuthKind::Password => {
            authenticate_with_password(&mut handle, &username, mapping).await?
        }
    }

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|error| format!("failed to open SSH session channel: {error}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| format!("failed to request SFTP subsystem: {error}"))?;

    let session = SftpSession::new(channel.into_stream())
        .await
        .map_err(|error| format!("failed to initialize SFTP session: {error}"))?;
    session.set_timeout(KEEPALIVE_SECS);

    let transfer_channel = handle
        .channel_open_session()
        .await
        .map_err(|error| format!("failed to open SFTP transfer channel: {error}"))?;
    transfer_channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| format!("failed to request SFTP transfer subsystem: {error}"))?;
    let transfer_session = RawSftpSession::new(transfer_channel.into_stream());
    transfer_session.set_timeout(KEEPALIVE_SECS);
    transfer_session
        .init()
        .await
        .map_err(|error| format!("failed to initialize SFTP transfer session: {error}"))?;

    debug!(
        mapping_id_hash = %iris_core_domain::observability::path_hash(&mapping.id),
        auth = ?mapping.auth,
        "sftp.connect_complete"
    );
    Ok(ConnectedSftp {
        _handle: handle,
        session,
        transfer_session: Arc::new(transfer_session),
    })
}

fn ssh_client_config() -> client::Config {
    client::Config {
        keepalive_interval: Some(Duration::from_secs(KEEPALIVE_SECS)),
        nodelay: true,
        window_size: 2 * 1024 * 1024,
        maximum_packet_size: SFTP_PIPELINE_CHUNK_BYTES as u32,
        ..Default::default()
    }
}

impl SftpConnectionLease {
    async fn checkout(mapping: &RemoteTransportMapping) -> Result<Self, String> {
        let key = sftp_pool_key(mapping)?;
        let pooled = {
            let mut pool = sftp_pool()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            pool.checkout(&key, Instant::now())
        };
        if let Some(connected) = pooled {
            let alive =
                tokio::time::timeout(Duration::from_secs(5), connected.session.canonicalize("."))
                    .await
                    .is_ok_and(|result| result.is_ok());
            if !alive {
                let _ = connected.session.close().await;
                let _ = connected.transfer_session.close_session();
                return Self::fresh(mapping).await;
            }
            debug!(
                mapping_id_hash = %iris_core_domain::observability::path_hash(&mapping.id),
                host_hash = %iris_core_domain::observability::path_hash(&mapping.host),
                auth = key.auth,
                "sftp.pool_reuse"
            );
            return Ok(Self {
                key,
                connected: Some(connected),
                reused: true,
            });
        }

        Self::fresh(mapping).await
    }

    async fn fresh(mapping: &RemoteTransportMapping) -> Result<Self, String> {
        let key = sftp_pool_key(mapping)?;
        let connected = connect_sftp(mapping).await?;
        Ok(Self {
            key,
            connected: Some(connected),
            reused: false,
        })
    }

    fn session(&self) -> &SftpSession {
        &self
            .connected
            .as_ref()
            .expect("sftp lease must hold a connection")
            .session
    }

    fn transfer_session(&self) -> Arc<RawSftpSession> {
        Arc::clone(
            &self
                .connected
                .as_ref()
                .expect("sftp lease must hold a connection")
                .transfer_session,
        )
    }

    fn return_to_pool(&mut self) {
        let Some(connected) = self.connected.take() else {
            return;
        };
        let mut pool = sftp_pool()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pool.checkin(self.key.clone(), connected, Instant::now());
    }

    async fn close(&mut self) {
        if let Some(connected) = self.connected.take() {
            let _ = connected.session.close().await;
            let _ = connected.transfer_session.close_session();
        }
    }
}

impl SftpPool {
    fn checkout(&mut self, key: &SftpPoolKey, now: Instant) -> Option<ConnectedSftp> {
        self.prune_expired(now);
        let connected = self
            .idle
            .get_mut(key)
            .and_then(|entries| entries.pop_back())
            .map(|pooled| pooled.connected);
        if self.idle.get(key).is_some_and(VecDeque::is_empty) {
            self.idle.remove(key);
        }
        connected
    }

    fn checkin(&mut self, key: SftpPoolKey, connected: ConnectedSftp, now: Instant) {
        self.prune_expired(now);
        let entries = self.idle.entry(key).or_default();
        entries.push_back(PooledSftp {
            connected,
            last_used: now,
        });
        while entries.len() > SFTP_POOL_MAX_IDLE_PER_KEY {
            entries.pop_front();
        }
    }

    fn prune_expired(&mut self, now: Instant) {
        self.idle.retain(|_, entries| {
            entries.retain(|entry| {
                now.saturating_duration_since(entry.last_used) <= SFTP_POOL_IDLE_TTL
            });
            !entries.is_empty()
        });
    }
}

impl RemoteDirCreateCache {
    fn pending_chain(&mut self, remote_path: &str) -> Vec<String> {
        remote_dir_chain(remote_path)
            .into_iter()
            .filter(|dir| self.attempted.insert(dir.clone()))
            .collect()
    }
}

fn sftp_pool() -> &'static Mutex<SftpPool> {
    static POOL: OnceLock<Mutex<SftpPool>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(SftpPool::default()))
}

fn sftp_pool_key(mapping: &RemoteTransportMapping) -> Result<SftpPoolKey, String> {
    Ok(SftpPoolKey {
        host: mapping.host.clone(),
        port: mapping.ssh_port,
        username: resolved_ssh_username(mapping)?,
        auth: mapping.auth.as_config_str(),
        auth_detail: sftp_pool_auth_detail(mapping),
    })
}

fn sftp_pool_auth_detail(mapping: &RemoteTransportMapping) -> String {
    match mapping.auth {
        SshAuthKind::IdentityFile => format!(
            "{}:{}",
            mapping.ssh_identity_file,
            iris_core_domain::observability::stable_hash(mapping.ssh_password.as_bytes())
        ),
        SshAuthKind::Password => {
            iris_core_domain::observability::stable_hash(mapping.ssh_password.as_bytes())
        }
        SshAuthKind::Agent | SshAuthKind::DefaultKey => String::new(),
    }
}

async fn execute_sftp_entries(
    plan: &SftpTransferPlan,
    session: &SftpSession,
    transfer_session: Arc<RawSftpSession>,
    pipeline_depth: usize,
    progress: &mut SftpPlanProgress,
) -> Result<(), String> {
    let mut remote_dirs = RemoteDirCreateCache::default();
    for entry in &plan.entries[progress.completed..] {
        // Probe before any entry side effects, so a stale pooled connection can
        // reconnect at a known boundary without replaying earlier move entries.
        session
            .canonicalize(".")
            .await
            .map_err(|error| format!("SFTP entry readiness check failed: {error}"))?;
        progress.entry_started = true;
        match plan.direction {
            SftpTransferDirection::Upload => {
                if entry.is_dir {
                    upload_directory_tree(
                        session,
                        Arc::clone(&transfer_session),
                        pipeline_depth,
                        &entry.local_path,
                        &entry.remote_path,
                        &mut remote_dirs,
                    )
                    .await?;
                } else {
                    upload_file(
                        session,
                        Arc::clone(&transfer_session),
                        pipeline_depth,
                        &entry.local_path,
                        &entry.remote_path,
                        &mut remote_dirs,
                    )
                    .await?;
                }
                if plan.delete_sources {
                    remove_local_path(&entry.local_path).await?;
                }
            }
            SftpTransferDirection::Download => {
                if entry.is_dir {
                    download_directory_tree(
                        session,
                        Arc::clone(&transfer_session),
                        pipeline_depth,
                        &entry.remote_path,
                        &entry.local_path,
                    )
                    .await?;
                } else {
                    download_file(
                        session,
                        Arc::clone(&transfer_session),
                        pipeline_depth,
                        &entry.remote_path,
                        &entry.local_path,
                    )
                    .await?;
                }
                if plan.delete_sources {
                    remove_remote_path(session, &entry.remote_path, entry.is_dir).await?;
                }
            }
        }
        progress.completed += 1;
        progress.entry_started = false;
    }
    Ok(())
}

#[cfg(unix)]
async fn authenticate_with_agent(handle: &mut SshHandle, username: &str) -> Result<(), String> {
    use russh::keys::agent::client::AgentClient;
    use tokio::net::UnixStream;

    let agent_path = std::env::var_os("SSH_AUTH_SOCK")
        .ok_or_else(|| "SSH agent is not available (SSH_AUTH_SOCK is not set)".to_string())?;
    let stream = UnixStream::connect(agent_path)
        .await
        .map_err(|error| format!("failed to connect SSH agent: {error}"))?;
    let mut agent = AgentClient::connect(stream);
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| format!("failed to enumerate SSH agent identities: {error}"))?;
    if identities.is_empty() {
        return Err("SSH agent has no available identities".into());
    }

    let preferred_hash = preferred_rsa_hash(handle).await?;
    for identity in identities {
        let public_key = identity.public_key().into_owned();
        let result = handle
            .authenticate_publickey_with(
                username.to_string(),
                public_key.clone(),
                rsa_hash_for_algorithm(public_key.algorithm(), preferred_hash),
                &mut agent,
            )
            .await
            .map_err(|error| format!("SSH agent authentication failed: {error}"))?;
        if result.success() {
            debug!(
                username_hash = %iris_core_domain::observability::path_hash(username),
                "sftp.auth_agent_success"
            );
            return Ok(());
        }
    }

    Err(format!(
        "SSH agent authentication failed for {}@{}",
        username,
        if handle.is_closed() {
            "closed-session"
        } else {
            "remote-host"
        }
    ))
}

#[cfg(not(unix))]
async fn authenticate_with_agent(_handle: &mut SshHandle, _username: &str) -> Result<(), String> {
    Err("SSH agent authentication is not supported on this platform".into())
}

async fn authenticate_with_default_key(
    handle: &mut SshHandle,
    username: &str,
) -> Result<(), String> {
    let key_paths = default_private_key_paths();
    if key_paths.is_empty() {
        return Err("no default SSH private keys were found under ~/.ssh".into());
    }

    let preferred_hash = preferred_rsa_hash(handle).await?;
    for key_path in key_paths {
        if try_private_key(handle, username, &key_path, None, preferred_hash).await? {
            return Ok(());
        }
    }

    Err(format!(
        "default SSH key authentication failed for {}",
        username
    ))
}

/// Loads `key_path` and offers it; `Ok(false)` means the server rejected the key.
async fn try_private_key(
    handle: &mut SshHandle,
    username: &str,
    key_path: &Path,
    passphrase: Option<&str>,
    preferred_hash: Option<HashAlg>,
) -> Result<bool, String> {
    let private_key = russh::keys::load_secret_key(key_path, passphrase)
        .map_err(|error| format!("failed to load SSH key {}: {error}", key_path.display()))?;
    let key = Arc::new(private_key);
    let auth_key = PrivateKeyWithHashAlg::new(
        Arc::clone(&key),
        rsa_hash_for_algorithm(key.algorithm(), preferred_hash),
    );
    let accepted = handle
        .authenticate_publickey(username.to_string(), auth_key)
        .await
        .map_err(|error| {
            format!(
                "public key authentication failed with {}: {error}",
                key_path.display()
            )
        })?
        .success();
    if accepted {
        debug!(
            username_hash = %iris_core_domain::observability::path_hash(username),
            key_path_hash = %iris_core_domain::observability::stable_hash(key_path.as_os_str().as_encoded_bytes()),
            "sftp.auth_public_key_success"
        );
    }
    Ok(accepted)
}

async fn authenticate_with_identity_file(
    handle: &mut SshHandle,
    username: &str,
    mapping: &RemoteTransportMapping,
) -> Result<(), String> {
    let key_path = Path::new(mapping.ssh_identity_file.trim());
    if mapping.ssh_identity_file.trim().is_empty() {
        return Err("identity file is not configured".into());
    }

    let preferred_hash = preferred_rsa_hash(handle).await?;
    let passphrase = (!mapping.ssh_password.is_empty()).then_some(mapping.ssh_password.as_str());
    if try_private_key(handle, username, key_path, passphrase, preferred_hash).await? {
        return Ok(());
    }

    Err(format!(
        "identity-file authentication failed for {} with {}",
        username,
        key_path.display()
    ))
}

async fn authenticate_with_password(
    handle: &mut SshHandle,
    username: &str,
    mapping: &RemoteTransportMapping,
) -> Result<(), String> {
    if mapping.ssh_password.is_empty() {
        return Err("SSH password is not configured".into());
    }

    let result = handle
        .authenticate_password(username.to_string(), mapping.ssh_password.clone())
        .await
        .map_err(|error| format!("password authentication failed: {error}"))?;
    if result.success() {
        debug!(
            username_hash = %iris_core_domain::observability::path_hash(username),
            "sftp.auth_password_success"
        );
        return Ok(());
    }

    Err(format!("password authentication failed for {}", username))
}

async fn preferred_rsa_hash(handle: &SshHandle) -> Result<Option<HashAlg>, String> {
    handle
        .best_supported_rsa_hash()
        .await
        .map(|best| best.unwrap_or(Some(HashAlg::Sha512)))
        .map_err(|error| format!("failed to inspect SSH server signature algorithms: {error}"))
}

fn rsa_hash_for_algorithm(algorithm: Algorithm, preferred: Option<HashAlg>) -> Option<HashAlg> {
    if algorithm.is_rsa() {
        preferred.or(Some(HashAlg::Sha512))
    } else {
        None
    }
}

fn resolved_ssh_username(mapping: &RemoteTransportMapping) -> Result<String, String> {
    if !mapping.ssh_username.trim().is_empty() {
        return Ok(mapping.ssh_username.clone());
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .map_err(|_| "failed to resolve local SSH username from environment".to_string())
}

fn default_private_key_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let ssh_dir = home.join(".ssh");
    DEFAULT_KEY_CANDIDATES
        .iter()
        .map(|name| ssh_dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut joined = PathBuf::from(drive);
            joined.push(path);
            Some(joined)
        })
}

fn upload_directory_tree<'a>(
    session: &'a SftpSession,
    transfer_session: Arc<RawSftpSession>,
    pipeline_depth: usize,
    local_root: &'a Path,
    remote_root: &'a str,
    remote_dirs: &'a mut RemoteDirCreateCache,
) -> BoxFuture<'a, Result<(), String>> {
    Box::pin(async move {
        ensure_remote_dir_all(session, remote_root, remote_dirs).await?;
        let mut stack = vec![(local_root.to_path_buf(), remote_root.to_string())];

        while let Some((local_dir, remote_dir)) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&local_dir).await.map_err(|error| {
                format!("failed to read directory {}: {error}", local_dir.display())
            })?;
            while let Some(entry) = entries.next_entry().await.map_err(|error| {
                format!(
                    "failed to iterate directory {}: {error}",
                    local_dir.display()
                )
            })? {
                let child_local = entry.path();
                let metadata = tokio::fs::metadata(&child_local).await.map_err(|error| {
                    format!(
                        "failed to stat local path {}: {error}",
                        child_local.display()
                    )
                })?;
                let child_name = entry.file_name();
                let child_name = child_name.to_string_lossy();
                let child_remote = join_remote_path(&remote_dir, &child_name);
                if metadata.is_dir() {
                    ensure_remote_dir_all(session, &child_remote, remote_dirs).await?;
                    stack.push((child_local, child_remote));
                } else {
                    upload_file(
                        session,
                        Arc::clone(&transfer_session),
                        pipeline_depth,
                        &child_local,
                        &child_remote,
                        remote_dirs,
                    )
                    .await?;
                }
            }
        }

        Ok(())
    })
}

fn download_directory_tree<'a>(
    session: &'a SftpSession,
    transfer_session: Arc<RawSftpSession>,
    pipeline_depth: usize,
    remote_root: &'a str,
    local_root: &'a Path,
) -> BoxFuture<'a, Result<(), String>> {
    Box::pin(async move {
        tokio::fs::create_dir_all(local_root)
            .await
            .map_err(|error| {
                format!(
                    "failed to create directory {}: {error}",
                    local_root.display()
                )
            })?;
        let mut stack = vec![(remote_root.to_string(), local_root.to_path_buf())];

        while let Some((remote_dir, local_dir)) = stack.pop() {
            let entries = session
                .read_dir(remote_dir.clone())
                .await
                .map_err(|error| {
                    format!("failed to list remote directory {remote_dir}: {error}")
                })?;
            for entry in entries {
                let child_name = entry.file_name();
                if child_name == "." || child_name == ".." {
                    continue;
                }
                let child_remote = join_remote_path(&remote_dir, &child_name);
                let child_local = local_dir.join(&child_name);
                if entry.metadata().is_dir() {
                    tokio::fs::create_dir_all(&child_local)
                        .await
                        .map_err(|error| {
                            format!(
                                "failed to create directory {}: {error}",
                                child_local.display()
                            )
                        })?;
                    stack.push((child_remote, child_local));
                } else {
                    download_file(
                        session,
                        Arc::clone(&transfer_session),
                        pipeline_depth,
                        &child_remote,
                        &child_local,
                    )
                    .await?;
                }
            }
        }

        Ok(())
    })
}

async fn upload_file(
    session: &SftpSession,
    transfer_session: Arc<RawSftpSession>,
    pipeline_depth: usize,
    local_path: &Path,
    remote_path: &str,
    remote_dirs: &mut RemoteDirCreateCache,
) -> Result<(), String> {
    ensure_remote_parent(session, remote_path, remote_dirs).await?;
    pipelined_upload(transfer_session, local_path, remote_path, pipeline_depth).await
}

async fn download_file(
    session: &SftpSession,
    transfer_session: Arc<RawSftpSession>,
    pipeline_depth: usize,
    remote_path: &str,
    local_path: &Path,
) -> Result<(), String> {
    ensure_local_parent(local_path).await?;
    let size = session
        .metadata(remote_path.to_string())
        .await
        .map_err(|error| format!("failed to stat remote file {remote_path}: {error}"))?
        .size
        .ok_or_else(|| format!("remote file size is unavailable for {remote_path}"))?;
    pipelined_download(
        transfer_session,
        remote_path,
        local_path,
        size,
        pipeline_depth,
    )
    .await
}

async fn pipelined_upload(
    session: Arc<RawSftpSession>,
    local_path: &Path,
    remote_path: &str,
    pipeline_depth: usize,
) -> Result<(), String> {
    let mut local_file = tokio::fs::File::open(local_path).await.map_err(|error| {
        format!(
            "failed to open local file {}: {error}",
            local_path.display()
        )
    })?;
    let metadata = local_file.metadata().await.map_err(|error| {
        format!(
            "failed to stat local file {}: {error}",
            local_path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!("not a local file: {}", local_path.display()));
    }
    let handle = session
        .open(
            remote_path.to_string(),
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            FileAttributes::empty(),
        )
        .await
        .map_err(|error| format!("failed to create remote file {remote_path}: {error}"))?
        .handle;

    let transfer_result = async {
        let mut writes = JoinSet::new();
        let mut next_offset = 0u64;
        let mut reached_eof = false;

        loop {
            while writes.len() < pipeline_depth && !reached_eof {
                let mut data = vec![0; SFTP_PIPELINE_CHUNK_BYTES];
                let read = local_file.read(&mut data).await.map_err(|error| {
                    format!(
                        "failed to read local file {}: {error}",
                        local_path.display()
                    )
                })?;
                if read == 0 {
                    reached_eof = true;
                    break;
                }
                data.truncate(read);
                let request_offset = next_offset;
                next_offset = next_offset.saturating_add(read as u64);
                let request_session = Arc::clone(&session);
                let request_handle = handle.clone();
                writes.spawn(async move {
                    request_session
                        .write(request_handle, request_offset, data)
                        .await
                        .map(|_| ())
                        .map_err(|error| {
                            format!("SFTP write failed at offset {request_offset}: {error}")
                        })
                });
            }

            let Some(completed) = writes.join_next().await else {
                break;
            };
            match completed {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    writes.abort_all();
                    return Err(error);
                }
                Err(error) => {
                    writes.abort_all();
                    return Err(format!("SFTP write task failed: {error}"));
                }
            }
        }
        Ok(())
    }
    .await;

    close_raw_file(
        &session,
        handle,
        transfer_result,
        &format!("remote file {remote_path}"),
    )
    .await
}

async fn pipelined_download(
    session: Arc<RawSftpSession>,
    remote_path: &str,
    local_path: &Path,
    file_size: u64,
    pipeline_depth: usize,
) -> Result<(), String> {
    let handle = session
        .open(
            remote_path.to_string(),
            OpenFlags::READ,
            FileAttributes::empty(),
        )
        .await
        .map_err(|error| format!("failed to open remote file {remote_path}: {error}"))?
        .handle;

    let transfer_result = async {
        let mut local_file = tokio::fs::File::create(local_path).await.map_err(|error| {
            format!(
                "failed to create local file {}: {error}",
                local_path.display()
            )
        })?;
        local_file.set_len(file_size).await.map_err(|error| {
            format!(
                "failed to size local file {}: {error}",
                local_path.display()
            )
        })?;
        let mut reads = JoinSet::new();
        let mut request_offset = 0u64;
        let mut write_offset = 0u64;
        let mut completed_chunks = BTreeMap::<u64, Vec<u8>>::new();

        while request_offset < file_size || !reads.is_empty() {
            while reads.len() + completed_chunks.len() < pipeline_depth
                && request_offset < file_size
            {
                let remaining = file_size - request_offset;
                let length = remaining.min(SFTP_PIPELINE_CHUNK_BYTES as u64) as usize;
                let chunk_offset = request_offset;
                request_offset += length as u64;
                let request_session = Arc::clone(&session);
                let request_handle = handle.clone();
                reads.spawn(async move {
                    read_sftp_range(request_session, request_handle, chunk_offset, length)
                        .await
                        .map(|data| (chunk_offset, data))
                });
            }

            let Some(completed) = reads.join_next().await else {
                break;
            };
            let (chunk_offset, data) = match completed {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(error)) => {
                    reads.abort_all();
                    return Err(error);
                }
                Err(error) => {
                    reads.abort_all();
                    return Err(format!("SFTP read task failed: {error}"));
                }
            };
            completed_chunks.insert(chunk_offset, data);

            while let Some(data) = completed_chunks.remove(&write_offset) {
                local_file.write_all(&data).await.map_err(|error| {
                    format!(
                        "failed to write local file {} at offset {write_offset}: {error}",
                        local_path.display()
                    )
                })?;
                write_offset += data.len() as u64;
            }
        }

        if write_offset != file_size {
            return Err(format!(
                "SFTP download ended at {write_offset} bytes, expected {file_size}"
            ));
        }
        local_file.flush().await.map_err(|error| {
            format!(
                "failed to flush local file {}: {error}",
                local_path.display()
            )
        })?;
        Ok(())
    }
    .await;

    close_raw_file(
        &session,
        handle,
        transfer_result,
        &format!("remote file {remote_path}"),
    )
    .await
}

async fn read_sftp_range(
    session: Arc<RawSftpSession>,
    handle: String,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(length);
    while data.len() < length {
        let read_offset = offset + data.len() as u64;
        let remaining = length - data.len();
        let response = session
            .read(handle.clone(), read_offset, remaining as u32)
            .await
            .map_err(|error| format!("SFTP read failed at offset {read_offset}: {error}"))?;
        if response.data.is_empty() {
            return Err(format!(
                "SFTP read returned no data at offset {read_offset}"
            ));
        }
        if response.data.len() > remaining {
            return Err(format!(
                "SFTP read returned {} bytes for a {remaining}-byte request at offset {read_offset}",
                response.data.len()
            ));
        }
        data.extend_from_slice(&response.data);
    }
    Ok(data)
}

async fn close_raw_file(
    session: &RawSftpSession,
    handle: String,
    transfer_result: Result<(), String>,
    description: &str,
) -> Result<(), String> {
    let close_result = session
        .close(handle)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to close {description}: {error}"));
    match (transfer_result, close_result) {
        (Ok(()), result) => result,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(close_error)) => Err(format!("{error}; {close_error}")),
    }
}

async fn ensure_local_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("failed to create directory {}: {error}", parent.display()))?;
    }
    Ok(())
}

async fn ensure_remote_parent(
    session: &SftpSession,
    remote_path: &str,
    remote_dirs: &mut RemoteDirCreateCache,
) -> Result<(), String> {
    if let Some(parent) = remote_parent(remote_path) {
        ensure_remote_dir_all(session, &parent, remote_dirs).await?;
    }
    Ok(())
}

async fn ensure_remote_dir_all(
    session: &SftpSession,
    remote_path: &str,
    remote_dirs: &mut RemoteDirCreateCache,
) -> Result<(), String> {
    for current in remote_dirs.pending_chain(remote_path) {
        match session.create_dir(current.clone()).await {
            Ok(()) => {}
            Err(SftpError::Status(status)) if status.status_code == StatusCode::Failure => {
                let metadata = session.metadata(current.clone()).await.map_err(|error| {
                    format!("failed to confirm remote directory {current}: {error}")
                })?;
                if !metadata.is_dir() {
                    return Err(format!(
                        "remote path exists but is not a directory: {current}"
                    ));
                }
            }
            Err(error) => {
                return Err(format!(
                    "failed to create remote directory {current}: {error}"
                ));
            }
        }
    }

    Ok(())
}

fn remote_dir_chain(remote_path: &str) -> Vec<String> {
    let normalized = remote_path.trim_end_matches('/');
    if normalized.is_empty() || normalized == "/" {
        return Vec::new();
    }

    let mut dirs = Vec::new();
    let mut current = if normalized.starts_with('/') {
        "/".to_string()
    } else {
        String::new()
    };
    for segment in normalized.split('/').filter(|segment| !segment.is_empty()) {
        current = join_remote_path(&current, segment);
        dirs.push(current.clone());
    }
    dirs
}

fn remove_remote_path<'a>(
    session: &'a SftpSession,
    remote_path: &'a str,
    is_dir: bool,
) -> BoxFuture<'a, Result<(), String>> {
    Box::pin(async move {
        if !is_dir {
            session
                .remove_file(remote_path.to_string())
                .await
                .map_err(|error| format!("failed to remove remote file {remote_path}: {error}"))?;
            return Ok(());
        }

        let mut stack = vec![(remote_path.to_string(), false)];
        while let Some((path, visited)) = stack.pop() {
            if visited {
                session.remove_dir(path.clone()).await.map_err(|error| {
                    format!("failed to remove remote directory {path}: {error}")
                })?;
                continue;
            }

            stack.push((path.clone(), true));
            let entries = session
                .read_dir(path.clone())
                .await
                .map_err(|error| format!("failed to list remote directory {path}: {error}"))?;
            for entry in entries {
                let child_name = entry.file_name();
                if child_name == "." || child_name == ".." {
                    continue;
                }
                let child_path = join_remote_path(&path, &child_name);
                if entry.metadata().is_dir() {
                    stack.push((child_path, false));
                } else {
                    session
                        .remove_file(child_path.clone())
                        .await
                        .map_err(|error| {
                            format!("failed to remove remote file {child_path}: {error}")
                        })?;
                }
            }
        }

        Ok(())
    })
}

async fn remove_local_path(path: &Path) -> Result<(), String> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| format!("failed to stat source path {}: {error}", path.display()))?;
    if metadata.is_dir() {
        tokio::fs::remove_dir_all(path).await.map_err(|error| {
            format!(
                "failed to remove source directory {}: {error}",
                path.display()
            )
        })
    } else {
        tokio::fs::remove_file(path)
            .await
            .map_err(|error| format!("failed to remove source file {}: {error}", path.display()))
    }
}

fn remote_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return None;
    }
    let idx = trimmed.rfind('/')?;
    if idx == 0 {
        Some("/".into())
    } else {
        Some(trimmed[..idx].to_string())
    }
}

fn join_remote_path(base: &str, segment: &str) -> String {
    if base.is_empty() {
        return segment.to_string();
    }
    if base == "/" {
        return format!("/{}", segment);
    }
    format!("{}/{}", base.trim_end_matches('/'), segment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteTransportKind;
    use russh_sftp::protocol::{Data, File, Handle, Name, Status};
    use russh_sftp::server::StatusReply;
    use tokio::sync::Mutex as TokioMutex;

    #[test]
    fn default_private_key_candidates_exclude_rsa() {
        assert_eq!(DEFAULT_KEY_CANDIDATES, ["id_ed25519", "id_ecdsa"]);
    }

    #[derive(Default)]
    struct MemoryFaults {
        probes: usize,
        fail_probe_at: Option<usize>,
        fail_close_once: Option<String>,
        opened: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct MemorySftpServer {
        files: Arc<TokioMutex<HashMap<String, Vec<u8>>>>,
        faults: Arc<Mutex<MemoryFaults>>,
    }

    impl russh_sftp::server::Handler for MemorySftpServer {
        type Error = StatusReply;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported.into()
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            let mut faults = self.faults.lock().unwrap();
            faults.probes += 1;
            if faults.fail_probe_at == Some(faults.probes) {
                faults.fail_probe_at = None;
                return Err(
                    StatusCode::Failure.with_message("connection reset (injected before entry)")
                );
            }
            Ok(Name {
                id,
                files: vec![File::new(path, FileAttributes::empty())],
            })
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            flags: OpenFlags,
            _attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            self.faults.lock().unwrap().opened.push(filename.clone());
            let mut files = self.files.lock().await;
            if flags.contains(OpenFlags::TRUNCATE) {
                files.insert(filename.clone(), Vec::new());
            } else if !files.contains_key(&filename) && !flags.contains(OpenFlags::CREATE) {
                return Err(StatusCode::NoSuchFile.into());
            } else {
                files.entry(filename.clone()).or_default();
            }
            Ok(Handle {
                id,
                handle: filename,
            })
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            let mut faults = self.faults.lock().unwrap();
            if faults.fail_close_once.as_deref() == Some(&handle) {
                faults.fail_close_once = None;
                return Err(
                    StatusCode::Failure.with_message("connection reset (injected after close)")
                );
            }
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let files = self.files.lock().await;
            let file = files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
            let start = offset as usize;
            if start >= file.len() {
                return Err(StatusCode::Eof.into());
            }
            let short_read_limit = 31_337usize;
            let end = start
                .saturating_add(len as usize)
                .min(start + short_read_limit)
                .min(file.len());
            Ok(Data {
                id,
                data: file[start..end].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let mut files = self.files.lock().await;
            let file = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
            let start = offset as usize;
            let end = start.saturating_add(data.len());
            if file.len() < end {
                file.resize(end, 0);
            }
            file[start..end].copy_from_slice(&data);
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    async fn memory_sftp_session() -> (Arc<RawSftpSession>, MemorySftpServer) {
        let (client, server) = tokio::io::duplex(4 * 1024 * 1024);
        let handler = MemorySftpServer::default();
        russh_sftp::server::run(server, handler.clone()).await;
        let session = RawSftpSession::new(client);
        session.init().await.expect("initialize memory SFTP");
        (Arc::new(session), handler)
    }

    async fn memory_control_session(server: &MemorySftpServer) -> SftpSession {
        let (client, stream) = tokio::io::duplex(4 * 1024 * 1024);
        russh_sftp::server::run(stream, server.clone()).await;
        SftpSession::new(client)
            .await
            .expect("memory control session")
    }

    fn mapping_with_auth(auth: SshAuthKind) -> RemoteTransportMapping {
        RemoteTransportMapping {
            id: "mapping-1".into(),
            local_mount_path: "/Volumes/share".into(),
            host: "nas.local".into(),
            ssh_port: 2222,
            ssh_username: "alice".into(),
            remote_base_path: "/volume1/share".into(),
            transport: RemoteTransportKind::Sftp,
            auth,
            ssh_identity_file: "/Users/alice/.ssh/id_ed25519".into(),
            ssh_password: "top-secret-passphrase".into(),
            enabled: true,
            auto_detected: false,
            notes: String::new(),
        }
    }

    #[test]
    fn remote_parent_handles_root_and_nested_paths() {
        assert_eq!(
            remote_parent("/volume1/data/file.txt"),
            Some("/volume1/data".into())
        );
        assert_eq!(remote_parent("/file.txt"), Some("/".into()));
        assert_eq!(remote_parent("/"), None);
        assert_eq!(remote_parent("relative"), None);
    }

    #[test]
    fn join_remote_path_preserves_root() {
        assert_eq!(join_remote_path("/", "album"), "/album");
        assert_eq!(
            join_remote_path("/volume1/data", "album"),
            "/volume1/data/album"
        );
        assert_eq!(join_remote_path("", "album"), "album");
    }

    #[test]
    fn remote_dir_chain_returns_incremental_directories() {
        assert_eq!(
            remote_dir_chain("/volume1/data/album"),
            vec!["/volume1", "/volume1/data", "/volume1/data/album"]
        );
        assert_eq!(
            remote_dir_chain("relative/data"),
            vec!["relative", "relative/data"]
        );
        assert!(remote_dir_chain("/").is_empty());
        assert!(remote_dir_chain("").is_empty());
    }

    #[test]
    fn remote_dir_create_cache_skips_previously_attempted_segments() {
        let mut cache = RemoteDirCreateCache::default();

        assert_eq!(
            cache.pending_chain("/volume1/data/album"),
            vec!["/volume1", "/volume1/data", "/volume1/data/album"]
        );
        assert_eq!(
            cache.pending_chain("/volume1/data/raw"),
            vec!["/volume1/data/raw"]
        );
        assert!(cache.pending_chain("/volume1/data/album").is_empty());
    }

    #[test]
    fn default_key_candidates_live_under_dot_ssh() {
        let paths = default_private_key_paths();
        for path in paths {
            assert!(path.to_string_lossy().contains(".ssh/"));
        }
    }

    #[test]
    fn ssh_client_config_enables_keepalive_and_nodelay() {
        let config = ssh_client_config();

        assert_eq!(
            config.keepalive_interval,
            Some(Duration::from_secs(KEEPALIVE_SECS))
        );
        assert!(config.nodelay);
        assert_eq!(config.window_size, 2 * 1024 * 1024);
        assert_eq!(config.maximum_packet_size, SFTP_PIPELINE_CHUNK_BYTES as u32);
    }

    #[test]
    fn pipeline_depth_stays_within_protocol_tuning_bounds() {
        assert_eq!(normalize_pipeline_depth(0), DEFAULT_SFTP_PIPELINE_DEPTH);
        assert_eq!(normalize_pipeline_depth(12), 12);
        assert_eq!(
            normalize_pipeline_depth(usize::MAX),
            MAX_SFTP_PIPELINE_DEPTH
        );
    }

    #[test]
    fn sftp_pool_key_hashes_password_without_storing_secret() {
        let mapping = mapping_with_auth(SshAuthKind::Password);
        let key = sftp_pool_key(&mapping).expect("pool key");

        assert_eq!(key.host, "nas.local");
        assert_eq!(key.port, 2222);
        assert_eq!(key.username, "alice");
        assert_eq!(key.auth, "Password");
        assert_ne!(key.auth_detail, mapping.ssh_password);
        assert_eq!(
            key.auth_detail,
            iris_core_domain::observability::stable_hash(mapping.ssh_password.as_bytes())
        );
        assert!(!key.auth_detail.contains("top-secret"));
    }

    #[test]
    fn sftp_pool_key_includes_identity_file_and_hashed_passphrase() {
        let mapping = mapping_with_auth(SshAuthKind::IdentityFile);
        let key = sftp_pool_key(&mapping).expect("pool key");

        assert_eq!(key.auth, "IdentityFile");
        assert!(key.auth_detail.contains("/Users/alice/.ssh/id_ed25519"));
        assert!(!key.auth_detail.contains("top-secret"));
        assert!(key
            .auth_detail
            .contains(&iris_core_domain::observability::stable_hash(
                mapping.ssh_password.as_bytes()
            )));
    }

    #[tokio::test]
    async fn pooled_move_never_replays_committed_entries() {
        // Exercise the same progress/reused-lease gate as execute_sftp_plan with
        // in-memory SFTP sessions, including a replacement session on safe retry.
        for ambiguous in [false, true] {
            let (transfer, server) = memory_sftp_session().await;
            let control = memory_control_session(&server).await;
            let temp = tempfile::tempdir().expect("tempdir");
            let entries = ["first", "second"].map(|name| SftpTransferEntry {
                local_path: temp.path().join(name),
                remote_path: format!("/{name}"),
                is_dir: false,
            });
            for entry in &entries {
                tokio::fs::write(&entry.local_path, entry.remote_path.as_bytes())
                    .await
                    .expect("write move source");
            }
            {
                let mut faults = server.faults.lock().unwrap();
                if ambiguous {
                    // The second payload is committed but its close reply is lost.
                    faults.fail_close_once = Some("/second".into());
                } else {
                    faults.fail_probe_at = Some(2);
                }
            }
            let plan = SftpTransferPlan {
                mapping: mapping_with_auth(SshAuthKind::Password),
                direction: SftpTransferDirection::Upload,
                delete_sources: true,
                pipeline_depth: DEFAULT_SFTP_PIPELINE_DEPTH,
                entries: entries.to_vec(),
            };
            let mut progress = SftpPlanProgress::default();
            let error = execute_sftp_entries(
                &plan,
                &control,
                Arc::clone(&transfer),
                plan.pipeline_depth,
                &mut progress,
            )
            .await
            .expect_err("injected connection failure");
            assert!(sftp_error_is_connection_failure(&error), "{error}");
            assert_eq!(progress.completed, 1);
            assert!(!entries[0].local_path.exists(), "first move source removed");
            assert!(entries[1].local_path.exists(), "uncertain source retained");
            assert_eq!(progress.can_retry(true, &error), !ambiguous);
            assert!(
                !progress.can_retry(false, &error),
                "fresh leases are not retried"
            );
            if progress.can_retry(true, &error) {
                control.close().await.expect("close stale control");
                transfer.close_session().expect("close stale transfer");
                let control = memory_control_session(&server).await;
                let (client, stream) = tokio::io::duplex(4 * 1024 * 1024);
                russh_sftp::server::run(stream, server.clone()).await;
                let transfer = Arc::new(RawSftpSession::new(client));
                transfer.init().await.expect("replacement transfer session");
                execute_sftp_entries(
                    &plan,
                    &control,
                    Arc::clone(&transfer),
                    plan.pipeline_depth,
                    &mut progress,
                )
                .await
                .expect("retry remaining entry");
                assert_eq!(progress.completed, 2);
                assert!(!entries[1].local_path.exists());
                control.close().await.expect("close replacement control");
                transfer
                    .close_session()
                    .expect("close replacement transfer");
            } else {
                control.close().await.expect("close failed control");
                transfer.close_session().expect("close failed transfer");
            }
            assert_eq!(server.files.lock().await.get("/first").unwrap(), b"/first");
            assert_eq!(
                server.files.lock().await.get("/second").unwrap(),
                b"/second"
            );
            assert_eq!(server.faults.lock().unwrap().opened, ["/first", "/second"]);
        }
    }

    #[tokio::test]
    async fn upload_validates_source_before_opening_remote_destination() {
        let (session, server) = memory_sftp_session().await;
        server
            .files
            .lock()
            .await
            .insert("/existing".into(), b"keep me".to_vec());
        let temp = tempfile::tempdir().expect("tempdir");
        for source in [temp.path().join("missing"), temp.path().to_path_buf()] {
            pipelined_upload(
                Arc::clone(&session),
                &source,
                "/existing",
                DEFAULT_SFTP_PIPELINE_DEPTH,
            )
            .await
            .expect_err("missing or non-file source");
            assert_eq!(
                server.files.lock().await.get("/existing").unwrap(),
                b"keep me"
            );
            assert!(server.faults.lock().unwrap().opened.is_empty());
        }
        session.close_session().expect("close memory SFTP");
    }

    #[tokio::test]
    async fn pipelined_transfer_round_trips_multiple_windows_and_short_reads() {
        let (session, server) = memory_sftp_session().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let payload_len = SFTP_PIPELINE_CHUNK_BYTES * (DEFAULT_SFTP_PIPELINE_DEPTH + 2) + 17_321;
        let payload = (0..payload_len)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        tokio::fs::write(&source, &payload)
            .await
            .expect("write source");

        pipelined_upload(
            Arc::clone(&session),
            &source,
            "/roundtrip.bin",
            DEFAULT_SFTP_PIPELINE_DEPTH,
        )
        .await
        .expect("pipelined upload");
        assert_eq!(
            server.files.lock().await.get("/roundtrip.bin").cloned(),
            Some(payload.clone())
        );

        pipelined_download(
            Arc::clone(&session),
            "/roundtrip.bin",
            &destination,
            payload.len() as u64,
            DEFAULT_SFTP_PIPELINE_DEPTH,
        )
        .await
        .expect("pipelined download");
        assert_eq!(
            tokio::fs::read(destination)
                .await
                .expect("read destination"),
            payload
        );
        session.close_session().expect("close memory SFTP");
    }
}
