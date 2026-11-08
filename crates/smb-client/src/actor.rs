use crate::connection::{SmbConnectionError, SmbResponse};
use crate::crypto::{
    derive_smb2_signing_key, smb311_initial_preauth_hash, smb311_update_preauth_hash,
    SmbSigningKey, SMB311_PREAUTH_HASH_BYTES,
};
use crate::packet::header::{
    Smb2Command, Smb2Header, SMB2_FLAGS_RELATED_OPERATIONS, SMB2_FLAGS_SIGNED, SMB2_HEADER_BYTES,
};
use crate::transform::{is_smb3_transform, Smb3EncryptionContext, Smb3EncryptionKeys};
use crate::transport::DirectTcpTransport;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep_until, Duration, Instant};

const DEFAULT_ACTOR_QUEUE_DEPTH: usize = 256;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TARGET_CREDITS: u16 = 256;

type ResponseResult = Result<SmbResponse, SmbConnectionError>;
type CompoundResponseResult = Result<Vec<SmbResponse>, SmbConnectionError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbCompoundRequest {
    pub command: Smb2Command,
    pub body: Vec<u8>,
    pub credit_charge: u16,
    pub related: bool,
}

#[derive(Clone)]
pub struct SmbConnectionActor {
    sender: mpsc::Sender<ActorCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmbConnectionActorState {
    pub next_message_id: u64,
    pub session_id: u64,
    pub tree_id: u32,
    pub credit_request: u16,
    pub available_credits: u16,
    pub preauth_integrity_hash: [u8; SMB311_PREAUTH_HASH_BYTES],
    pub encryption_enabled: bool,
}

enum ActorCommand {
    Request {
        command: Smb2Command,
        body: Vec<u8>,
        credit_charge: u16,
        header_tree_id: Option<u32>,
        response_state_update: ResponseStateUpdate,
        response_tx: oneshot::Sender<ResponseResult>,
        signing_after_send: Option<PendingSigningKey>,
    },
    Compound {
        requests: Vec<SmbCompoundRequest>,
        response_tx: oneshot::Sender<CompoundResponseResult>,
    },
    #[cfg(test)]
    SetIds {
        session_id: u64,
        tree_id: u32,
        ack_tx: oneshot::Sender<Result<(), SmbConnectionError>>,
    },
    SetSigningKey {
        signing_key: SmbSigningKey,
        ack_tx: oneshot::Sender<Result<(), SmbConnectionError>>,
    },
    EnableEncryption {
        encryption: Box<Smb3EncryptionContext>,
        ack_tx: oneshot::Sender<Result<(), SmbConnectionError>>,
    },
    Snapshot {
        response_tx: oneshot::Sender<SmbConnectionActorState>,
    },
}

enum PendingRequest {
    Single {
        command: Smb2Command,
        response_state_update: ResponseStateUpdate,
        response_tx: oneshot::Sender<ResponseResult>,
        deadline: Instant,
    },
    Compound {
        command: Smb2Command,
        index: usize,
        shared: Arc<Mutex<PendingCompound>>,
        deadline: Instant,
    },
}

struct PendingCompound {
    responses: Vec<Option<SmbResponse>>,
    response_tx: Option<oneshot::Sender<CompoundResponseResult>>,
}

struct OutboundRequest {
    command: Smb2Command,
    body: Vec<u8>,
    credit_charge: u16,
    header_tree_id: Option<u32>,
    response_state_update: ResponseStateUpdate,
    response_tx: oneshot::Sender<ResponseResult>,
    signing_after_send: Option<PendingSigningKey>,
}

struct PendingSigningKey {
    session_key: Vec<u8>,
    dialect: crate::packet::negotiate::Smb2Dialect,
}

impl PendingRequest {
    fn deadline(&self) -> Instant {
        match self {
            Self::Single { deadline, .. } | Self::Compound { deadline, .. } => *deadline,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseStateUpdate {
    None,
    SetSessionIdFromResponse,
    SetTreeIdFromResponse,
}

struct ActorState {
    next_message_id: u64,
    session_id: u64,
    tree_id: u32,
    credit_request: u16,
    available_credits: u16,
    signing_key: Option<SmbSigningKey>,
    encryption: Option<Smb3EncryptionContext>,
    preauth_integrity_hash: [u8; SMB311_PREAUTH_HASH_BYTES],
    request_timeout: Duration,
}

impl Default for ActorState {
    fn default() -> Self {
        Self {
            next_message_id: 0,
            session_id: 0,
            tree_id: 0,
            credit_request: TARGET_CREDITS,
            // SMB clients start with exactly one credit. The initial NEGOTIATE
            // asks the server for the remainder of the target window.
            available_credits: 1,
            signing_key: None,
            encryption: None,
            preauth_integrity_hash: smb311_initial_preauth_hash(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl SmbConnectionActor {
    pub fn spawn<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        Self::spawn_with_transport(DirectTcpTransport::new(stream))
    }

    #[cfg(test)]
    fn spawn_with_timeout<T>(stream: T, request_timeout: Duration) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (sender, receiver) = mpsc::channel(DEFAULT_ACTOR_QUEUE_DEPTH);
        tokio::spawn(async move {
            run_actor_with_timeout(DirectTcpTransport::new(stream), receiver, request_timeout)
                .await;
        });
        Self { sender }
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_initial_credits<T>(stream: T, initial_credits: u16) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (sender, receiver) = mpsc::channel(DEFAULT_ACTOR_QUEUE_DEPTH);
        tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(stream);
            let mut receiver = receiver;
            let mut state = ActorState {
                available_credits: initial_credits.max(1),
                ..ActorState::default()
            };
            run_actor_loop(&mut transport, &mut receiver, &mut state).await;
        });
        Self { sender }
    }

    pub fn spawn_with_transport<T>(transport: DirectTcpTransport<T>) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (sender, receiver) = mpsc::channel(DEFAULT_ACTOR_QUEUE_DEPTH);
        tokio::spawn(async move {
            run_actor(transport, receiver).await;
        });
        Self { sender }
    }

    pub async fn request(
        &self,
        command: Smb2Command,
        body: impl Into<Vec<u8>>,
    ) -> Result<SmbResponse, SmbConnectionError> {
        self.request_with_credit_charge(command, body, 0).await
    }

    pub async fn request_with_credit_charge(
        &self,
        command: Smb2Command,
        body: impl Into<Vec<u8>>,
        credit_charge: u16,
    ) -> Result<SmbResponse, SmbConnectionError> {
        self.call(|response_tx| ActorCommand::Request {
            command,
            body: body.into(),
            credit_charge,
            header_tree_id: None,
            response_state_update: ResponseStateUpdate::None,
            response_tx,
            signing_after_send: None,
        })
        .await
    }

    /// Sends one command to the actor task and waits for its reply.
    async fn call<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, SmbConnectionError>>) -> ActorCommand,
    ) -> Result<T, SmbConnectionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(command(reply_tx))
            .await
            .map_err(|_| SmbConnectionError::RequestQueueClosed)?;
        reply_rx
            .await
            .map_err(|_| SmbConnectionError::ResponseCanceled)?
    }

