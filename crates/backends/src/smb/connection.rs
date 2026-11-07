use iris_core_domain::types::error::{AuthErrorDetail, BackendError, NetworkErrorDetail};
use iris_smb_client::{
    authenticate_guest, authenticate_ntlm_v2, tree_connect_share, NtlmV2SecurityConfig,
    NtlmV2SessionSetup, Smb2Command, Smb2Dialect, Smb2EncryptionCipher, Smb2NegotiateContext,
    Smb2NegotiateRequest, Smb2NegotiateResponse, Smb2SecurityMode, SmbConnectionActor, SmbTree,
};
use ring::rand::{SecureRandom, SystemRandom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub const DEFAULT_SMB_PORT: u16 = 445;

const NTLM_FILETIME_UNIX_EPOCH_OFFSET_SECONDS: u64 = 11_644_473_600;
const NTLM_FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const SMB2_GLOBAL_CAP_ENCRYPTION: u32 = 0x0000_0040;
const SMB_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct SmbConnectionConfig {
    pub server: String,
    pub port: u16,
    pub share_name: String,
    pub username: Option<String>,
    pub domain: Option<String>,
    /// Used for native NTLMv2 authentication.
    pub password: Option<String>,
    /// NTLM workstation name.
    pub workstation: Option<String>,
    /// Allow an unauthenticated guest session. This must be explicitly enabled.
    pub allow_guest: bool,
}

#[derive(Clone)]
pub struct SmbNativeSession {
    pub actor: SmbConnectionActor,
    pub tree: SmbTree,
    pub negotiated: Smb2NegotiateResponse,
    pub generation: u64,
}

impl std::fmt::Debug for SmbNativeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmbNativeSession")
            .field("tree", &self.tree)
            .field("negotiated_dialect", &self.negotiated.dialect)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct SmbSession {
    pub native: SmbNativeSession,
}

#[derive(Debug)]
pub struct SmbPool {
    config: SmbConnectionConfig,
    session: Mutex<Option<SmbSession>>,
    next_generation: AtomicU64,
    invalid_through_generation: AtomicU64,
}

impl SmbPool {
    pub fn new(config: SmbConnectionConfig) -> Self {
        Self {
            config,
            session: Mutex::new(None),
            next_generation: AtomicU64::new(1),
            invalid_through_generation: AtomicU64::new(0),
        }
    }

    pub async fn session(&self) -> Result<SmbSession, BackendError> {
        let mut cached = self.session.lock().await;
        let invalid_through = self.invalid_through_generation.load(Ordering::Acquire);
        if cached
            .as_ref()
            .is_some_and(|session| session.native.generation <= invalid_through)
        {
            *cached = None;
        }
        if let Some(session) = cached.as_ref() {
            if session.native.actor.state().await.is_ok() {
                return Ok(session.clone());
            }
            *cached = None;
        }

        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let session = connect_smb(&self.config, generation).await?;
        *cached = Some(session.clone());
        Ok(session)
    }

    pub async fn invalidate(&self, generation: u64) {
        self.mark_invalid(generation);
        let mut cached = self.session.lock().await;
        if cached
            .as_ref()
            .is_some_and(|session| session.native.generation == generation)
        {
            *cached = None;
        }
    }

    pub fn mark_invalid(&self, generation: u64) {
        self.invalid_through_generation
            .fetch_max(generation, Ordering::Release);
    }
}

/// Connects with the native client; never falls back to an OS mount.
pub async fn connect_smb(
    config: &SmbConnectionConfig,
    generation: u64,
) -> Result<SmbSession, BackendError> {
    connect_native_smb(config, generation).await
}

async fn connect_native_smb(
    config: &SmbConnectionConfig,
    generation: u64,
) -> Result<SmbSession, BackendError> {
    let server = config.server.trim();
    if server.is_empty() {
        return Err(network_error("SMB native server is not configured"));
    }
    let share = config.share_name.trim();
    if share.is_empty() {
        return Err(network_error("SMB native share is not configured"));
    }

    let stream = tokio::time::timeout(
        SMB_CONNECT_TIMEOUT,
        TcpStream::connect((server, config.port)),
    )
    .await
    .map_err(|_| network_error("SMB native TCP connect timed out"))?
    .map_err(|error| network_error(format!("SMB native TCP connect failed: {error}")))?;
    stream
        .set_nodelay(true)
        .map_err(|error| network_error(format!("SMB native TCP_NODELAY failed: {error}")))?;

    let actor = SmbConnectionActor::spawn(stream);
    let negotiated = negotiate_native_smb(&actor).await?;

    if negotiated.security_mode.0
        & (Smb2SecurityMode::SIGNING_ENABLED.0 | Smb2SecurityMode::SIGNING_REQUIRED.0)
        == 0
    {
        return Err(auth_error("SMB server did not advertise signing support"));
    }

    if has_native_credentials(config) {
        let cipher = match negotiated.dialect {
            Smb2Dialect::Smb202
            | Smb2Dialect::Smb210
            | Smb2Dialect::Smb300
            | Smb2Dialect::Smb302 => Smb2EncryptionCipher::Aes128Ccm,
            Smb2Dialect::Smb311 => match negotiated.encryption_cipher {
                Some(
                    cipher @ (Smb2EncryptionCipher::Aes128Gcm | Smb2EncryptionCipher::Aes256Gcm),
                ) => cipher,
                _ => {
                    return Err(network_error(
                        "SMB server did not negotiate a supported GCM encryption cipher",
                    ))
                }
            },
        };
        authenticate_native_smb(&actor, config, negotiated.dialect, cipher).await?;
    } else if !config.allow_guest {
        return Err(auth_error(
            "SMB guest access is disabled; provide credentials or explicitly allow guest access",
        ));
    } else {
        authenticate_guest(&actor)
            .await
            .map_err(|error| auth_error(format!("SMB guest session setup failed: {error}")))?;
    }

    let tree = tree_connect_share(&actor, server, share)
        .await
        .map_err(|error| network_error(format!("SMB native tree connect failed: {error}")))?;

    Ok(SmbSession {
        native: SmbNativeSession {
            actor,
            tree,
            negotiated,
            generation,
        },
    })
}

async fn negotiate_native_smb(
    actor: &SmbConnectionActor,
) -> Result<Smb2NegotiateResponse, BackendError> {
    let response = actor
        .request(
            Smb2Command::Negotiate,
            Smb2NegotiateRequest {
                security_mode: Smb2SecurityMode(
                    Smb2SecurityMode::SIGNING_ENABLED.0 | Smb2SecurityMode::SIGNING_REQUIRED.0,
                ),
                capabilities: SMB2_GLOBAL_CAP_ENCRYPTION,
                client_guid: secure_random_array()?,
                dialects: vec![
                    Smb2Dialect::Smb311,
                    Smb2Dialect::Smb302,
                    Smb2Dialect::Smb300,
                    Smb2Dialect::Smb210,
                ],
                contexts: vec![
                    Smb2NegotiateContext::PreauthIntegritySha512 {
                        salt: secure_random_array::<32>()?.to_vec(),
                    },
                    Smb2NegotiateContext::EncryptionCapabilities {
                        ciphers: vec![
                            Smb2EncryptionCipher::Aes256Gcm,
                            Smb2EncryptionCipher::Aes128Gcm,
                            Smb2EncryptionCipher::Aes128Ccm,
                        ],
                    },
                ],
            }
            .encode(),
        )
        .await
        .map_err(|error| network_error(format!("SMB native negotiate failed: {error}")))?;
    Smb2NegotiateResponse::decode_packet(&response.to_packet())
        .map_err(|error| network_error(format!("SMB native negotiate decode failed: {error}")))
}

async fn authenticate_native_smb(
    actor: &SmbConnectionActor,
    config: &SmbConnectionConfig,
    dialect: Smb2Dialect,
    cipher: Smb2EncryptionCipher,
) -> Result<(), BackendError> {
    let password = config
        .password
        .as_deref()
        .ok_or_else(|| auth_error("SMB native password is not configured"))?;
    authenticate_ntlm_v2(
        actor,
        &NtlmV2SessionSetup {
            user: config.username.clone().unwrap_or_default(),
            domain: config.domain.clone().unwrap_or_default(),
            password: password.to_string(),
            workstation: config
                .workstation
                .clone()
                .unwrap_or_else(|| "IRIS".to_string()),
            timestamp: ntlm_filetime_now()?,
            client_challenge: secure_random_array()?,
            target_name: format!("cifs/{}", config.server.trim()),
            security: Some(NtlmV2SecurityConfig {
                dialect,
                encrypt: matches!(
                    dialect,
                    Smb2Dialect::Smb300 | Smb2Dialect::Smb302 | Smb2Dialect::Smb311
                ),
                cipher,
            }),
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        auth_error(format!(
            "SMB native NTLMv2 authentication failed for negotiated dialect {dialect:?}: {error}"
        ))
    })
}

fn has_native_credentials(config: &SmbConnectionConfig) -> bool {
    config
        .username
        .as_deref()
        .is_some_and(|username| !username.is_empty())
        || config.password.is_some()
}

fn secure_random_array<const N: usize>() -> Result<[u8; N], BackendError> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| network_error("SMB native secure random generation failed"))?;
    Ok(bytes)
}

fn ntlm_filetime_now() -> Result<u64, BackendError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| network_error(format!("system clock is before UNIX epoch: {error}")))?;
    let seconds = duration
        .as_secs()
        .checked_add(NTLM_FILETIME_UNIX_EPOCH_OFFSET_SECONDS)
        .ok_or_else(|| network_error("NTLM timestamp seconds overflowed"))?;
    let ticks_from_seconds = seconds
        .checked_mul(NTLM_FILETIME_TICKS_PER_SECOND)
        .ok_or_else(|| network_error("NTLM timestamp ticks overflowed"))?;
    Ok(ticks_from_seconds + u64::from(duration.subsec_nanos() / 100))
}

fn network_error(message: impl Into<String>) -> BackendError {
    BackendError::Network(NetworkErrorDetail {
        message: message.into(),
        is_timeout: false,
    })
}

fn auth_error(message: impl Into<String>) -> BackendError {
    BackendError::Auth(AuthErrorDetail {
        message: message.into(),
        is_token_expired: false,
    })
}
