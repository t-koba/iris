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
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
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

/// Overwrite-oriented batch transfer. Files are staged individually, not as one
/// atomic directory transaction. Remote replacement requires the server's
/// advertised OpenSSH POSIX-rename extension; standard SFTP rename is used only
/// without replacement. Upload stages stay owner-only during payload writes and
/// through close; final permissions are restored before publication. Existing
/// regular-file rwx bits are preserved, not ownership, ACLs or privilege bits;
/// symlink/non-regular targets are rejected. Lost publication replies are uncertain.
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
    transfer_session: Arc<SftpTransferSession>,
}

// The batch shortcut requests Overwrite only. Standard SFTP v3 rename cannot
// replace an existing entry; never emulate replacement by unlinking it first.
struct SftpTransferSession {
    raw: RawSftpSession,
    posix_rename: bool,
}

impl std::ops::Deref for SftpTransferSession {
    type Target = RawSftpSession;

    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

impl SftpTransferSession {
    async fn init(raw: RawSftpSession) -> Result<Arc<Self>, SftpError> {
        let version = raw.init().await?;
        Ok(Arc::new(Self {
            raw,
            posix_rename: version
                .extensions
                .get("posix-rename@openssh.com")
                .is_some_and(|version| version == "1"),
        }))
    }

    async fn publish(&self, staging: &str, destination: &str) -> Result<(), String> {
        let result = if self.posix_rename {
            let mut data = Vec::new();
            for path in [staging, destination] {
                // SFTP strings have a protocol-defined u32 byte length.
                let length = u32::try_from(path.len())
                    .map_err(|_| "SFTP rename path exceeds the protocol string length")?;
                data.extend_from_slice(&length.to_be_bytes());
                data.extend_from_slice(path.as_bytes());
            }
            match self.raw.extended("posix-rename@openssh.com", data).await {
                Ok(Packet::Status(status)) if status.status_code == StatusCode::Ok => Ok(()),
                Ok(Packet::Status(status)) => Err(SftpError::Status(status)),
                Ok(_) => Err(SftpError::UnexpectedPacket),
                Err(error) => Err(error),
            }
        } else {
            self.raw.rename(staging, destination).await.map(|_| ())
        };
        let rename_strategy = if self.posix_rename {
            "server-advertised POSIX rename was attempted"
        } else {
            "server did not advertise POSIX rename; standard SFTP rename was attempted"
        };
        result.map_err(|error| {
            format!(
                "failed to publish remote file {destination}: {error}; {rename_strategy}; \
                 publication outcome may be uncertain and Iris did not unlink the destination"
            )
        })
    }
}

// Ownership begins only after an exclusive OPEN is acknowledged. A lost OPEN
// reply can leave staging data, but must never cause deletion of an unowned path.
struct RemoteStagingFile {
    session: Arc<SftpTransferSession>,
    path: String,
    handle: Option<String>,
    owned: bool,
}

impl RemoteStagingFile {
    // Use the owned handle until CLOSE, then the staging path. No uid/gid or
    // non-portable ACL attributes are copied. These are observed POSIX modes,
    // not protection against concurrent third-party metadata/path changes.
    async fn mode(&self) -> Result<u32, String> {
        let reply = match &self.handle {
            Some(handle) => self.session.fstat(handle).await,
            None => self.session.lstat(&self.path).await,
        }
        .map_err(|error| format!("failed to verify SFTP staging mode: {error}"))?;
        reply
            .attrs
            .permissions
            .map(|mode| mode & 0o7777)
            .ok_or_else(|| "server did not report SFTP staging mode".into())
    }

    async fn set_mode(&self, mode: u32) -> Result<(), String> {
        if self.mode().await? == mode {
            return Ok(());
        }
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(mode);
        match &self.handle {
            Some(handle) => self.session.fsetstat(handle, attrs).await,
            None => self.session.setstat(&self.path, attrs).await,
        }
        .map_err(|error| format!("failed to establish SFTP staging mode: {error}"))?;
        if self.mode().await? != mode {
            return Err("server did not establish the required SFTP staging mode".into());
        }
        Ok(())
    }

    async fn cleanup(&mut self, error: String) -> Result<(), String> {
        // Final-mode restoration may have succeeded even if its reply or the
        // subsequent verification/publication failed. Re-private best-effort
        // before removal so failed cleanup need not leave readable payload.
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(0o600);
        let _ = self.session.setstat(&self.path, attrs).await;
        let cleanup = self.session.remove(self.path.clone()).await;
        self.owned = false;
        match cleanup {
            Ok(_) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; failed to clean SFTP staging file {}: {cleanup_error}",
                self.path
            )),
        }
    }
}