    pub async fn request_compound(
        &self,
        requests: Vec<SmbCompoundRequest>,
    ) -> Result<Vec<SmbResponse>, SmbConnectionError> {
        self.call(|response_tx| ActorCommand::Compound {
            requests,
            response_tx,
        })
        .await
    }

    pub(crate) async fn request_tree_connect(
        &self,
        body: impl Into<Vec<u8>>,
    ) -> Result<SmbResponse, SmbConnectionError> {
        self.call(|response_tx| ActorCommand::Request {
            command: Smb2Command::TreeConnect,
            body: body.into(),
            credit_charge: 0,
            header_tree_id: Some(0),
            response_state_update: ResponseStateUpdate::SetTreeIdFromResponse,
            response_tx,
            signing_after_send: None,
        })
        .await
    }

    /// Sends a SESSION_SETUP request. With `signing`, the signing key derived from that
    /// session key and dialect takes effect right after this request is sent.
    pub(crate) async fn request_session_setup(
        &self,
        body: impl Into<Vec<u8>>,
        signing: Option<(Vec<u8>, crate::packet::negotiate::Smb2Dialect)>,
    ) -> Result<SmbResponse, SmbConnectionError> {
        self.call(|response_tx| ActorCommand::Request {
            command: Smb2Command::SessionSetup,
            body: body.into(),
            credit_charge: 0,
            header_tree_id: None,
            response_state_update: ResponseStateUpdate::SetSessionIdFromResponse,
            response_tx,
            signing_after_send: signing.map(|(session_key, dialect)| PendingSigningKey {
                session_key,
                dialect,
            }),
        })
        .await
    }

    /// Sets the session and tree ids that real connections learn from responses.
    #[cfg(test)]
    pub(crate) async fn set_ids(
        &self,
        session_id: u64,
        tree_id: u32,
    ) -> Result<(), SmbConnectionError> {
        self.call(|ack_tx| ActorCommand::SetIds {
            session_id,
            tree_id,
            ack_tx,
        })
        .await
    }

    pub async fn set_signing_key(
        &self,
        signing_key: SmbSigningKey,
    ) -> Result<(), SmbConnectionError> {
        self.call(|ack_tx| ActorCommand::SetSigningKey {
            signing_key,
            ack_tx,
        })
        .await
    }

    pub async fn enable_encryption(
        &self,
        keys: Smb3EncryptionKeys,
    ) -> Result<(), SmbConnectionError> {
        let encryption = Smb3EncryptionContext::new(keys)?;
        self.call(|ack_tx| ActorCommand::EnableEncryption {
            encryption: Box::new(encryption),
            ack_tx,
        })
        .await
    }

    pub async fn state(&self) -> Result<SmbConnectionActorState, SmbConnectionError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(ActorCommand::Snapshot { response_tx })
            .await
            .map_err(|_| SmbConnectionError::RequestQueueClosed)?;
        response_rx
            .await
            .map_err(|_| SmbConnectionError::ResponseCanceled)
    }
}

async fn run_actor<T>(
    mut transport: DirectTcpTransport<T>,
    mut receiver: mpsc::Receiver<ActorCommand>,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut state = ActorState::default();
    run_actor_loop(&mut transport, &mut receiver, &mut state).await;
}

#[cfg(test)]
async fn run_actor_with_timeout<T>(
    mut transport: DirectTcpTransport<T>,
    mut receiver: mpsc::Receiver<ActorCommand>,
    request_timeout: Duration,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut state = ActorState {
        request_timeout,
        ..ActorState::default()
    };
    run_actor_loop(&mut transport, &mut receiver, &mut state).await;
}

