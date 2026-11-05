use crate::crypto::{
    derive_smb2_signing_key, smb311_initial_preauth_hash, smb311_update_preauth_hash,
    SmbSigningKey, SMB311_PREAUTH_HASH_BYTES,
};
use crate::packet::close::{Smb2CloseRequest, Smb2CloseResponse};
use crate::packet::create::{Smb2CreateRequest, Smb2CreateResponse};
use crate::packet::header::{
    smb2_credit_charge_for_payload, Smb2Command, Smb2Header, SmbNtStatus, SMB2_FLAGS_SIGNED,
    SMB2_HEADER_BYTES,
};
use crate::packet::ioctl::{Smb2IoctlRequest, Smb2IoctlResponse};
use crate::packet::negotiate::{Smb2NegotiateRequest, Smb2NegotiateResponse};
use crate::packet::query_directory::{Smb2QueryDirectoryRequest, Smb2QueryDirectoryResponse};
use crate::packet::read::{Smb2ReadRequest, Smb2ReadResponse};
use crate::packet::session_setup::{Smb2SessionSetupRequest, Smb2SessionSetupResponse};
use crate::packet::set_info::{Smb2SetInfoRequest, Smb2SetInfoResponse};
use crate::packet::tree_connect::{Smb2TreeConnectRequest, Smb2TreeConnectResponse};
use crate::packet::write::{Smb2WriteRequest, Smb2WriteResponse};
use crate::transform::{is_smb3_transform, Smb3EncryptionContext, Smb3EncryptionKeys};
use crate::transport::{DirectTcpTransport, SmbTransportError};
use crate::SmbClientError;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, thiserror::Error)]
pub enum SmbConnectionError {
    #[error(transparent)]
    Transport(#[from] SmbTransportError),

    #[error(transparent)]
    Protocol(#[from] SmbClientError),

    #[error("SMB response command mismatch: expected {expected:?}, got {actual:?}")]
    CommandMismatch {
        expected: Smb2Command,
        actual: Smb2Command,
    },

    #[error("SMB response message id mismatch: expected {expected}, got {actual}")]
    MessageIdMismatch { expected: u64, actual: u64 },

    #[error("SMB response message id was not pending: {actual}")]
    UnexpectedMessageId { actual: u64 },

    #[error("SMB response signature verification failed")]
    SignatureVerificationFailed,

    #[error("SMB response was not signed after signing became mandatory")]
    UnsignedResponse,

    #[error("SMB {command:?} returned unexpected status 0x{status:08x}")]
    UnexpectedStatus { command: Smb2Command, status: u32 },

    #[error("SMB write accepted {actual} bytes for expected {expected}")]
    ShortWrite { expected: u32, actual: u32 },

    #[error("SMB connection actor request queue closed")]
    RequestQueueClosed,

    #[error("SMB connection actor response channel closed")]
    ResponseCanceled,

    #[error("SMB request timed out after {seconds} seconds")]
    RequestTimedOut { seconds: u64 },

    #[error("SMB request needs {required} credits but only {available} are available")]
    InsufficientCredits { required: u16, available: u16 },

    #[error("SMB connection actor stopped: {reason}")]
    ActorStopped { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbResponse {
    pub header: Smb2Header,
    pub body: Vec<u8>,
}

impl SmbResponse {
    pub fn to_packet(&self) -> Vec<u8> {
        let mut packet = Vec::with_capacity(SMB2_HEADER_BYTES + self.body.len());
        packet.extend_from_slice(&self.header.encode());
        packet.extend_from_slice(&self.body);
        packet
    }

    pub fn status(&self) -> SmbNtStatus {
        self.header.status()
    }
}

pub struct SmbConnection<T> {
    transport: DirectTcpTransport<T>,
    next_message_id: u64,
    session_id: u64,
    tree_id: u32,
    credit_request: u16,
    available_credits: u16,
    signing_key: Option<SmbSigningKey>,
    encryption: Option<Smb3EncryptionContext>,
    preauth_integrity_hash: [u8; SMB311_PREAUTH_HASH_BYTES],
    signing_after_next_request: Option<(Vec<u8>, crate::packet::negotiate::Smb2Dialect)>,
}

impl<T> SmbConnection<T> {
    pub fn new(stream: T) -> Self {
        Self::with_transport(DirectTcpTransport::new(stream))
    }

    pub fn with_transport(transport: DirectTcpTransport<T>) -> Self {
        Self {
            transport,
            next_message_id: 0,
            session_id: 0,
            tree_id: 0,
            credit_request: 256,
            available_credits: 1,
            signing_key: None,
            encryption: None,
            preauth_integrity_hash: smb311_initial_preauth_hash(),
            signing_after_next_request: None,
        }
    }

    pub fn set_session_id(&mut self, session_id: u64) {
        self.session_id = session_id;
    }

    pub fn set_tree_id(&mut self, tree_id: u32) {
        self.tree_id = tree_id;
    }

    pub fn set_credit_request(&mut self, credit_request: u16) {
        self.credit_request = credit_request.max(1);
    }

    pub fn next_message_id(&self) -> u64 {
        self.next_message_id
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn tree_id(&self) -> u32 {
        self.tree_id
    }

    pub fn available_credits(&self) -> u16 {
        self.available_credits
    }

    pub fn preauth_integrity_hash(&self) -> [u8; SMB311_PREAUTH_HASH_BYTES] {
        self.preauth_integrity_hash
    }

    pub fn set_hmac_sha256_signing_key(&mut self, signing_key: impl Into<Vec<u8>>) {
        self.set_signing_key(SmbSigningKey::hmac_sha256(signing_key));
    }

    pub fn set_signing_key(&mut self, signing_key: SmbSigningKey) {
        self.signing_key = Some(signing_key);
    }

    pub(crate) fn install_signing_after_next_request(
        &mut self,
        session_key: Vec<u8>,
        dialect: crate::packet::negotiate::Smb2Dialect,
    ) {
        self.signing_after_next_request = Some((session_key, dialect));
    }

    pub fn enable_encryption(
        &mut self,
        keys: Smb3EncryptionKeys,
    ) -> Result<(), SmbConnectionError> {
        self.encryption = Some(Smb3EncryptionContext::new(keys)?);
        Ok(())
    }

    pub fn into_inner(self) -> T {
        self.transport.into_inner()
    }
}

impl<T> SmbConnection<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    pub async fn request(
        &mut self,
        command: Smb2Command,
        body: &[u8],
    ) -> Result<SmbResponse, SmbConnectionError> {
        self.request_with_credit_charge(command, body, 0).await
    }

    pub async fn request_with_credit_charge(
        &mut self,
        command: Smb2Command,
        body: &[u8],
        credit_charge: u16,
    ) -> Result<SmbResponse, SmbConnectionError> {
        let message_id = self.reserve_message_id(credit_charge);
        let charged = credit_charge.max(1);
        if charged > self.available_credits {
            return Err(SmbConnectionError::InsufficientCredits {
                required: charged,
                available: self.available_credits,
            });
        }
        self.available_credits -= charged;
        let request_signing_key = self
            .signing_after_next_request
            .as_ref()
            .map(|(session_key, dialect)| {
                derive_smb2_signing_key(session_key, *dialect, &self.preauth_integrity_hash)
            })
            .transpose()?;
        let signing_key = request_signing_key.as_ref().or(self.signing_key.as_ref());
        let mut header = Smb2Header::new(command, message_id);
        header.credit_charge = credit_charge;
        header.credit_request_response = self
            .credit_request
            .saturating_sub(self.available_credits)
            .max(1);
        header.session_id = self.session_id;
        header.tree_id = self.tree_id;
        if signing_key.is_some() && self.encryption.is_none() {
            header.flags |= SMB2_FLAGS_SIGNED;
        }

        let mut packet = Vec::with_capacity(SMB2_HEADER_BYTES + body.len());
        packet.extend_from_slice(&header.encode());
        packet.extend_from_slice(body);
        if self.encryption.is_none() {
            if let Some(signing_key) = signing_key {
                signing_key.sign_packet(&mut packet)?;
            }
        }
        let wire_packet = if let Some(encryption) = &mut self.encryption {
            encryption.encrypt(self.session_id, &packet)?
        } else {
            packet
        };
        self.transport.send_packet(&wire_packet).await?;
        self.update_preauth_hash(command, &wire_packet);
        if let Some((session_key, dialect)) = self.signing_after_next_request.take() {
            self.signing_key = Some(derive_smb2_signing_key(
                &session_key,
                dialect,
                &self.preauth_integrity_hash,
            )?);
        }

        let response_packet = self.transport.recv_packet().await?;
        let response_packet = match &self.encryption {
            Some(encryption) if is_smb3_transform(&response_packet) => {
                encryption.decrypt(self.session_id, &response_packet)?
            }
            Some(_) => return Err(SmbClientError::ExpectedEncryptedTransform.into()),
            None => response_packet,
        };
        let response_header = Smb2Header::decode(&response_packet)?;
        if self.encryption.is_none() {
            if let Some(signing_key) = &self.signing_key {
                let response_signed =
                    response_header.flags & SMB2_FLAGS_SIGNED == SMB2_FLAGS_SIGNED;
                if !response_signed {
                    return Err(SmbConnectionError::UnsignedResponse);
                }
                if !signing_key.verify_packet(&response_packet)? {
                    return Err(SmbConnectionError::SignatureVerificationFailed);
                }
            }
        }
        if response_header.command != command {
            return Err(SmbConnectionError::CommandMismatch {
                expected: command,
                actual: response_header.command,
            });
        }
        if response_header.message_id != message_id {
            return Err(SmbConnectionError::MessageIdMismatch {
                expected: message_id,
                actual: response_header.message_id,
            });
        }

        self.available_credits = self
            .available_credits
            .saturating_add(response_header.credit_request_response);
        self.update_preauth_response(command, response_header.status(), &response_packet);
        Ok(SmbResponse {
            header: response_header,
            body: response_packet[SMB2_HEADER_BYTES..].to_vec(),
        })
    }

    pub async fn negotiate(
        &mut self,
        request: &Smb2NegotiateRequest,
    ) -> Result<Smb2NegotiateResponse, SmbConnectionError> {
        let response = self
            .request(Smb2Command::Negotiate, &request.encode())
            .await?;
        Ok(Smb2NegotiateResponse::decode_packet(&response.to_packet())?)
    }

    pub async fn session_setup(
        &mut self,
        request: &Smb2SessionSetupRequest,
    ) -> Result<Smb2SessionSetupResponse, SmbConnectionError> {
        let response = self
            .request(Smb2Command::SessionSetup, &request.encode())
            .await?;
        if response.header.session_id != 0 {
            self.session_id = response.header.session_id;
        }
        Ok(Smb2SessionSetupResponse::decode_packet(
            &response.to_packet(),
        )?)
    }

    pub async fn tree_connect(
        &mut self,
        request: &Smb2TreeConnectRequest,
    ) -> Result<Smb2TreeConnectResponse, SmbConnectionError> {
        let previous_tree_id = self.tree_id;
        self.tree_id = 0;
        let response = self
            .request(Smb2Command::TreeConnect, &request.encode()?)
            .await;
        match response {
            Ok(response) => {
                self.tree_id = response.header.tree_id;
                Ok(Smb2TreeConnectResponse::decode_packet(
                    &response.to_packet(),
                )?)
            }
            Err(error) => {
                self.tree_id = previous_tree_id;
                Err(error)
            }
        }
    }

    pub async fn create(
        &mut self,
        request: &Smb2CreateRequest,
    ) -> Result<Smb2CreateResponse, SmbConnectionError> {
        let response = self
            .request(Smb2Command::Create, &request.encode()?)
            .await?;
        Ok(Smb2CreateResponse::decode_packet(&response.to_packet())?)
    }

    pub async fn read(
        &mut self,
        request: &Smb2ReadRequest,
    ) -> Result<Smb2ReadResponse, SmbConnectionError> {
        let credit_charge = smb2_credit_charge_for_payload(request.length as usize)?;
        let response = self
            .request_with_credit_charge(Smb2Command::Read, &request.encode()?, credit_charge)
            .await?;
        Ok(Smb2ReadResponse::decode_packet(&response.to_packet())?)
    }

    pub async fn write(
        &mut self,
        request: &Smb2WriteRequest,
    ) -> Result<Smb2WriteResponse, SmbConnectionError> {
        let response = self.request(Smb2Command::Write, &request.encode()?).await?;
        Ok(Smb2WriteResponse::decode_packet(&response.to_packet())?)
    }

    pub async fn ioctl(
        &mut self,
        request: &Smb2IoctlRequest,
    ) -> Result<Smb2IoctlResponse, SmbConnectionError> {
        let response = self.request(Smb2Command::Ioctl, &request.encode()?).await?;
        Ok(Smb2IoctlResponse::decode_packet(&response.to_packet())?)
    }

    pub async fn query_directory(
        &mut self,
        request: &Smb2QueryDirectoryRequest,
    ) -> Result<Smb2QueryDirectoryResponse, SmbConnectionError> {
        let response = self
            .request(Smb2Command::QueryDirectory, &request.encode()?)
            .await?;
        Ok(Smb2QueryDirectoryResponse::decode_packet(
            &response.to_packet(),
        )?)
    }

    pub async fn set_info(
        &mut self,
        request: &Smb2SetInfoRequest,
    ) -> Result<Smb2SetInfoResponse, SmbConnectionError> {
        let response = self
            .request(Smb2Command::SetInfo, &request.encode()?)
            .await?;
        Ok(Smb2SetInfoResponse::decode_packet(&response.to_packet())?)
    }

    pub async fn close(
        &mut self,
        request: &Smb2CloseRequest,
    ) -> Result<Smb2CloseResponse, SmbConnectionError> {
        let response = self.request(Smb2Command::Close, &request.encode()).await?;
        Ok(Smb2CloseResponse::decode_packet(&response.to_packet())?)
    }

    fn reserve_message_id(&mut self, credit_charge: u16) -> u64 {
        let message_id = self.next_message_id;
        self.next_message_id = self
            .next_message_id
            .saturating_add(u64::from(credit_charge.max(1)));
        message_id
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
        status: SmbNtStatus,
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
    use crate::packet::close::Smb2CloseFlags;
    use crate::packet::create::{
        Smb2CreateAction, Smb2CreateDisposition, Smb2CreateOptions, Smb2DesiredAccess,
        Smb2FileAttributes, Smb2FileId, Smb2ImpersonationLevel, Smb2OplockLevel, Smb2ShareAccess,
    };
    use crate::packet::negotiate::{Smb2Dialect, Smb2SecurityMode};
    use crate::packet::query_directory::{Smb2FileInformationClass, Smb2QueryDirectoryFlags};
    use crate::packet::read::{Smb2ReadChannel, Smb2ReadFlags};
    use crate::packet::session_setup::Smb2SessionSetupSecurityMode;
    use crate::packet::set_info::{Smb2FileInfoClass, Smb2InfoType};
    use crate::packet::tree_connect::{
        Smb2ShareCapabilities, Smb2ShareFlags, Smb2ShareType, Smb2TreeConnectFlags,
    };
    use crate::packet::write::{Smb2WriteChannel, Smb2WriteFlags};
    use tokio::io::duplex;

    #[tokio::test]
    async fn negotiate_decodes_response_packet() {
        let (client, server) = duplex(1024);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            assert_eq!(request_header.command, Smb2Command::Negotiate);
            assert_eq!(request_header.message_id, 0);
            let request_body =
                Smb2NegotiateRequest::decode(&request[SMB2_HEADER_BYTES..]).expect("request body");
            assert_eq!(
                request_body.dialects,
                vec![Smb2Dialect::Smb311, Smb2Dialect::Smb302]
            );

            let mut response_header =
                Smb2Header::new(Smb2Command::Negotiate, request_header.message_id);
            response_header.credit_request_response = 16;
            transport
                .send_packet(&response_packet(
                    response_header,
                    &negotiate_response_body(b"spnego-token"),
                    b"spnego-token",
                ))
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        let response = connection
            .negotiate(&Smb2NegotiateRequest {
                security_mode: Smb2SecurityMode::SIGNING_ENABLED,
                capabilities: 0x0000_0040,
                client_guid: [0x22; 16],
                dialects: vec![Smb2Dialect::Smb311, Smb2Dialect::Smb302],
                contexts: Vec::new(),
            })
            .await
            .expect("negotiate");

        assert_eq!(response.dialect, Smb2Dialect::Smb311);
        assert_eq!(response.security_buffer, b"spnego-token");
        assert_eq!(connection.available_credits(), 16);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn session_setup_updates_session_id() {
        let (client, server) = duplex(1024);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            assert_eq!(request_header.command, Smb2Command::SessionSetup);
            assert_eq!(request_header.session_id, 0);
            let request_body = Smb2SessionSetupRequest::decode(&request[SMB2_HEADER_BYTES..])
                .expect("request body");
            assert_eq!(request_body.security_buffer, b"ntlm-negotiate");

            let mut response_header =
                Smb2Header::new(Smb2Command::SessionSetup, request_header.message_id);
            response_header.session_id = 0x0102_0304_0506_0708;
            transport
                .send_packet(&response_packet(
                    response_header,
                    &session_setup_response_body(b"ntlm-challenge"),
                    b"ntlm-challenge",
                ))
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        let response = connection
            .session_setup(&Smb2SessionSetupRequest {
                flags: 0,
                security_mode: Smb2SessionSetupSecurityMode::SIGNING_ENABLED,
                capabilities: 0,
                channel: 0,
                previous_session_id: 0,
                security_buffer: b"ntlm-negotiate".to_vec(),
            })
            .await
            .expect("session setup");

        assert_eq!(response.security_buffer, b"ntlm-challenge");
        assert_eq!(connection.session_id(), 0x0102_0304_0506_0708);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_tracks_preauth_hash_for_negotiate_and_session_setup() {
        let (client, server) = duplex(4096);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let mut preauth_packets = Vec::new();

            let negotiate = transport.recv_packet().await.expect("negotiate request");
            let negotiate_header = Smb2Header::decode(&negotiate).expect("negotiate header");
            assert_eq!(negotiate_header.command, Smb2Command::Negotiate);
            preauth_packets.push(negotiate);
            let negotiate_response = response_packet(
                Smb2Header::new(Smb2Command::Negotiate, negotiate_header.message_id),
                &negotiate_response_body(b"spnego"),
                b"spnego",
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
            let session_setup_response = response_packet(
                session_setup_response_header,
                &session_setup_response_body(b"ntlm-challenge"),
                b"ntlm-challenge",
            );
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
                    &[],
                ))
                .await
                .expect("echo response");

            smb311_preauth_hash_sequence(preauth_packets.iter().map(Vec::as_slice))
        });

        let mut connection = SmbConnection::new(client);
        connection
            .negotiate(&Smb2NegotiateRequest {
                security_mode: Smb2SecurityMode::SIGNING_ENABLED,
                capabilities: 0x0000_0040,
                client_guid: [0x22; 16],
                dialects: vec![Smb2Dialect::Smb311, Smb2Dialect::Smb302],
                contexts: Vec::new(),
            })
            .await
            .expect("negotiate");
        connection
            .session_setup(&Smb2SessionSetupRequest {
                flags: 0,
                security_mode: Smb2SessionSetupSecurityMode::SIGNING_ENABLED,
                capabilities: 0,
                channel: 0,
                previous_session_id: 0,
                security_buffer: b"ntlm-negotiate".to_vec(),
            })
            .await
            .expect("session setup");
        let before_echo = connection.preauth_integrity_hash();
        connection
            .request(Smb2Command::Echo, b"probe")
            .await
            .expect("echo");
        let after_echo = connection.preauth_integrity_hash();
        let expected = server_task.await.expect("server task");

        assert_eq!(before_echo, expected);
        assert_eq!(after_echo, expected);
    }

    #[tokio::test]
    async fn tree_connect_sends_zero_tree_id_and_updates_tree_id() {
        let (client, server) = duplex(1024);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            assert_eq!(request_header.command, Smb2Command::TreeConnect);
            assert_eq!(request_header.session_id, 0x0102_0304_0506_0708);
            assert_eq!(request_header.tree_id, 0);
            let request_body =
                Smb2TreeConnectRequest::decode(&request[SMB2_HEADER_BYTES..]).expect("request");
            assert_eq!(request_body.path, r"\\server\share");

            let mut response_header =
                Smb2Header::new(Smb2Command::TreeConnect, request_header.message_id);
            response_header.session_id = request_header.session_id;
            response_header.tree_id = 0x1122_3344;
            transport
                .send_packet(&response_packet(
                    response_header,
                    &tree_connect_response_body(),
                    &[],
                ))
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        connection.set_session_id(0x0102_0304_0506_0708);
        connection.set_tree_id(0xffff_ffff);

        let response = connection
            .tree_connect(&Smb2TreeConnectRequest {
                flags: Smb2TreeConnectFlags::default(),
                path: r"\\server\share".to_string(),
            })
            .await
            .expect("tree connect");

        assert_eq!(response.share_type, Smb2ShareType::Disk);
        assert_eq!(connection.tree_id(), 0x1122_3344);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn file_operations_decode_create_read_and_close() {
        let (client, server) = duplex(4096);
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            for expected in [
                Smb2Command::Create,
                Smb2Command::QueryDirectory,
                Smb2Command::Read,
                Smb2Command::Write,
                Smb2Command::SetInfo,
                Smb2Command::Close,
            ] {
                let request = transport.recv_packet().await.expect("receive request");
                let request_header = Smb2Header::decode(&request).expect("decode request");
                assert_eq!(request_header.command, expected);
                assert_eq!(request_header.session_id, 0x0102_0304_0506_0708);
                assert_eq!(request_header.tree_id, 0x1122_3344);
                let body = &request[SMB2_HEADER_BYTES..];
                let response_body = match expected {
                    Smb2Command::Create => {
                        let create = Smb2CreateRequest::decode(body).expect("create request");
                        assert_eq!(create.name, "folder/file.txt");
                        create_response_body(file_id)
                    }
                    Smb2Command::Read => {
                        let read = Smb2ReadRequest::decode(body).expect("read request");
                        assert_eq!(request_header.credit_charge, 1);
                        assert_eq!(read.file_id, file_id);
                        assert_eq!(read.offset, 4096);
                        read_response_body(b"payload")
                    }
                    Smb2Command::Write => {
                        let write = Smb2WriteRequest::decode(body).expect("write request");
                        assert_eq!(write.file_id, file_id);
                        assert_eq!(write.offset, 8192);
                        assert_eq!(write.data, b"payload");
                        write_response_body(7)
                    }
                    Smb2Command::SetInfo => {
                        let set_info = Smb2SetInfoRequest::decode(body).expect("set info request");
                        assert_eq!(set_info.file_id, file_id);
                        assert_eq!(set_info.buffer, b"rename-info");
                        set_info_response_body()
                    }
                    Smb2Command::QueryDirectory => {
                        let query = Smb2QueryDirectoryRequest::decode(body).expect("query request");
                        assert_eq!(query.file_id, file_id);
                        assert_eq!(query.file_name, "*");
                        query_directory_response_body(b"dir-entry")
                    }
                    Smb2Command::Close => {
                        let close = Smb2CloseRequest::decode(body).expect("close request");
                        assert_eq!(close.file_id, file_id);
                        close_response_body()
                    }
                    _ => unreachable!("covered by expected command list"),
                };

                let mut response_header = Smb2Header::new(expected, request_header.message_id);
                response_header.session_id = request_header.session_id;
                response_header.tree_id = request_header.tree_id;
                transport
                    .send_packet(&response_packet(response_header, &response_body, &[]))
                    .await
                    .expect("send response");
            }
        });

        let mut connection = SmbConnection::new(client);
        connection.set_session_id(0x0102_0304_0506_0708);
        connection.set_tree_id(0x1122_3344);

        let create = connection
            .create(&Smb2CreateRequest {
                security_flags: 0,
                requested_oplock_level: Smb2OplockLevel::None,
                impersonation_level: Smb2ImpersonationLevel::Impersonation,
                smb_create_flags: 0,
                desired_access: Smb2DesiredAccess::GENERIC_READ,
                file_attributes: Smb2FileAttributes::NORMAL,
                share_access: Smb2ShareAccess::READ,
                create_disposition: Smb2CreateDisposition::Open,
                create_options: Smb2CreateOptions::NON_DIRECTORY_FILE,
                name: "folder/file.txt".to_string(),
                create_contexts: Vec::new(),
            })
            .await
            .expect("create");
        assert_eq!(create.file_id, file_id);
        assert_eq!(create.create_action, Smb2CreateAction::Opened);

        let query = connection
            .query_directory(&Smb2QueryDirectoryRequest {
                file_information_class: Smb2FileInformationClass::FILE_DIRECTORY_INFORMATION,
                flags: Smb2QueryDirectoryFlags::RESTART_SCANS,
                file_index: 0,
                file_id,
                file_name: "*".to_string(),
                output_buffer_length: 1_048_576,
            })
            .await
            .expect("query directory");
        assert_eq!(query.output_buffer, b"dir-entry");

        let read = connection
            .read(&Smb2ReadRequest {
                flags: Smb2ReadFlags::default(),
                length: 7,
                offset: 4096,
                file_id,
                minimum_count: 1,
                channel: Smb2ReadChannel::NONE,
                remaining_bytes: 0,
                channel_info: Vec::new(),
            })
            .await
            .expect("read");
        assert_eq!(read.data, b"payload");

        let write = connection
            .write(&Smb2WriteRequest {
                offset: 8192,
                file_id,
                channel: Smb2WriteChannel::NONE,
                remaining_bytes: 0,
                flags: Smb2WriteFlags::WRITE_THROUGH,
                data: b"payload".to_vec(),
                channel_info: Vec::new(),
            })
            .await
            .expect("write");
        assert_eq!(write.count, 7);

        let set_info = connection
            .set_info(&Smb2SetInfoRequest {
                info_type: Smb2InfoType::File,
                file_info_class: Smb2FileInfoClass::FILE_RENAME_INFORMATION,
                additional_information: 0,
                file_id,
                buffer: b"rename-info".to_vec(),
            })
            .await
            .expect("set info");
        assert_eq!(set_info, Smb2SetInfoResponse);

        let close = connection
            .close(&Smb2CloseRequest {
                flags: Smb2CloseFlags::POSTQUERY_ATTRIB,
                file_id,
            })
            .await
            .expect("close");
        assert_eq!(close.end_of_file, 7);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_request_round_trips_smb2_packet() {
        let (client, server) = duplex(512);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            assert_eq!(request_header.command, Smb2Command::Echo);
            assert_eq!(request_header.message_id, 0);
            assert_eq!(request_header.session_id, 0x0102_0304_0506_0708);
            assert_eq!(request_header.tree_id, 0x1112_1314);
            assert_eq!(&request[SMB2_HEADER_BYTES..], b"ping");

            let mut response_header =
                Smb2Header::new(request_header.command, request_header.message_id);
            response_header.credit_request_response = 8;
            response_header.status_or_channel_sequence = SmbNtStatus::SUCCESS.0;
            let mut response = response_header.encode().to_vec();
            response.extend_from_slice(b"pong");
            transport
                .send_packet(&response)
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        connection.set_session_id(0x0102_0304_0506_0708);
        connection.set_tree_id(0x1112_1314);
        connection.set_credit_request(4);

        let response = connection
            .request(Smb2Command::Echo, b"ping")
            .await
            .expect("request");

        assert_eq!(response.body, b"pong");
        assert_eq!(response.status(), SmbNtStatus::SUCCESS);
        assert_eq!(response.header.credit_request_response, 8);
        assert_eq!(connection.available_credits(), 8);
        assert_eq!(connection.next_message_id(), 1);
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_signs_requests_and_verifies_signed_responses() {
        let signing_key = b"session-signing-key".to_vec();
        let server_key = signing_key.clone();
        let (client, server) = duplex(512);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            assert_eq!(request_header.command, Smb2Command::Echo);
            assert_eq!(request_header.flags & SMB2_FLAGS_SIGNED, SMB2_FLAGS_SIGNED);
            assert!(verify_smb2_packet_hmac_sha256(&server_key, &request).expect("verify request"));

            let mut response_header =
                Smb2Header::new(request_header.command, request_header.message_id);
            response_header.flags |= SMB2_FLAGS_SIGNED;
            let mut response = response_packet(response_header, b"pong", &[]);
            sign_smb2_packet_hmac_sha256(&server_key, &mut response).expect("sign response");
            transport
                .send_packet(&response)
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        connection.set_hmac_sha256_signing_key(signing_key);
        let response = connection
            .request(Smb2Command::Echo, b"ping")
            .await
            .expect("request");

        assert_eq!(response.body, b"pong");
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_rejects_bad_signed_response() {
        let signing_key = b"session-signing-key".to_vec();
        let server_key = signing_key.clone();
        let (client, server) = duplex(512);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");

            let mut response_header =
                Smb2Header::new(request_header.command, request_header.message_id);
            response_header.flags |= SMB2_FLAGS_SIGNED;
            let mut response = response_packet(response_header, b"pong", &[]);
            sign_smb2_packet_hmac_sha256(&server_key, &mut response).expect("sign response");
            let last = response.last_mut().expect("response byte");
            *last ^= 0xff;
            transport
                .send_packet(&response)
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        connection.set_hmac_sha256_signing_key(signing_key);
        let error = connection
            .request(Smb2Command::Echo, b"ping")
            .await
            .expect_err("reject bad signature");

        assert!(matches!(
            error,
            SmbConnectionError::SignatureVerificationFailed
        ));
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_rejects_unsigned_response_after_signing_is_enabled() {
        let (client, server) = duplex(512);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(request_header.command, request_header.message_id),
                    b"unsigned",
                    &[],
                ))
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        connection.set_hmac_sha256_signing_key(b"session-signing-key".to_vec());
        let error = connection
            .request(Smb2Command::Echo, b"ping")
            .await
            .expect_err("reject unsigned response");

        assert!(matches!(error, SmbConnectionError::UnsignedResponse));
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_rejects_command_mismatch() {
        let (client, server) = duplex(512);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            let response = Smb2Header::new(Smb2Command::Cancel, request_header.message_id)
                .encode()
                .to_vec();
            transport
                .send_packet(&response)
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        let error = connection
            .request(Smb2Command::Echo, b"ping")
            .await
            .expect_err("reject");

        assert!(matches!(
            error,
            SmbConnectionError::CommandMismatch {
                expected: Smb2Command::Echo,
                actual: Smb2Command::Cancel,
            }
        ));
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_rejects_message_id_mismatch() {
        let (client, server) = duplex(512);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let request = transport.recv_packet().await.expect("receive request");
            let request_header = Smb2Header::decode(&request).expect("decode request");
            let response = Smb2Header::new(request_header.command, request_header.message_id + 1)
                .encode()
                .to_vec();
            transport
                .send_packet(&response)
                .await
                .expect("send response");
        });

        let mut connection = SmbConnection::new(client);
        let error = connection
            .request(Smb2Command::Echo, b"ping")
            .await
            .expect_err("reject");

        assert!(matches!(
            error,
            SmbConnectionError::MessageIdMismatch {
                expected: 0,
                actual: 1
            }
        ));
        server_task.await.expect("server task");
    }

    fn response_packet(header: Smb2Header, body: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut packet = header.encode().to_vec();
        packet.extend_from_slice(body);
        packet.extend_from_slice(tail);
        packet
    }

    fn negotiate_response_body(security: &[u8]) -> Vec<u8> {
        let mut body = vec![0; 65];
        put_u16(&mut body, 0, 65);
        put_u16(&mut body, 2, Smb2SecurityMode::SIGNING_ENABLED.0);
        put_u16(&mut body, 4, Smb2Dialect::Smb311.to_wire());
        body[8..24].copy_from_slice(&[0x55; 16]);
        put_u32(&mut body, 24, 0x0000_0040);
        put_u32(&mut body, 28, 1_048_576);
        put_u32(&mut body, 32, 1_048_576);
        put_u32(&mut body, 36, 1_048_576);
        put_u64(&mut body, 40, 1);
        put_u64(&mut body, 48, 2);
        let security_offset = (SMB2_HEADER_BYTES + body.len()) as u16;
        put_u16(&mut body, 56, security_offset);
        put_u16(&mut body, 58, security.len() as u16);
        body
    }

    fn session_setup_response_body(security: &[u8]) -> Vec<u8> {
        let mut body = vec![0; 8];
        put_u16(&mut body, 0, 9);
        put_u16(&mut body, 2, 1);
        let security_offset = (SMB2_HEADER_BYTES + body.len()) as u16;
        put_u16(&mut body, 4, security_offset);
        put_u16(&mut body, 6, security.len() as u16);
        body
    }

    fn tree_connect_response_body() -> Vec<u8> {
        let mut body = vec![0; 16];
        put_u16(&mut body, 0, 16);
        body[2] = Smb2ShareType::Disk.to_wire();
        put_u32(&mut body, 4, Smb2ShareFlags::DFS.0);
        put_u32(&mut body, 8, Smb2ShareCapabilities::DFS.0);
        put_u32(&mut body, 12, 0x001f_01ff);
        body
    }

    fn create_response_body(file_id: Smb2FileId) -> Vec<u8> {
        let mut body = vec![0; 88];
        put_u16(&mut body, 0, 89);
        body[2] = Smb2OplockLevel::None.to_wire();
        put_u32(&mut body, 4, Smb2CreateAction::Opened.to_wire());
        put_u64(&mut body, 8, 1);
        put_u64(&mut body, 16, 2);
        put_u64(&mut body, 24, 3);
        put_u64(&mut body, 32, 4);
        put_u64(&mut body, 40, 4096);
        put_u64(&mut body, 48, 7);
        put_u32(&mut body, 56, Smb2FileAttributes::ARCHIVE.0);
        put_u64(&mut body, 64, file_id.persistent);
        put_u64(&mut body, 72, file_id.volatile);
        body
    }

    fn read_response_body(data: &[u8]) -> Vec<u8> {
        let mut body = vec![0; 16 + data.len()];
        put_u16(&mut body, 0, 17);
        body[2] = (SMB2_HEADER_BYTES + 16) as u8;
        put_u32(&mut body, 4, data.len() as u32);
        body[16..].copy_from_slice(data);
        body
    }

    fn write_response_body(count: u32) -> Vec<u8> {
        let mut body = vec![0; 16];
        put_u16(&mut body, 0, 17);
        put_u32(&mut body, 4, count);
        body
    }

    fn set_info_response_body() -> Vec<u8> {
        let mut body = vec![0; 2];
        put_u16(&mut body, 0, 2);
        body
    }

    fn query_directory_response_body(buffer: &[u8]) -> Vec<u8> {
        let mut body = vec![0; 8 + buffer.len()];
        put_u16(&mut body, 0, 9);
        put_u16(&mut body, 2, (SMB2_HEADER_BYTES + 8) as u16);
        put_u32(&mut body, 4, buffer.len() as u32);
        body[8..].copy_from_slice(buffer);
        body
    }

    fn close_response_body() -> Vec<u8> {
        let mut body = vec![0; 60];
        put_u16(&mut body, 0, 60);
        put_u16(&mut body, 2, Smb2CloseFlags::POSTQUERY_ATTRIB.0);
        put_u64(&mut body, 8, 1);
        put_u64(&mut body, 16, 2);
        put_u64(&mut body, 24, 3);
        put_u64(&mut body, 32, 4);
        put_u64(&mut body, 40, 4096);
        put_u64(&mut body, 48, 7);
        put_u32(&mut body, 56, Smb2FileAttributes::ARCHIVE.0);
        body
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