impl Drop for RemoteStagingFile {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        // Cancellation cannot await cleanup. Never publish from this task, and
        // only remove the exclusively created sibling, not the destination.
        let session = Arc::clone(&self.session);
        let path = self.path.clone();
        let handle = self.handle.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(handle) = handle {
                    let _ = session.close(handle).await;
                }
                let mut attrs = FileAttributes::empty();
                attrs.permissions = Some(0o600);
                let _ = session.setstat(&path, attrs).await;
                let _ = session.remove(path).await;
            });
        }
    }
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
    let transfer_session = SftpTransferSession::init(transfer_session)
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
        transfer_session,
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

    fn transfer_session(&self) -> Arc<SftpTransferSession> {
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
    transfer_session: Arc<SftpTransferSession>,
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
    transfer_session: Arc<SftpTransferSession>,
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
    transfer_session: Arc<SftpTransferSession>,
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
    transfer_session: Arc<SftpTransferSession>,
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
    transfer_session: Arc<SftpTransferSession>,
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

// SFTP v3 permissions contain the POSIX type and mode. Use LSTAT: following a
// symlink would select permissions from an inode that rename does not replace.
async fn upload_mode(session: &SftpTransferSession, path: &str) -> Result<Option<u32>, String> {
    match session.lstat(path).await {
        Ok(reply) => match reply.attrs.permissions {
            // These masks are defined by the SFTP v3 POSIX mode format.
            Some(mode) if mode & 0o170000 == 0o100000 => Ok(Some(mode & 0o777)),
            _ => Err(format!(
                "cannot safely replace remote file {path}: LSTAT must report a regular file and its mode; symlinks and non-regular targets are not replaced"
            )),
        },
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
        Err(error) => Err(format!("failed to lstat remote destination {path}: {error}")),
    }
}

async fn pipelined_upload(
    session: Arc<SftpTransferSession>,
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
    let mode = upload_mode(&session, remote_path).await?;
    let mut attrs = FileAttributes::empty();
    // New files need the ordinary server-created mode captured from the empty
    // stage. Existing targets can request private creation immediately. OPEN
    // attributes are not trusted: privacy is verified before any payload.
    attrs.permissions = mode.map(|_| 0o600);
    let mut random = [0; 16];
    getrandom::fill(&mut random)
        .map_err(|error| format!("failed to name SFTP staging file: {error}"))?;
    let staging_path = join_remote_path(
        &remote_parent(remote_path).unwrap_or_default(),
        &format!(".iris-sftp-{:032x}", u128::from_ne_bytes(random)),
    );
    let handle = session
        .open(
            staging_path.clone(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            attrs,
        )
        .await
        .map_err(|error| {
            format!(
                "failed to exclusively create SFTP staging file {staging_path}: {error}; \
             a lost acknowledgement may leave staging data"
            )
        })?
        .handle;
    let mut staging = RemoteStagingFile {
        session: Arc::clone(&session),
        path: staging_path,
        handle: Some(handle.clone()),
        owned: true,
    };

    let transfer_result = async {
        let final_mode = match mode {
            Some(mode) => mode,
            None => staging.mode().await? & 0o777,
        };
        staging.set_mode(0o600).await?;
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
        if staging.mode().await? != 0o600 {
            return Err("SFTP staging mode changed during payload writes".into());
        }
        Ok(final_mode)
    }
    .await;

    let close_result = close_raw_file(
        &session,
        handle,
        transfer_result.as_ref().map(|_| ()).map_err(Clone::clone),
        &format!("SFTP staging file {}", staging.path),
    )
    .await;
    staging.handle = None;
    let result = async {
        close_result?;
        // Only a successful CLOSE permits broadening the private stage. Restore
        // and verify the final mode on the path, then immediately publish.
        staging.set_mode(transfer_result?).await?;
        session.publish(&staging.path, remote_path).await
    }
    .await;
    match result {
        Ok(()) => {
            staging.owned = false;
            Ok(())
        }
        Err(error) => staging.cleanup(error).await,
    }
}

// Atomic replacement cannot preserve inode ownership, ACLs or hard links like
// truncation did. Preserve regular-file rwx bits, but never copy privilege bits
// onto a new inode. Symlinks are replaced, not followed for permission selection.
#[cfg(unix)]
fn download_permissions(local_path: &Path, parent: &Path) -> std::io::Result<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::symlink_metadata(local_path) {
        Ok(metadata) if metadata.is_file() => {
            return Ok(std::fs::Permissions::from_mode(
                metadata.permissions().mode() & 0o777,
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Let the filesystem apply 0666-and-umask (and directory defaults) without
    // reading/changing the process-wide umask. This probe stays empty and is
    // removed on drop: payload staging must remain private throughout transfer.
    let probe = tempfile::Builder::new()
        .prefix(".iris-sftp-mode-")
        .permissions(std::fs::Permissions::from_mode(0o666))
        .tempfile_in(parent)?;
    Ok(probe.as_file().metadata()?.permissions())
}

async fn pipelined_download(
    session: Arc<SftpTransferSession>,
    remote_path: &str,
    local_path: &Path,
    file_size: u64,
    pipeline_depth: usize,
) -> Result<(), String> {
    let parent = local_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".iris-sftp-")
        .tempfile_in(parent)
        .map_err(|error| format!("failed to create local staging file: {error}"))?;
    let (local_file, staging) = staging.into_parts();
    let mut local_file = tokio::fs::File::from_std(local_file);
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

    let result = close_raw_file(
        &session,
        handle,
        transfer_result,
        &format!("remote file {remote_path}"),
    )
    .await;
    // Finish Tokio's pending filesystem operations and close the local handle
    // before installation (also required on Windows).
    let local_file = local_file.into_std().await;
    result?;
    #[cfg(unix)]
    {
        let permissions = download_permissions(local_path, parent)
            .and_then(|permissions| local_file.set_permissions(permissions));
        permissions.map_err(|error| {
            format!(
                "failed to set download permissions for {}: {error}",
                local_path.display()
            )
        })?;
    }
    drop(local_file);
    staging.persist(local_path).map_err(|error| {
        format!(
            "failed to publish local file {}: {error}",
            local_path.display()
        )
    })
}

async fn read_sftp_range(
    session: Arc<SftpTransferSession>,
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
    use russh_sftp::protocol::{Attrs, Data, File, Handle, Name, Status, Version};
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
        closes: usize,
        fail_close_at: Option<usize>,
        fail_publish_reply_for: Option<String>,
        fail_publish: bool,
        fail_remove: bool,
        fail_io: bool,
        pause_io: bool,
        no_posix_rename: bool,
        occupy_staging: bool,
        modes: HashMap<String, Option<u32>>,
        fail_lstat: bool,
        fail_fstat: bool,
        omit_fstat_mode: bool,
        ignore_open_mode: bool,
        create_umask: u32,
        fail_fsetstat: bool,
        ignore_fsetstat: bool,
        mode_changes: usize,
        final_mode_fault: Option<&'static str>,
        path_mode_changes: Vec<u32>,
        closed_modes: Vec<Option<u32>>,
        lstatted: Vec<String>,
        written_modes: Vec<Option<u32>>,
        broaden_mode_on_write: bool,
        listed: HashSet<String>,
        opened: Vec<String>,
        published: Vec<String>,
        removed: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct MemorySftpServer {
        files: Arc<TokioMutex<HashMap<String, Vec<u8>>>>,
        faults: Arc<Mutex<MemoryFaults>>,
        io_started: Arc<tokio::sync::Notify>,
        io_resume: Arc<tokio::sync::Notify>,
    }

    fn memory_ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        }
    }

    impl MemorySftpServer {
        async fn check_io(&self, offset: u64) -> Result<(), StatusReply> {
            let (pause, fail) = {
                let faults = self.faults.lock().unwrap();
                (faults.pause_io, faults.fail_io)
            };
            if pause {
                self.io_started.notify_one();
                self.io_resume.notified().await;
            }
            if fail && offset >= SFTP_PIPELINE_CHUNK_BYTES as u64 {
                return Err(StatusCode::Failure.with_message("injected transfer failure"));
            }
            Ok(())
        }

        async fn publish(
            &self,
            id: u32,
            old: String,
            new: String,
            replace: bool,
        ) -> Result<Status, StatusReply> {
            if self.faults.lock().unwrap().fail_publish {
                return Err(StatusCode::Failure.with_message("injected rename failure"));
            }
            let mut files = self.files.lock().await;
            if !replace && files.contains_key(&new) {
                return Err(StatusCode::Failure.with_message("destination exists"));
            }
            let data = files.remove(&old).ok_or(StatusCode::NoSuchFile)?;
            files.insert(new.clone(), data);
            let mut faults = self.faults.lock().unwrap();
            let mode = faults.modes.remove(&old).unwrap_or(Some(0o100644));
            faults.modes.insert(new.clone(), mode);
            faults.published.push(new.clone());
            if faults.fail_publish_reply_for.as_deref() == Some(&new) {
                faults.fail_publish_reply_for = None;
                return Err(
                    StatusCode::Failure.with_message("connection reset (injected after rename)")
                );
            }
            Ok(memory_ok(id))
        }
    }

    impl russh_sftp::server::Handler for MemorySftpServer {
        type Error = StatusReply;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported.into()
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: HashMap<String, String>,
        ) -> Result<Version, Self::Error> {
            let mut version = Version::new();
            if !self.faults.lock().unwrap().no_posix_rename {
                version
                    .extensions
                    .insert("posix-rename@openssh.com".into(), "1".into());
            }
            Ok(version)
        }

        async fn extended(
            &mut self,
            id: u32,
            request: String,
            data: Vec<u8>,
        ) -> Result<Packet, Self::Error> {
            if request != "posix-rename@openssh.com" {
                return Err(StatusCode::OpUnsupported.into());
            }
            let mut input = data.as_slice();
            let mut paths = Vec::new();
            for _ in 0..2 {
                let length = u32::from_be_bytes(input[..4].try_into().unwrap()) as usize;
                paths.push(String::from_utf8(input[4..4 + length].to_vec()).unwrap());
                input = &input[4 + length..];
            }
            assert!(input.is_empty());
            self.publish(id, paths[0].clone(), paths[1].clone(), true)
                .await
                .map(Packet::Status)
        }

        async fn rename(
            &mut self,
            id: u32,
            old: String,
            new: String,
        ) -> Result<Status, Self::Error> {
            self.publish(id, old, new, false).await
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
            {
                let mut faults = self.faults.lock().unwrap();
                faults.removed.push(filename.clone());
                if faults.fail_remove {
                    return Err(StatusCode::Failure.with_message("injected cleanup failure"));
                }
                faults.modes.remove(&filename);
            }
            self.files
                .lock()
                .await
                .remove(&filename)
                .ok_or(StatusCode::NoSuchFile)?;
            Ok(memory_ok(id))
        }

        async fn mkdir(
            &mut self,
            id: u32,
            _path: String,
            _attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            Ok(memory_ok(id))
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let files = self.files.lock().await;
            let data = files.get(&path).ok_or(StatusCode::NoSuchFile)?;
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(data.len() as u64);
            let faults = self.faults.lock().unwrap();
            if faults.final_mode_fault == Some("verify")
                && path.contains(".iris-sftp-")
                && faults.path_mode_changes.iter().any(|mode| *mode != 0o600)
            {
                return Err(StatusCode::Failure.into());
            }
            attrs.permissions = faults.modes.get(&path).copied().unwrap_or(Some(0o100644));
            Ok(Attrs { id, attrs })
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            {
                let mut faults = self.faults.lock().unwrap();
                faults.lstatted.push(path.clone());
                if faults.fail_lstat {
                    return Err(StatusCode::PermissionDenied.into());
                }
            }
            self.stat(id, path).await
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            if self.faults.lock().unwrap().fail_fstat {
                return Err(StatusCode::OpUnsupported.into());
            }
            let mut reply = self.stat(id, handle).await?;
            if self.faults.lock().unwrap().omit_fstat_mode {
                reply.attrs.permissions = None;
            }
            Ok(reply)
        }

        async fn fsetstat(
            &mut self,
            id: u32,
            handle: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let mut faults = self.faults.lock().unwrap();
            faults.mode_changes += 1;
            if faults.fail_fsetstat {
                return Err(StatusCode::PermissionDenied.into());
            }
            assert!(attrs.uid.is_none() && attrs.gid.is_none());
            let mode = attrs.permissions.expect("permissions-only FSETSTAT");
            assert_eq!(mode & !0o777, 0, "no privilege or type bits copied");
            if !faults.ignore_fsetstat {
                faults.modes.insert(handle, Some(0o100000 | mode));
            }
            Ok(memory_ok(id))
        }

        async fn setstat(
            &mut self,
            id: u32,
            path: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            if !self.files.lock().await.contains_key(&path) {
                return Err(StatusCode::NoSuchFile.into());
            }
            let mut faults = self.faults.lock().unwrap();
            assert!(attrs.uid.is_none() && attrs.gid.is_none());
            let mode = attrs.permissions.expect("permissions-only SETSTAT");
            assert_eq!(mode & !0o777, 0, "no privilege or type bits copied");
            assert!(faults.closes > 0, "path chmod must follow CLOSE");
            faults.path_mode_changes.push(mode);
            let fault = if mode == 0o600 {
                None // best-effort re-private must still work after restoration fails
            } else {
                faults.final_mode_fault
            };
            if fault == Some("denied") {
                return Err(StatusCode::PermissionDenied.into());
            }
            if fault != Some("ignored") {
                let applied = if fault == Some("wrong") { 0o666 } else { mode };
                faults.modes.insert(path, Some(0o100000 | applied));
            }
            if fault == Some("lost-reply") {
                return Err(StatusCode::Failure.into());
            }
            Ok(memory_ok(id))
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            self.faults.lock().unwrap().listed.remove(&path);
            Ok(Handle { id, handle: path })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
            if !self.faults.lock().unwrap().listed.insert(handle.clone()) {
                return Err(StatusCode::Eof.into());
            }
            let prefix = format!("{handle}/");
            let mut children = BTreeMap::new();
            for (path, data) in self.files.lock().await.iter() {
                if let Some(relative) = path.strip_prefix(&prefix) {
                    let mut attrs = FileAttributes::empty();
                    let name = if let Some((dir, _)) = relative.split_once('/') {
                        attrs.set_dir(true);
                        dir
                    } else {
                        attrs.set_regular(true);
                        attrs.size = Some(data.len() as u64);
                        relative
                    };
                    children.insert(name.to_string(), attrs);
                }
            }
            Ok(Name {
                id,
                files: children
                    .into_iter()
                    .map(|(name, attrs)| File::new(name, attrs))
                    .collect(),
            })
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
            attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            self.faults.lock().unwrap().opened.push(filename.clone());
            let mut files = self.files.lock().await;
            if flags.contains(OpenFlags::EXCLUDE) {
                if self.faults.lock().unwrap().occupy_staging {
                    files.insert(filename.clone(), b"not owned by Iris".to_vec());
                }
                if files.contains_key(&filename) {
                    return Err(StatusCode::Failure.with_message("exclusive create collision"));
                }
            }
            if flags.contains(OpenFlags::TRUNCATE) {
                files.insert(filename.clone(), Vec::new());
            } else if !files.contains_key(&filename) && !flags.contains(OpenFlags::CREATE) {
                return Err(StatusCode::NoSuchFile.into());
            } else {
                files.entry(filename.clone()).or_default();
            }
            if flags.contains(OpenFlags::CREATE) {
                let mut faults = self.faults.lock().unwrap();
                assert!(attrs.uid.is_none() && attrs.gid.is_none());
                let mode = if faults.ignore_open_mode {
                    0o644
                } else {
                    attrs.permissions.unwrap_or(0o644) & !faults.create_umask
                };
                faults.modes.insert(filename.clone(), Some(0o100000 | mode));
            }
            Ok(Handle {
                id,
                handle: filename,
            })
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            let mut faults = self.faults.lock().unwrap();
            let mode = faults.modes.get(&handle).copied().flatten();
            faults.closed_modes.push(mode);
            faults.closes += 1;
            if faults.fail_close_at == Some(faults.closes) {
                faults.fail_close_at = None;
                return Err(
                    StatusCode::Failure.with_message("connection reset (injected after close)")
                );
            }
            Ok(memory_ok(id))
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            self.check_io(offset).await?;
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
            self.check_io(offset).await?;
            let mut files = self.files.lock().await;
            let file = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
            {
                let mut faults = self.faults.lock().unwrap();
                let mode = faults.modes.get(&handle).copied().flatten();
                faults.written_modes.push(mode);
                if faults.broaden_mode_on_write {
                    faults.modes.insert(handle.clone(), Some(0o100666));
                }
            }
            let start = offset as usize;
            let end = start.saturating_add(data.len());
            if file.len() < end {
                file.resize(end, 0);
            }
            file[start..end].copy_from_slice(&data);
            Ok(memory_ok(id))
        }
    }

    async fn memory_sftp_session() -> (Arc<SftpTransferSession>, MemorySftpServer) {
        memory_sftp_with_server(MemorySftpServer::default()).await
    }

    async fn memory_sftp_with_server(
        handler: MemorySftpServer,
    ) -> (Arc<SftpTransferSession>, MemorySftpServer) {
        let (client, server) = tokio::io::duplex(4 * 1024 * 1024);
        russh_sftp::server::run(server, handler.clone()).await;
        let session = RawSftpSession::new(client);
        let session = SftpTransferSession::init(session)
            .await
            .expect("initialize memory SFTP");
        (session, handler)
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
                    // The second publication committed but its reply is lost.
                    faults.fail_publish_reply_for = Some("/second".into());
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
                let transfer = SftpTransferSession::init(RawSftpSession::new(client))
                    .await
                    .expect("replacement transfer session");
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
            let faults = server.faults.lock().unwrap();
            assert_eq!(faults.published, ["/first", "/second"]);
            assert_eq!(faults.opened.len(), 2);
            assert!(faults
                .opened
                .iter()
                .all(|path| path.starts_with("/.iris-sftp-")));
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
    async fn staged_transfers_preserve_existing_files_on_transfer_and_close_failure() {
        for upload in [true, false] {
            for close_failure in [false, true] {
                let (session, server) = memory_sftp_session().await;
                let temp = tempfile::tempdir().unwrap();
                let local = temp.path().join("destination");
                let payload = vec![42; SFTP_PIPELINE_CHUNK_BYTES + 1];
                let (local_data, remote_data) = if upload {
                    (payload.as_slice(), b"existing".as_slice())
                } else {
                    (b"existing".as_slice(), payload.as_slice())
                };
                tokio::fs::write(&local, local_data).await.unwrap();
                server
                    .files
                    .lock()
                    .await
                    .insert("/destination".into(), remote_data.to_vec());
                {
                    let mut faults = server.faults.lock().unwrap();
                    faults.fail_io = !close_failure;
                    faults.fail_close_at = close_failure.then_some(1);
                }
                let result = if upload {
                    pipelined_upload(Arc::clone(&session), &local, "/destination", 1).await
                } else {
                    pipelined_download(
                        Arc::clone(&session),
                        "/destination",
                        &local,
                        payload.len() as u64,
                        1,
                    )
                    .await
                };
                let error = result.expect_err("injected transfer or close failure");
                assert!(
                    error.contains(if close_failure {
                        "failed to close"
                    } else {
                        "injected transfer failure"
                    }),
                    "{error}"
                );
                let files = server.files.lock().await;
                assert_eq!(files.len(), 1, "remote staging cleaned up");
                assert_eq!(files["/destination"], remote_data);
                assert_eq!(tokio::fs::read(&local).await.unwrap(), local_data);
                assert_eq!(
                    std::fs::read_dir(temp.path()).unwrap().count(),
                    1,
                    "local staging cleaned up"
                );
                let faults = server.faults.lock().unwrap();
                assert!(faults.published.is_empty());
                if upload {
                    assert_eq!(faults.removed, faults.opened);
                }
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn downloads_preserve_destination_modes_and_keep_partial_content_private() {
        use std::os::unix::fs::PermissionsExt;

        for existing_mode in [None, Some(0o640), Some(0o751)] {
            let (session, server) = memory_sftp_session().await;
            let temp = tempfile::tempdir().unwrap();
            let local = temp.path().join("destination");
            // Compare against ordinary creation without changing the process-wide
            // umask, which would race other tests. Run under umask 027 to also
            // distinguish the new-file result from tempfile's default 0600.
            let reference = temp.path().join("ordinary");
            std::fs::File::create(&reference).unwrap();
            let expected = existing_mode.unwrap_or_else(|| {
                std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777
            });
            if let Some(mode) = existing_mode {
                std::fs::write(&local, b"existing").unwrap();
                std::fs::set_permissions(&local, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            server
                .files
                .lock()
                .await
                .insert("/source".into(), b"replacement".to_vec());
            server.faults.lock().unwrap().pause_io = true;
            let transfer = pipelined_download(Arc::clone(&session), "/source", &local, 11, 1);
            tokio::pin!(transfer);
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    result = &mut transfer => panic!("transfer unexpectedly finished: {result:?}"),
                    _ = server.io_started.notified() => {}
                }
            })
            .await
            .expect("reach in-flight read");
            let staging = std::fs::read_dir(temp.path())
                .unwrap()
                .map(Result::unwrap)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".iris-sftp-")
                })
                .expect("private staging file");
            assert_eq!(staging.metadata().unwrap().permissions().mode() & 0o077, 0);
            if existing_mode.is_some() {
                assert_eq!(std::fs::read(&local).unwrap(), b"existing");
                assert_eq!(
                    std::fs::metadata(&local).unwrap().permissions().mode() & 0o777,
                    expected
                );
            } else {
                assert!(!local.exists(), "partial file is not published");
            }
            server.faults.lock().unwrap().pause_io = false;
            server.io_resume.notify_one();
            transfer.await.expect("publish download");
            assert_eq!(std::fs::read(&local).unwrap(), b"replacement");
            assert_eq!(
                std::fs::metadata(&local).unwrap().permissions().mode() & 0o777,
                expected
            );
            assert_eq!(
                std::fs::read_dir(temp.path()).unwrap().count(),
                2,
                "staging and mode probe removed"
            );
        }
    }

    #[tokio::test]
    async fn uploads_preserve_rwx_without_privilege_bits_and_keep_new_file_defaults() {
        // OPEN may ignore attributes or narrow them with the server's umask.
        for (existing, ignore_open_mode, umask) in [
            (Some(0o644), false, 0),
            (Some(0o644), true, 0),
            (Some(0o600), false, 0),
            (Some(0o600), true, 0),
            (Some(0o7751), false, 0o077),
            (Some(0), false, 0),
            (None, false, 0),
            (None, false, 0o027),
        ] {
            let server = MemorySftpServer::default();
            {
                let mut faults = server.faults.lock().unwrap();
                faults.ignore_open_mode = ignore_open_mode;
                faults.create_umask = umask;
                if let Some(mode) = existing {
                    faults
                        .modes
                        .insert("/destination".into(), Some(0o100000 | mode));
                }
            }
            if existing.is_some() {
                server
                    .files
                    .lock()
                    .await
                    .insert("/destination".into(), b"keep".to_vec());
            }
            let (session, server) = memory_sftp_with_server(server).await;
            let temp = tempfile::tempdir().unwrap();
            let local = temp.path().join("source");
            let payload = vec![42; SFTP_PIPELINE_CHUNK_BYTES * 3 + 17];
            tokio::fs::write(&local, &payload).await.unwrap();
            pipelined_upload(session, &local, "/destination", 3)
                .await
                .unwrap();
            let files = server.files.lock().await;
            assert_eq!(files.len(), 1, "no staging files left");
            assert_eq!(files["/destination"], payload);
            let faults = server.faults.lock().unwrap();
            let expected = existing.map_or(0o644 & !umask, |mode| mode & 0o777);
            assert_eq!(faults.modes["/destination"], Some(0o100000 | expected));
            assert_eq!(
                faults.written_modes,
                vec![Some(0o100600); 4],
                "every payload write must observe private staging"
            );
            assert_eq!(faults.closed_modes, [Some(0o100600)]);
            assert_eq!(faults.lstatted[0], "/destination");
            assert_eq!(
                faults.mode_changes,
                usize::from(ignore_open_mode || existing.is_none())
            );
            assert_eq!(
                faults.path_mode_changes,
                if expected == 0o600 {
                    vec![]
                } else {
                    vec![expected]
                },
                "final permissions restored only after successful CLOSE"
            );
        }
    }

    #[tokio::test]
    async fn uploads_reject_unknown_symlink_and_non_regular_destinations_before_staging() {
        for (mode, denied) in [
            (None, false),
            (Some(0o600), false),    // mode without a type is not a regular file
            (Some(0o120777), false), // symlink, even if its referent might be regular
            (Some(0o040700), false), // directory
            (Some(0o010600), false), // FIFO
            (Some(0o100600), true),
        ] {
            let server = MemorySftpServer::default();
            server
                .files
                .lock()
                .await
                .insert("/destination".into(), b"keep".to_vec());
            {
                let mut faults = server.faults.lock().unwrap();
                faults.modes.insert("/destination".into(), mode);
                faults.fail_lstat = denied;
            }
            let (session, server) = memory_sftp_with_server(server).await;
            let temp = tempfile::tempdir().unwrap();
            let local = temp.path().join("source");
            tokio::fs::write(&local, b"replacement").await.unwrap();
            let error = pipelined_upload(session, &local, "/destination", 1)
                .await
                .unwrap_err();
            assert!(
                error.contains(if denied {
                    "failed to lstat"
                } else {
                    "LSTAT must report a regular file and its mode"
                }),
                "{error}"
            );
            let files = server.files.lock().await;
            assert_eq!(files.len(), 1);
            assert_eq!(files["/destination"], b"keep");
            let faults = server.faults.lock().unwrap();
            assert_eq!(faults.lstatted, ["/destination"]);
            assert_eq!(faults.modes["/destination"], mode);
            assert!(
                faults.opened.is_empty()
                    && faults.published.is_empty()
                    && faults.removed.is_empty()
            );
        }
    }

    #[tokio::test]
    async fn upload_mode_failures_retain_destination_and_clean_owned_staging() {
        for existing in [false, true] {
            for failure in [
                "fstat",
                "no-mode",
                "chmod-denied",
                "chmod-ignored",
                "mode-changed-during-write",
            ] {
                let server = MemorySftpServer::default();
                if existing {
                    server
                        .files
                        .lock()
                        .await
                        .insert("/destination".into(), b"keep".to_vec());
                }
                {
                    let mut faults = server.faults.lock().unwrap();
                    if existing {
                        faults.modes.insert("/destination".into(), Some(0o100644));
                    }
                    faults.fail_fstat = failure == "fstat";
                    faults.omit_fstat_mode = failure == "no-mode";
                    faults.ignore_open_mode = failure.starts_with("chmod-");
                    faults.fail_fsetstat = failure == "chmod-denied";
                    faults.ignore_fsetstat = failure == "chmod-ignored";
                    faults.broaden_mode_on_write = failure == "mode-changed-during-write";
                }
                let (session, server) = memory_sftp_with_server(server).await;
                let temp = tempfile::tempdir().unwrap();
                let local = temp.path().join("source");
                tokio::fs::write(&local, b"replacement").await.unwrap();
                let error = pipelined_upload(session, &local, "/destination", 1)
                    .await
                    .unwrap_err();
                assert!(error.contains("staging mode"), "{failure}: {error}");
                let files = server.files.lock().await;
                assert_eq!(
                    files.len(),
                    usize::from(existing),
                    "{failure}: owned staging removed"
                );
                if existing {
                    assert_eq!(files["/destination"], b"keep");
                }
                let faults = server.faults.lock().unwrap();
                if existing {
                    assert_eq!(faults.modes["/destination"], Some(0o100644));
                }
                assert_eq!(faults.closes, 1);
                assert_eq!(faults.removed, faults.opened);
                assert_eq!(faults.removed.len(), 1);
                assert!(faults.published.is_empty());
                assert_eq!(
                    faults.written_modes.len(),
                    usize::from(failure == "mode-changed-during-write")
                );
            }
        }
    }

    #[tokio::test]
    async fn upload_final_mode_and_close_failures_never_publish_and_reprivate_before_cleanup() {
        for existing in [false, true] {
            for failure in [
                "denied",
                "ignored",
                "wrong",
                "lost-reply",
                "lost-reply-cleanup",
                "verify",
                "close",
            ] {
                let server = MemorySftpServer::default();
                if existing {
                    server
                        .files
                        .lock()
                        .await
                        .insert("/destination".into(), b"keep".to_vec());
                    server
                        .faults
                        .lock()
                        .unwrap()
                        .modes
                        .insert("/destination".into(), Some(0o100644));
                }
                {
                    let mut faults = server.faults.lock().unwrap();
                    faults.final_mode_fault = Some(if failure == "lost-reply-cleanup" {
                        "lost-reply"
                    } else {
                        failure
                    });
                    faults.fail_remove = failure == "lost-reply-cleanup";
                    faults.fail_close_at = (failure == "close").then_some(1);
                }
                let (session, server) = memory_sftp_with_server(server).await;
                let temp = tempfile::tempdir().unwrap();
                let local = temp.path().join("source");
                tokio::fs::write(&local, b"replacement").await.unwrap();
                let error = pipelined_upload(session, &local, "/destination", 1)
                    .await
                    .unwrap_err();
                assert!(
                    error.contains(if failure == "close" {
                        "failed to close"
                    } else {
                        "staging mode"
                    }),
                    "{failure}: {error}"
                );
                let files = server.files.lock().await;
                assert_eq!(
                    files.len(),
                    usize::from(existing) + usize::from(failure == "lost-reply-cleanup"),
                    "{failure}: only owned staging can remain"
                );
                if existing {
                    assert_eq!(files["/destination"], b"keep");
                }
                let faults = server.faults.lock().unwrap();
                assert_eq!(faults.written_modes, [Some(0o100600)]);
                assert_eq!(faults.closed_modes, [Some(0o100600)]);
                assert!(faults.published.is_empty());
                assert_eq!(faults.removed, faults.opened);
                assert_eq!(faults.removed.len(), 1);
                if failure == "lost-reply-cleanup" {
                    assert!(error.contains("failed to clean SFTP staging file"));
                    assert_eq!(
                        faults.modes[&faults.opened[0]],
                        Some(0o100600),
                        "failed cleanup leaves re-privatized staging"
                    );
                }
                assert_eq!(
                    faults.path_mode_changes,
                    if failure == "close" {
                        vec![0o600]
                    } else {
                        vec![0o644, 0o600]
                    }
                );
                if existing {
                    assert_eq!(faults.modes["/destination"], Some(0o100644));
                }
            }
        }
    }

    #[tokio::test]
    async fn upload_publication_and_cleanup_fail_safely_without_deleting_unowned_files() {
        for mode in ["rename", "cleanup", "collision", "no-posix"] {
            let server = MemorySftpServer::default();
            {
                let mut faults = server.faults.lock().unwrap();
                faults.fail_publish = mode == "rename" || mode == "cleanup";
                faults.fail_remove = mode == "cleanup";
                faults.occupy_staging = mode == "collision";
                faults.no_posix_rename = mode == "no-posix";
            }
            let (session, server) = memory_sftp_with_server(server).await;
            let temp = tempfile::tempdir().unwrap();
            let local = temp.path().join("source");
            tokio::fs::write(&local, b"replacement").await.unwrap();
            server
                .files
                .lock()
                .await
                .insert("/existing".into(), b"keep".to_vec());
            let error = pipelined_upload(Arc::clone(&session), &local, "/existing", 1)
                .await
                .expect_err("publication must fail safely");
            {
                let files = server.files.lock().await;
                assert_eq!(files["/existing"], b"keep");
                let faults = server.faults.lock().unwrap();
                assert!(faults.published.is_empty());
                assert!(!faults.removed.iter().any(|path| path == "/existing"));
                if mode == "collision" {
                    assert!(
                        faults.removed.is_empty(),
                        "exclusive OPEN did not establish ownership"
                    );
                    assert_eq!(files[&faults.opened[0]], b"not owned by Iris");
                } else if mode == "cleanup" {
                    assert!(
                        error.contains("failed to clean SFTP staging file"),
                        "{error}"
                    );
                    assert_eq!(files.len(), 2, "unreachable staging cleanup is reported");
                    assert_eq!(faults.modes[&faults.opened[0]], Some(0o100600));
                } else {
                    assert_eq!(files.len(), 1);
                    assert!(
                        error.contains("publication outcome may be uncertain"),
                        "{error}"
                    );
                    assert!(
                        error.contains(if mode == "no-posix" {
                            "server did not advertise POSIX rename"
                        } else {
                            "server-advertised POSIX rename was attempted"
                        }),
                        "{error}"
                    );
                }
            }
            if mode == "no-posix" {
                pipelined_upload(Arc::clone(&session), &local, "/new", 1)
                    .await
                    .expect("standard rename can install a new file");
                assert_eq!(server.files.lock().await["/new"], b"replacement");
            }
        }
    }

    #[tokio::test]
    async fn cancellation_during_transfer_preserves_destination_and_cleans_staging() {
        for upload in [true, false] {
            let (session, server) = memory_sftp_session().await;
            let temp = tempfile::tempdir().unwrap();
            let local = temp.path().join("destination");
            tokio::fs::write(&local, b"local data").await.unwrap();
            server
                .files
                .lock()
                .await
                .insert("/destination".into(), b"remote data".to_vec());
            server.faults.lock().unwrap().pause_io = true;
            {
                let transfer = async {
                    if upload {
                        pipelined_upload(Arc::clone(&session), &local, "/destination", 1).await
                    } else {
                        pipelined_download(Arc::clone(&session), "/destination", &local, 11, 1)
                            .await
                    }
                };
                tokio::pin!(transfer);
                tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::select! {
                        result = &mut transfer => panic!("transfer unexpectedly finished: {result:?}"),
                        _ = server.io_started.notified() => {}
                    }
                }).await.expect("reach in-flight request");
                // Dropping this future cancels it, rather than allowing a detached
                // worker to finish the payload and replace the destination.
            }
            server.faults.lock().unwrap().pause_io = false;
            server.io_resume.notify_one();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let clean = if upload {
                        server.files.lock().await.len() == 1
                    } else {
                        std::fs::read_dir(temp.path()).unwrap().count() == 1
                    };
                    if clean {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("best-effort cancellation cleanup");
            assert_eq!(server.files.lock().await["/destination"], b"remote data");
            assert_eq!(tokio::fs::read(&local).await.unwrap(), b"local data");
            assert!(server.faults.lock().unwrap().published.is_empty());
        }
    }

    #[tokio::test]
    async fn recursive_transfers_stage_siblings_and_publish_individual_files() {
        let (session, server) = memory_sftp_session().await;
        let control = memory_control_session(&server).await;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        tokio::fs::create_dir_all(source.join("nested/deeper"))
            .await
            .unwrap();
        tokio::fs::write(source.join("nested/deeper/payload"), b"new data")
            .await
            .unwrap();
        tokio::fs::write(source.join("empty"), []).await.unwrap();
        server
            .files
            .lock()
            .await
            .insert("/tree/nested/deeper/payload".into(), b"old remote".to_vec());
        upload_directory_tree(
            &control,
            Arc::clone(&session),
            1,
            &source,
            "/tree",
            &mut RemoteDirCreateCache::default(),
        )
        .await
        .unwrap();
        {
            let files = server.files.lock().await;
            assert_eq!(files.len(), 2);
            assert_eq!(files["/tree/nested/deeper/payload"], b"new data");
            assert!(files["/tree/empty"].is_empty());
            let faults = server.faults.lock().unwrap();
            assert!(faults
                .opened
                .iter()
                .any(|path| path.starts_with("/tree/nested/deeper/.iris-sftp-")));
        }
        tokio::fs::create_dir_all(destination.join("nested/deeper"))
            .await
            .unwrap();
        tokio::fs::write(destination.join("nested/deeper/payload"), b"old local")
            .await
            .unwrap();
        download_directory_tree(&control, Arc::clone(&session), 1, "/tree", &destination)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(destination.join("nested/deeper/payload"))
                .await
                .unwrap(),
            b"new data"
        );
        assert!(tokio::fs::read(destination.join("empty"))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read_dir(destination.join("nested/deeper"))
                .unwrap()
                .count(),
            1
        );
        control.close().await.unwrap();
        session.close_session().unwrap();
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

        server
            .files
            .lock()
            .await
            .insert("/roundtrip.bin".into(), b"old remote".to_vec());
        tokio::fs::write(&destination, b"old local").await.unwrap();
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