async fn run_actor_loop<T>(
    transport: &mut DirectTcpTransport<T>,
    receiver: &mut mpsc::Receiver<ActorCommand>,
    state: &mut ActorState,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending = HashMap::<u64, PendingRequest>::new();
    let mut deferred = VecDeque::<ActorCommand>::new();
    let mut accepting_commands = true;

    loop {
        if pending.is_empty() && !accepting_commands {
            return;
        }

        if pending.is_empty() {
            match receiver.recv().await {
                Some(command) => {
                    deferred.push_back(command);
                    if let Err(error) =
                        flush_deferred(&mut deferred, transport, state, &mut pending).await
                    {
                        close_pending(&mut pending, error.to_string());
                        return;
                    }
                }
                None => return,
            }
            continue;
        }

        tokio::select! {
            maybe_command = receiver.recv(), if accepting_commands => {
                match maybe_command {
                    Some(command) => {
                        deferred.push_back(command);
                        if let Err(error) = flush_deferred(&mut deferred, transport, state, &mut pending).await {
                            close_pending(&mut pending, error.to_string());
                            return;
                        }
                    }
                    None => {
                        accepting_commands = false;
                    }
                }
            }
            response = transport.recv_packet(), if !pending.is_empty() => {
                match response {
                    Ok(packet) => {
                        if let Err(error) = dispatch_response(packet, state, &mut pending) {
                            close_pending(&mut pending, error.to_string());
                            return;
                        }
                        if let Err(error) = flush_deferred(&mut deferred, transport, state, &mut pending).await {
                            close_pending(&mut pending, error.to_string());
                            return;
                        }
                    }
                    Err(error) => {
                        close_pending(&mut pending, error.to_string());
                        return;
                    }
                }
            }
            _ = sleep_until(next_pending_deadline(&pending)), if !pending.is_empty() => {
                expire_pending(&mut pending, Instant::now());
            }
        }
    }
}

async fn flush_deferred<T>(
    deferred: &mut VecDeque<ActorCommand>,
    transport: &mut DirectTcpTransport<T>,
    state: &mut ActorState,
    pending: &mut HashMap<u64, PendingRequest>,
) -> Result<(), SmbConnectionError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let Some(command) = deferred.front() else {
            return Ok(());
        };
        let required = command_credit_charge(command);
        if required > state.available_credits {
            return Ok(());
        }
        let Some(command) = deferred.pop_front() else {
            return Ok(());
        };
        handle_command(command, transport, state, pending).await?;
    }
}

fn command_credit_charge(command: &ActorCommand) -> u16 {
    match command {
        ActorCommand::Request { credit_charge, .. } => (*credit_charge).max(1),
        ActorCommand::Compound { requests, .. } => requests
            .iter()
            .map(|request| request.credit_charge.max(1))
            .fold(0u16, u16::saturating_add)
            .max(1),
        _ => 0,
    }
}

async fn handle_command<T>(
    command: ActorCommand,
    transport: &mut DirectTcpTransport<T>,
    state: &mut ActorState,
    pending: &mut HashMap<u64, PendingRequest>,
) -> Result<(), SmbConnectionError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    match command {
        ActorCommand::Request {
            command,
            body,
            credit_charge,
            header_tree_id,
            response_state_update,
            response_tx,
            signing_after_send,
        } => {
            send_request(
                transport,
                state,
                pending,
                OutboundRequest {
                    command,
                    body,
                    credit_charge,
                    header_tree_id,
                    response_state_update,
                    response_tx,
                    signing_after_send,
                },
            )
            .await
        }
        ActorCommand::Compound {
            requests,
            response_tx,
        } => send_compound_request(transport, state, pending, requests, response_tx).await,
        #[cfg(test)]
        ActorCommand::SetIds {
            session_id,
            tree_id,
            ack_tx,
        } => {
            state.session_id = session_id;
            state.tree_id = tree_id;
            send_ack(ack_tx);
            Ok(())
        }
        ActorCommand::SetSigningKey {
            signing_key,
            ack_tx,
        } => {
            state.signing_key = Some(signing_key);
            send_ack(ack_tx);
            Ok(())
        }
        ActorCommand::EnableEncryption { encryption, ack_tx } => {
            state.encryption = Some(*encryption);
            send_ack(ack_tx);
            Ok(())
        }
        ActorCommand::Snapshot { response_tx } => {
            let _ = response_tx.send(state.snapshot());
            Ok(())
        }
    }
}

async fn send_request<T>(
    transport: &mut DirectTcpTransport<T>,
    state: &mut ActorState,
    pending: &mut HashMap<u64, PendingRequest>,
    request: OutboundRequest,
) -> Result<(), SmbConnectionError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let message_id = state.reserve_message_id(request.credit_charge);
    let charged = request.credit_charge.max(1);
    state.available_credits = state.available_credits.saturating_sub(charged);
    // The pending key comes from the final authentication token; the server cannot
    // install it until it processes this SESSION_SETUP. Use it only after send, so
    // the final response can be verified with the derived session key.
    let packet = match encode_request_packet(
        request.command,
        message_id,
        state,
        &request.body,
        request.credit_charge,
        request.header_tree_id,
        None,
    ) {
        Ok(packet) => packet,
        Err(error) => {
            state.available_credits = state.available_credits.saturating_add(charged);
            let _ = request.response_tx.send(Err(error));
            return Ok(());
        }
    };
    let wire_packet = if let Some(encryption) = &mut state.encryption {
        encryption.encrypt(state.session_id, &packet)?
    } else {
        packet
    };
    match transport.send_packet(&wire_packet).await {
        Ok(()) => {
            state.update_preauth_hash(request.command, &wire_packet);
            if let Some(pending_key) = request.signing_after_send {
                state.signing_key = Some(derive_smb2_signing_key(
                    &pending_key.session_key,
                    pending_key.dialect,
                    &state.preauth_integrity_hash,
                )?);
            }
            pending.insert(
                message_id,
                PendingRequest::Single {
                    command: request.command,
                    response_state_update: request.response_state_update,
                    response_tx: request.response_tx,
                    deadline: Instant::now() + state.request_timeout,
                },
            );
            Ok(())
        }
        Err(error) => {
            state.available_credits = state.available_credits.saturating_add(charged);
            let _ = request
                .response_tx
                .send(Err(SmbConnectionError::Transport(error)));
            Ok(())
        }
    }
}

async fn send_compound_request<T>(
    transport: &mut DirectTcpTransport<T>,
    state: &mut ActorState,
    pending: &mut HashMap<u64, PendingRequest>,
    requests: Vec<SmbCompoundRequest>,
    response_tx: oneshot::Sender<CompoundResponseResult>,
) -> Result<(), SmbConnectionError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    if requests.is_empty() {
        let _ = response_tx.send(Err(crate::SmbClientError::EmptyCompoundRequest.into()));
        return Ok(());
    }
    let related_style = requests.get(1).is_some_and(|request| request.related);
    if requests[0].related
        || requests
            .iter()
            .skip(1)
            .any(|request| request.related != related_style)
    {
        let _ = response_tx.send(Err(crate::SmbClientError::MixedCompoundRequestStyles.into()));
        return Ok(());
    }

    let total_charge = requests
        .iter()
        .map(|request| request.credit_charge.max(1))
        .fold(0u16, u16::saturating_add);
    state.available_credits = state.available_credits.saturating_sub(total_charge);
    let message_ids = requests
        .iter()
        .map(|request| state.reserve_message_id(request.credit_charge))
        .collect::<Vec<_>>();
    let packet = match encode_compound_request_packet(&requests, &message_ids, state) {
        Ok(packet) => packet,
        Err(error) => {
            state.available_credits = state.available_credits.saturating_add(total_charge);
            let _ = response_tx.send(Err(error));
            return Ok(());
        }
    };
    let wire_packet = if let Some(encryption) = &mut state.encryption {
        encryption.encrypt(state.session_id, &packet)?
    } else {
        packet
    };
    match transport.send_packet(&wire_packet).await {
        Ok(()) => {
            let shared = Arc::new(Mutex::new(PendingCompound {
                responses: vec![None; requests.len()],
                response_tx: Some(response_tx),
            }));
            let deadline = Instant::now() + state.request_timeout;
            for (index, (request, message_id)) in requests.iter().zip(message_ids).enumerate() {
                pending.insert(
                    message_id,
                    PendingRequest::Compound {
                        command: request.command,
                        index,
                        shared: Arc::clone(&shared),
                        deadline,
                    },
                );
            }
            Ok(())
        }
        Err(error) => {
            state.available_credits = state.available_credits.saturating_add(total_charge);
            let _ = response_tx.send(Err(SmbConnectionError::Transport(error)));
            Ok(())
        }
    }
}

fn encode_compound_request_packet(
    requests: &[SmbCompoundRequest],
    message_ids: &[u64],
    state: &ActorState,
) -> Result<Vec<u8>, SmbConnectionError> {
    let mut packet = Vec::new();
    for (index, (request, message_id)) in requests.iter().zip(message_ids).enumerate() {
        let is_last = index + 1 == requests.len();
        let unpadded_len = SMB2_HEADER_BYTES + request.body.len();
        let segment_len = if is_last {
            unpadded_len
        } else {
            unpadded_len.checked_add(7).ok_or(
                crate::SmbClientError::InvalidCompoundNextCommand {
                    offset: unpadded_len,
                    remaining: usize::MAX,
                },
            )? & !7
        };
        let next_command = if is_last {
            0
        } else {
            u32::try_from(segment_len).map_err(|_| crate::SmbClientError::FieldTooLarge {
                field: "compound.next_command",
                length: segment_len,
                max: u32::MAX as usize,
            })?
        };
        let mut header = Smb2Header::new(request.command, *message_id);
        header.credit_charge = request.credit_charge;
        header.credit_request_response = credit_request_for_state(state);
        header.next_command = next_command;
        if request.related && index > 0 {
            header.flags |= SMB2_FLAGS_RELATED_OPERATIONS;
            header.session_id = u64::MAX;
            header.tree_id = u32::MAX;
        } else {
            header.session_id = state.session_id;
            header.tree_id = state.tree_id;
        }
        if state.signing_key.is_some() && state.encryption.is_none() {
            header.flags |= SMB2_FLAGS_SIGNED;
        }

        let segment_start = packet.len();
        packet.extend_from_slice(&header.encode());
        packet.extend_from_slice(&request.body);
        packet.resize(segment_start + segment_len, 0);
        if state.encryption.is_none() {
            if let Some(signing_key) = &state.signing_key {
                signing_key.sign_packet(&mut packet[segment_start..segment_start + segment_len])?;
            }
        }
    }
    Ok(packet)
}

fn encode_request_packet(
    command: Smb2Command,
    message_id: u64,
    state: &ActorState,
    body: &[u8],
    credit_charge: u16,
    header_tree_id: Option<u32>,
    signing_key_override: Option<&SmbSigningKey>,
) -> Result<Vec<u8>, SmbConnectionError> {
    let mut header = Smb2Header::new(command, message_id);
    header.credit_charge = credit_charge;
    header.credit_request_response = credit_request_for_state(state);
    header.session_id = state.session_id;
    header.tree_id = header_tree_id.unwrap_or(state.tree_id);
    let signing_key = signing_key_override.or(state.signing_key.as_ref());
    if signing_key.is_some() && state.encryption.is_none() {
        header.flags |= SMB2_FLAGS_SIGNED;
    }

    let mut packet = Vec::with_capacity(SMB2_HEADER_BYTES + body.len());
    packet.extend_from_slice(&header.encode());
    packet.extend_from_slice(body);
    if state.encryption.is_none() {
        if let Some(signing_key) = signing_key {
            signing_key.sign_packet(&mut packet)?;
        }
    }
    Ok(packet)
}

fn dispatch_response(
    response_packet: Vec<u8>,
    state: &mut ActorState,
    pending: &mut HashMap<u64, PendingRequest>,
) -> Result<(), SmbConnectionError> {
    let response_packet = match &state.encryption {
        Some(encryption) if is_smb3_transform(&response_packet) => {
            encryption.decrypt(state.session_id, &response_packet)?
        }
        Some(_) => return Err(crate::SmbClientError::ExpectedEncryptedTransform.into()),
        None => response_packet,
    };
    let responses = decode_compound_responses(&response_packet)?;
    let mut granted_credits = 0u16;
    for response in responses {
        let message_id = response.header.message_id;
        let Some(pending_request) = pending.remove(&message_id) else {
            return Err(SmbConnectionError::UnexpectedMessageId { actual: message_id });
        };
        granted_credits = granted_credits.saturating_add(response.header.credit_request_response);

        match pending_request {
            PendingRequest::Single {
                command,
                response_state_update,
                response_tx,
                ..
            } => {
                let result = validate_response_segment(
                    &response.to_packet(),
                    &response.header,
                    command,
                    message_id,
                    state,
                )
                .map(|()| {
                    state.update_preauth_response(
                        command,
                        response.header.status(),
                        &response.to_packet(),
                    );
                    apply_response_state_update(state, &response.header, response_state_update);
                    response
                });
                let _ = response_tx.send(result);
            }
            PendingRequest::Compound {
                command,
                index,
                shared,
                ..
            } => {
                validate_response_segment(
                    &response.to_packet(),
                    &response.header,
                    command,
                    message_id,
                    state,
                )?;
                let completed = {
                    let mut compound = shared.lock().unwrap_or_else(|error| error.into_inner());
                    compound.responses[index] = Some(response);
                    if compound.responses.iter().all(Option::is_some) {
                        let responses = compound
                            .responses
                            .iter_mut()
                            .map(|response| response.take().expect("all responses present"))
                            .collect::<Vec<_>>();
                        compound
                            .response_tx
                            .take()
                            .map(|response_tx| (response_tx, responses))
                    } else {
                        None
                    }
                };
                if let Some((response_tx, responses)) = completed {
                    let _ = response_tx.send(Ok(responses));
                }
            }
        }
    }
    state.available_credits = state.available_credits.saturating_add(granted_credits);
    Ok(())
}

fn credit_request_for_state(state: &ActorState) -> u16 {
    state
        .credit_request
        .saturating_sub(state.available_credits)
        .max(1)
}

pub fn decode_compound_responses(packet: &[u8]) -> Result<Vec<SmbResponse>, SmbConnectionError> {
    let mut responses = Vec::new();
    let mut offset = 0usize;
    loop {
        let remaining = packet.len().saturating_sub(offset);
        if remaining < SMB2_HEADER_BYTES {
            return Err(crate::SmbClientError::ShortFrame {
                expected: offset + SMB2_HEADER_BYTES,
                actual: packet.len(),
            }
            .into());
        }
        let header = Smb2Header::decode(&packet[offset..])?;
        let next_command = header.next_command as usize;
        let segment_end = if next_command == 0 {
            packet.len()
        } else {
            if next_command < SMB2_HEADER_BYTES
                || !next_command.is_multiple_of(8)
                || next_command > remaining
            {
                return Err(crate::SmbClientError::InvalidCompoundNextCommand {
                    offset: next_command,
                    remaining,
                }
                .into());
            }
            offset + next_command
        };
        responses.push(SmbResponse {
            header,
            body: packet[offset + SMB2_HEADER_BYTES..segment_end].to_vec(),
        });
        if next_command == 0 {
            break;
        }
        offset = segment_end;
    }
    Ok(responses)
}

fn apply_response_state_update(
    state: &mut ActorState,
    response_header: &Smb2Header,
    update: ResponseStateUpdate,
) {
    match update {
        ResponseStateUpdate::None => {}
        ResponseStateUpdate::SetSessionIdFromResponse => {
            if response_header.session_id != 0 {
                state.session_id = response_header.session_id;
            }
        }
        ResponseStateUpdate::SetTreeIdFromResponse => {
            state.tree_id = response_header.tree_id;
        }
    }
}

fn validate_response_segment(
    response_packet: &[u8],
    response_header: &Smb2Header,
    expected_command: Smb2Command,
    expected_message_id: u64,
    state: &ActorState,
) -> Result<(), SmbConnectionError> {
    if state.encryption.is_none() {
        let Some(signing_key) = &state.signing_key else {
            return validate_response_identity(
                response_header,
                expected_command,
                expected_message_id,
            );
        };
        let response_signed = response_header.flags & SMB2_FLAGS_SIGNED == SMB2_FLAGS_SIGNED;
        if !response_signed {
            return Err(SmbConnectionError::UnsignedResponse {
                command: expected_command,
                status: response_header.status().0,
            });
        }
        if !signing_key.verify_packet(response_packet)? {
            return Err(SmbConnectionError::SignatureVerificationFailed);
        }
    }
    validate_response_identity(response_header, expected_command, expected_message_id)
}

fn validate_response_identity(
    response_header: &Smb2Header,
    expected_command: Smb2Command,
    expected_message_id: u64,
) -> Result<(), SmbConnectionError> {
    if response_header.command != expected_command {
        return Err(SmbConnectionError::CommandMismatch {
            expected: expected_command,
            actual: response_header.command,
        });
    }
    if response_header.message_id != expected_message_id {
        return Err(SmbConnectionError::MessageIdMismatch {
            expected: expected_message_id,
            actual: response_header.message_id,
        });
    }
    Ok(())
}

fn close_pending(pending: &mut HashMap<u64, PendingRequest>, reason: String) {
    for (_, request) in pending.drain() {
        let error = SmbConnectionError::ActorStopped {
            reason: reason.clone(),
        };
        match request {
            PendingRequest::Single { response_tx, .. } => {
                let _ = response_tx.send(Err(error));
            }
            PendingRequest::Compound { shared, .. } => {
                let response_tx = shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .response_tx
                    .take();
                if let Some(response_tx) = response_tx {
                    let _ = response_tx.send(Err(error));
                }
            }
        }
    }
}

fn next_pending_deadline(pending: &HashMap<u64, PendingRequest>) -> Instant {
    pending
        .values()
        .map(PendingRequest::deadline)
        .min()
        .unwrap_or_else(Instant::now)
}

fn expire_pending(pending: &mut HashMap<u64, PendingRequest>, now: Instant) {
    let expired = pending
        .iter()
        .filter_map(|(message_id, request)| (request.deadline() <= now).then_some(*message_id))
        .collect::<Vec<_>>();
    for message_id in expired {
        let Some(request) = pending.remove(&message_id) else {
            continue;
        };
        let error = SmbConnectionError::RequestTimedOut {
            seconds: DEFAULT_REQUEST_TIMEOUT.as_secs(),
        };
        match request {
            PendingRequest::Single { response_tx, .. } => {
                let _ = response_tx.send(Err(error));
            }
            PendingRequest::Compound { shared, .. } => {
                let response_tx = shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .response_tx
                    .take();
                if let Some(response_tx) = response_tx {
                    let _ = response_tx.send(Err(error));
                }
                pending.retain(|_, request| match request {
                    PendingRequest::Compound { shared: other, .. } => !Arc::ptr_eq(other, &shared),
                    PendingRequest::Single { .. } => true,
                });
            }
        }
    }
}

fn send_ack(ack_tx: oneshot::Sender<Result<(), SmbConnectionError>>) {
    let _ = ack_tx.send(Ok(()));
}

impl ActorState {
    fn reserve_message_id(&mut self, credit_charge: u16) -> u64 {
        let message_id = self.next_message_id;
        self.next_message_id = self
            .next_message_id
            .saturating_add(u64::from(credit_charge.max(1)));
        message_id
    }

    fn snapshot(&self) -> SmbConnectionActorState {
        SmbConnectionActorState {
            next_message_id: self.next_message_id,
            session_id: self.session_id,
            tree_id: self.tree_id,
            credit_request: self.credit_request,
            available_credits: self.available_credits,
            preauth_integrity_hash: self.preauth_integrity_hash,
            encryption_enabled: self.encryption.is_some(),
        }
    }

    fn update_preauth_hash(&mut self, command: Smb2Command, packet: &[u8]) {
        if tracks_preauth(command) {
            self.preauth_integrity_hash =
                smb311_update_preauth_hash(&self.preauth_integrity_hash, packet);
        }
    }

    fn update_preauth_response(
        &mut self,
        command: Smb2Command,
        status: crate::packet::header::SmbNtStatus,
        packet: &[u8],
    ) {
        if tracks_preauth(command) && !(command == Smb2Command::SessionSetup && status.is_success())
        {
            self.preauth_integrity_hash =
                smb311_update_preauth_hash(&self.preauth_integrity_hash, packet);
        }
    }
}

fn tracks_preauth(command: Smb2Command) -> bool {
    matches!(command, Smb2Command::Negotiate | Smb2Command::SessionSetup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{
        sign_smb2_packet_hmac_sha256, smb311_preauth_hash_sequence, verify_smb2_packet_hmac_sha256,
    };
    use crate::test_support::*;
    use crate::transform::{Smb3EncryptionContext, Smb3EncryptionKeys};
    use crate::transport::DirectTcpTransport;
    use tokio::io::duplex;

    #[tokio::test]
    async fn actor_expires_requests_without_a_response() {
        let (client, mut server) = duplex(1024);
        let actor = SmbConnectionActor::spawn_with_timeout(client, Duration::from_millis(10));
        let server_task = tokio::spawn(async move {
            let mut packet = [0u8; 128];
            let _ = tokio::io::AsyncReadExt::read(&mut server, &mut packet).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let error = actor
            .request(Smb2Command::Echo, b"no-response".to_vec())
            .await
            .expect_err("request should time out");
        assert!(matches!(error, SmbConnectionError::RequestTimedOut { .. }));
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_dispatches_out_of_order_responses_by_message_id() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn_with_initial_credits(client, 2);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let first = transport.recv_packet().await.expect("first request");
            let second = transport.recv_packet().await.expect("second request");
            let first_header = Smb2Header::decode(&first).expect("first header");
            let second_header = Smb2Header::decode(&second).expect("second header");
            assert_eq!(first_header.command, Smb2Command::Echo);
            assert_eq!(second_header.command, Smb2Command::Echo);
            assert_eq!(first_header.message_id, 0);
            assert_eq!(second_header.message_id, 1);

            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Echo, second_header.message_id),
                    b"second",
                ))
                .await
                .expect("second response");
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Echo, first_header.message_id),
                    b"first",
                ))
                .await
                .expect("first response");
        });

        let first = actor.request(Smb2Command::Echo, b"one".to_vec());
        let second = actor.request(Smb2Command::Echo, b"two".to_vec());
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.expect("first").body, b"first");
        assert_eq!(second.expect("second").body, b"second");
        let state = actor.state().await.expect("state");
        assert_eq!(state.next_message_id, 2);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_round_trips_related_compound_operations() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn_with_initial_credits(client, 4);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("compound request");
            let first = Smb2Header::decode(&request).expect("create header");
            let second_offset = first.next_command as usize;
            let second = Smb2Header::decode(&request[second_offset..]).expect("read header");
            let third_offset = second_offset + second.next_command as usize;
            let third = Smb2Header::decode(&request[third_offset..]).expect("close header");

            assert_eq!(first.command, Smb2Command::Create);
            assert_eq!(second.command, Smb2Command::Read);
            assert_eq!(third.command, Smb2Command::Close);
            assert_eq!(
                (first.message_id, second.message_id, third.message_id),
                (0, 1, 3)
            );
            assert_eq!(first.next_command as usize % 8, 0);
            assert_eq!(second.next_command as usize % 8, 0);
            assert_eq!(third.next_command, 0);
            assert_eq!(first.session_id, 0x1111);
            assert_eq!(first.tree_id, 0x2222);
            assert_eq!(second.session_id, u64::MAX);
            assert_eq!(second.tree_id, u32::MAX);
            assert!(second.flags & SMB2_FLAGS_RELATED_OPERATIONS != 0);
            assert!(third.flags & SMB2_FLAGS_RELATED_OPERATIONS != 0);

            transport
                .send_packet(&compound_response_packet(vec![
                    (
                        Smb2Header::new(Smb2Command::Create, first.message_id),
                        b"created".to_vec(),
                    ),
                    (
                        Smb2Header::new(Smb2Command::Read, second.message_id),
                        b"payload".to_vec(),
                    ),
                    (
                        Smb2Header::new(Smb2Command::Close, third.message_id),
                        b"closed".to_vec(),
                    ),
                ]))
                .await
                .expect("compound response");
        });

        actor.set_ids(0x1111, 0x2222).await.expect("ids");
        let responses = actor
            .request_compound(vec![
                SmbCompoundRequest {
                    command: Smb2Command::Create,
                    body: b"create".to_vec(),
                    credit_charge: 0,
                    related: false,
                },
                SmbCompoundRequest {
                    command: Smb2Command::Read,
                    body: b"read".to_vec(),
                    credit_charge: 2,
                    related: true,
                },
                SmbCompoundRequest {
                    command: Smb2Command::Close,
                    body: b"close".to_vec(),
                    credit_charge: 0,
                    related: true,
                },
            ])
            .await
            .expect("compound responses");

        assert_eq!(responses.len(), 3);
        assert!(responses[0].body.starts_with(b"created"));
        assert!(responses[1].body.starts_with(b"payload"));
        assert_eq!(responses[2].body, b"closed");
        assert_eq!(actor.state().await.expect("state").next_message_id, 4);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_collects_split_responses_for_a_compound_request() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn_with_initial_credits(client, 2);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("compound request");
            let first = Smb2Header::decode(&request).expect("first header");
            let second =
                Smb2Header::decode(&request[first.next_command as usize..]).expect("second header");
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(first.command, first.message_id),
                    b"first",
                ))
                .await
                .expect("first response");
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(second.command, second.message_id),
                    b"second",
                ))
                .await
                .expect("second response");
        });

        let responses = actor
            .request_compound(vec![
                SmbCompoundRequest {
                    command: Smb2Command::Create,
                    body: vec![1],
                    credit_charge: 0,
                    related: false,
                },
                SmbCompoundRequest {
                    command: Smb2Command::Close,
                    body: vec![2],
                    credit_charge: 0,
                    related: true,
                },
            ])
            .await
            .expect("split compound responses");

        assert_eq!(responses[0].body, b"first");
        assert_eq!(responses[1].body, b"second");
        server_task.await.expect("server task");
    }

    #[test]
    fn compound_response_rejects_unaligned_next_command() {
        let mut header = Smb2Header::new(Smb2Command::Create, 0);
        header.next_command = 65;
        let error = decode_compound_responses(&header.encode()).expect_err("reject");
        assert!(matches!(
            error,
            SmbConnectionError::Protocol(crate::SmbClientError::InvalidCompoundNextCommand {
                offset: 65,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn actor_applies_state_updates_before_following_requests() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("request");
            let header = Smb2Header::decode(&request).expect("header");
            assert_eq!(header.session_id, 0x1010);
            assert_eq!(header.tree_id, 0x2020);
            let mut response_header = Smb2Header::new(Smb2Command::Echo, header.message_id);
            response_header.credit_request_response = 32;
            transport
                .send_packet(&response_packet(response_header, b"ok"))
                .await
                .expect("response");
        });

        actor.set_ids(0x1010, 0x2020).await.expect("ids");
        let response = actor
            .request(Smb2Command::Echo, b"probe".to_vec())
            .await
            .expect("response");

        assert_eq!(response.body, b"ok");
        let state = actor.state().await.expect("state");
        assert_eq!(state.available_credits, 32);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_encrypts_requests_and_requires_encrypted_responses() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let session_id = 0x0102_0304_0506_0708;
        let client_keys = Smb3EncryptionKeys::new([0x11; 16], [0x22; 16]);
        let server_keys = Smb3EncryptionKeys::new([0x22; 16], [0x11; 16]);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let mut encryption = Smb3EncryptionContext::new(server_keys).expect("encryption");
            let encrypted_request = transport.recv_packet().await.expect("request");
            assert!(is_smb3_transform(&encrypted_request));
            let request = encryption
                .decrypt(session_id, &encrypted_request)
                .expect("decrypt request");
            let request_header = Smb2Header::decode(&request).expect("request header");
            assert_eq!(request_header.command, Smb2Command::Echo);

            let response = response_packet(
                Smb2Header::new(Smb2Command::Echo, request_header.message_id),
                b"encrypted response",
            );
            let encrypted_response = encryption
                .encrypt(session_id, &response)
                .expect("encrypt response");
            transport
                .send_packet(&encrypted_response)
                .await
                .expect("response");
        });

        actor.set_ids(session_id, 0).await.expect("ids");
        actor
            .enable_encryption(client_keys)
            .await
            .expect("enable encryption");
        let response = actor
            .request(Smb2Command::Echo, b"secret".to_vec())
            .await
            .expect("echo");

        assert_eq!(response.body, b"encrypted response");
        assert!(actor.state().await.expect("state").encryption_enabled);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_tracks_preauth_hash_for_negotiate_and_session_setup() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let mut preauth_packets = Vec::new();

            let negotiate = transport.recv_packet().await.expect("negotiate request");
            let negotiate_header = Smb2Header::decode(&negotiate).expect("negotiate header");
            assert_eq!(negotiate_header.command, Smb2Command::Negotiate);
            preauth_packets.push(negotiate);
            let negotiate_response = response_packet(
                Smb2Header::new(Smb2Command::Negotiate, negotiate_header.message_id),
                b"negotiate-ok",
            );
            transport
                .send_packet(&negotiate_response)
                .await
                .expect("negotiate response");
            preauth_packets.push(negotiate_response);

            let session_setup = transport
                .recv_packet()
                .await
                .expect("session setup request");
            let session_setup_header =
                Smb2Header::decode(&session_setup).expect("session setup header");
            assert_eq!(session_setup_header.command, Smb2Command::SessionSetup);
            preauth_packets.push(session_setup);
            let mut session_setup_response_header =
                Smb2Header::new(Smb2Command::SessionSetup, session_setup_header.message_id);
            session_setup_response_header.session_id = 0x0102_0304_0506_0708;
            let session_setup_response =
                response_packet(session_setup_response_header, b"session-ok");
            transport
                .send_packet(&session_setup_response)
                .await
                .expect("session setup response");
            // The terminal successful SessionSetup response is excluded from the
            // session preauthentication hash used for key derivation.

            let echo = transport.recv_packet().await.expect("echo request");
            let echo_header = Smb2Header::decode(&echo).expect("echo header");
            assert_eq!(echo_header.command, Smb2Command::Echo);
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Echo, echo_header.message_id),
                    b"echo-ok",
                ))
                .await
                .expect("echo response");

            smb311_preauth_hash_sequence(preauth_packets.iter().map(Vec::as_slice))
        });

        actor
            .request(Smb2Command::Negotiate, b"negotiate".to_vec())
            .await
            .expect("negotiate");
        actor
            .request_session_setup(b"session".to_vec(), None)
            .await
            .expect("session setup");
        let before_echo = actor
            .state()
            .await
            .expect("state before echo")
            .preauth_integrity_hash;
        actor
            .request(Smb2Command::Echo, b"probe".to_vec())
            .await
            .expect("echo");
        let after_echo = actor
            .state()
            .await
            .expect("state after echo")
            .preauth_integrity_hash;
        let expected = server_task.await.expect("server task");

        assert_eq!(before_echo, expected);
        assert_eq!(after_echo, expected);
    }

    async fn echo_with_hmac_signing(
        respond: impl FnOnce(Smb2Header, &[u8]) -> Vec<u8> + Send + 'static,
    ) -> Result<SmbResponse, SmbConnectionError> {
        let key = b"session-signing-key".to_vec();
        let server_key = key.clone();
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("request");
            let header = Smb2Header::decode(&request).expect("header");
            assert_eq!(header.flags & SMB2_FLAGS_SIGNED, SMB2_FLAGS_SIGNED);
            assert!(verify_smb2_packet_hmac_sha256(&server_key, &request).expect("verify"));
            transport
                .send_packet(&respond(header, &server_key))
                .await
                .expect("response");
        });
        actor
            .set_signing_key(SmbSigningKey::hmac_sha256(key))
            .await
            .expect("signing key");
        let result = actor.request(Smb2Command::Echo, b"ping".to_vec()).await;
        server_task.await.expect("server task");
        result
    }

    fn signed_echo_response(request: Smb2Header, key: &[u8]) -> Vec<u8> {
        let mut header = Smb2Header::new(request.command, request.message_id);
        header.flags |= SMB2_FLAGS_SIGNED;
        let mut response = response_packet(header, b"pong");
        sign_smb2_packet_hmac_sha256(key, &mut response).expect("sign");
        response
    }

    #[tokio::test]
    async fn actor_signs_requests_and_verifies_signed_responses() {
        let response = echo_with_hmac_signing(signed_echo_response)
            .await
            .expect("signed response");
        assert_eq!(response.body, b"pong");
    }

    #[tokio::test]
    async fn actor_rejects_bad_response_signature() {
        let error = echo_with_hmac_signing(|request, key| {
            let mut response = signed_echo_response(request, key);
            *response.last_mut().expect("byte") ^= 0xff;
            response
        })
        .await
        .expect_err("bad signature");
        assert!(matches!(
            error,
            SmbConnectionError::SignatureVerificationFailed
        ));
    }

    #[tokio::test]
    async fn actor_rejects_unsigned_response_once_signing_is_enabled() {
        let error = echo_with_hmac_signing(|request, _| {
            let mut header = Smb2Header::new(request.command, request.message_id);
            header.status_or_channel_sequence =
                crate::packet::header::SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
            response_packet(header, b"pong")
        })
        .await
        .expect_err("unsigned response");
        assert!(matches!(
            error,
            SmbConnectionError::UnsignedResponse {
                command: Smb2Command::Echo,
                status: 0xc000_0016,
            }
        ));
    }

    #[tokio::test]
    async fn actor_rejects_response_for_another_command() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("request");
            let header = Smb2Header::decode(&request).expect("header");
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Cancel, header.message_id),
                    b"",
                ))
                .await
                .expect("response");
        });
        let error = actor
            .request(Smb2Command::Echo, b"ping".to_vec())
            .await
            .expect_err("command mismatch");
        assert!(matches!(
            error,
            SmbConnectionError::CommandMismatch {
                expected: Smb2Command::Echo,
                actual: Smb2Command::Cancel,
            }
        ));
        server_task.await.expect("server task");
    }
}
