use crate::actor::SmbConnectionActor;
use crate::auth::ntlmssp::{
    default_negotiate_flags, ntlm_v2_authenticate_message, NtlmV2AuthenticateRequest,
    NtlmsspChallenge, NtlmsspNegotiate,
};
use crate::auth::spnego::{
    decode_neg_token_resp, spnego_initial_context_token_ntlm, spnego_neg_token_resp_ntlm,
};
use crate::connection::SmbConnectionError;
use crate::crypto::derive_smb2_signing_key;
use crate::packet::header::SmbNtStatus;
use crate::packet::negotiate::{Smb2Dialect, Smb2EncryptionCipher};
use crate::packet::session_setup::{
    Smb2SessionSetupRequest, Smb2SessionSetupResponse, Smb2SessionSetupSecurityMode,
};
use crate::transform::derive_smb3_encryption_keys;
use crate::SmbClientError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmV2SessionSetup {
    pub user: String,
    pub domain: String,
    pub password: String,
    pub workstation: String,
    pub timestamp: u64,
    pub client_challenge: [u8; 8],
    pub target_name: String,
    pub security: Option<NtlmV2SecurityConfig>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtlmV2SecurityConfig {
    pub dialect: Smb2Dialect,
    pub encrypt: bool,
    pub cipher: Smb2EncryptionCipher,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmV2Session {
    pub session_id: u64,
    pub session_base_key: [u8; 16],
}

#[derive(Debug, thiserror::Error)]
pub enum SmbSessionError {
    #[error(transparent)]
    Connection(#[from] SmbConnectionError),

    #[error(transparent)]
    Protocol(#[from] SmbClientError),

    #[error("SMB session setup {step} returned unexpected status 0x{status:08x}")]
    UnexpectedStatus { step: &'static str, status: u32 },

    #[error("SMB session setup response did not include an NTLM challenge token")]
    MissingChallengeToken,

    #[error("SMB authenticated session was downgraded to guest or null authentication")]
    AuthenticationDowngraded,

    #[error("SMB server did not establish the explicitly requested guest or null session")]
    GuestSessionNotGranted,
}

pub async fn authenticate_guest(connection: &SmbConnectionActor) -> Result<u64, SmbSessionError> {
    let exchange = send_session_setup(connection, Vec::new(), 0, false).await?;
    if !exchange.response.status().is_success() {
        return Err(SmbSessionError::UnexpectedStatus {
            step: "guest",
            status: exchange.response.status().0,
        });
    }
    if exchange.body.session_flags & 0x0003 == 0 {
        return Err(SmbSessionError::GuestSessionNotGranted);
    }

    Ok(connection.state().await?.session_id)
}

pub async fn authenticate_ntlm_v2(
    connection: &SmbConnectionActor,
    setup: &NtlmV2SessionSetup,
) -> Result<NtlmV2Session, SmbSessionError> {
    let negotiate = NtlmsspNegotiate {
        flags: default_negotiate_flags(),
        domain: setup.domain.clone(),
        workstation: setup.workstation.clone(),
    }
    .encode()?;
    let security_buffer = spnego_initial_context_token_ntlm(&negotiate)?;
    let first = send_session_setup(connection, security_buffer, 0, true).await?;
    if first.response.status() != SmbNtStatus::MORE_PROCESSING_REQUIRED {
        return Err(SmbSessionError::UnexpectedStatus {
            step: "challenge",
            status: first.response.status().0,
        });
    }

    let spnego = decode_neg_token_resp(&first.body.security_buffer)?;
    let challenge_token = spnego
        .response_token
        .as_deref()
        .ok_or(SmbSessionError::MissingChallengeToken)?;
    let challenge = NtlmsspChallenge::decode(challenge_token)?;
    let authenticate = ntlm_v2_authenticate_message(NtlmV2AuthenticateRequest {
        user: &setup.user,
        domain: &setup.domain,
        password: &setup.password,
        workstation: &setup.workstation,
        challenge: &challenge,
        timestamp: setup.timestamp,
        client_challenge: setup.client_challenge,
        negotiate_message: &negotiate,
        target_name: &setup.target_name,
    })?;
    let authenticate_security = spnego_neg_token_resp_ntlm(&authenticate.message)?;
    let second = if let Some(security) = setup.security {
        send_final_session_setup(
            connection,
            authenticate_security,
            first.response.header.session_id,
            authenticate.exported_session_key.to_vec(),
            security.dialect,
        )
        .await?
    } else {
        send_session_setup(
            connection,
            authenticate_security,
            first.response.header.session_id,
            true,
        )
        .await?
    };
    if !second.response.status().is_success() {
        return Err(SmbSessionError::UnexpectedStatus {
            step: "authenticate",
            status: second.response.status().0,
        });
    }
    if second.body.session_flags & 0x0003 != 0 {
        return Err(SmbSessionError::AuthenticationDowngraded);
    }
    if let Some(security) = setup.security {
        let preauth_integrity_hash = connection.state().await?.preauth_integrity_hash;
        let signing_key = derive_smb2_signing_key(
            &authenticate.exported_session_key,
            security.dialect,
            &preauth_integrity_hash,
        )?;
        connection.set_signing_key(signing_key).await?;
        if security.encrypt {
            let keys = derive_smb3_encryption_keys(
                &authenticate.exported_session_key,
                security.dialect,
                &preauth_integrity_hash,
                security.cipher,
            )?;
            connection.enable_encryption(keys).await?;
        }
    }

    Ok(NtlmV2Session {
        session_id: connection.state().await?.session_id,
        session_base_key: authenticate.exported_session_key,
    })
}

struct SessionSetupExchange {
    response: crate::SmbResponse,
    body: Smb2SessionSetupResponse,
}

async fn send_session_setup(
    connection: &SmbConnectionActor,
    security_buffer: Vec<u8>,
    previous_session_id: u64,
    signing_required: bool,
) -> Result<SessionSetupExchange, SmbSessionError> {
    let request = Smb2SessionSetupRequest {
        flags: 0,
        security_mode: if signing_required {
            Smb2SessionSetupSecurityMode::SIGNING_REQUIRED
        } else {
            Smb2SessionSetupSecurityMode::SIGNING_ENABLED
        },
        capabilities: 0,
        channel: 0,
        previous_session_id,
        security_buffer,
    };
    let response = connection.request_session_setup(request.encode()).await?;
    let body = Smb2SessionSetupResponse::decode_packet(&response.to_packet())?;
    Ok(SessionSetupExchange { response, body })
}

async fn send_final_session_setup(
    connection: &SmbConnectionActor,
    security_buffer: Vec<u8>,
    previous_session_id: u64,
    session_key: Vec<u8>,
    dialect: Smb2Dialect,
) -> Result<SessionSetupExchange, SmbSessionError> {
    let request = Smb2SessionSetupRequest {
        flags: 0,
        security_mode: Smb2SessionSetupSecurityMode::SIGNING_REQUIRED,
        capabilities: 0,
        channel: 0,
        previous_session_id,
        security_buffer,
    };
    let response = connection
        .request_final_session_setup(request.encode(), session_key, dialect)
        .await?;
    let body = Smb2SessionSetupResponse::decode_packet(&response.to_packet())?;
    Ok(SessionSetupExchange { response, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ntlmssp::default_negotiate_flags;
    use crate::auth::spnego::{encode_neg_token_resp, SpnegoNegState};
    use crate::crypto::{
        sign_smb2_packet_hmac_sha256, smb311_preauth_hash_sequence, verify_smb2_packet_hmac_sha256,
    };
    use crate::packet::header::Smb2Command;
    use crate::packet::header::{Smb2Header, SMB2_FLAGS_SIGNED, SMB2_HEADER_BYTES};
    use crate::test_support::*;
    use crate::transform::{
        derive_smb311_encryption_keys, is_smb3_transform, Smb3EncryptionContext,
    };
    use crate::transport::DirectTcpTransport;
    use crate::wire::encode_utf16le;
    use tokio::io::duplex;

    #[tokio::test]
    async fn ntlm_v2_session_setup_exchanges_spnego_tokens_and_installs_signing_key() {
        let (client, server) = duplex(4096);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);

            let first = transport.recv_packet().await.expect("first request");
            let first_header = Smb2Header::decode(&first).expect("first header");
            assert_eq!(first_header.command, Smb2Command::SessionSetup);
            let first_body =
                Smb2SessionSetupRequest::decode(&first[SMB2_HEADER_BYTES..]).expect("first body");
            assert!(first_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            assert!(first_body.security_buffer.starts_with(&[0x60]));
            assert!(find_bytes(&first_body.security_buffer, b"NTLMSSP\0"));

            let session_id = 0x0102_0304_0506_0708;
            let challenge = ntlm_challenge_message();
            let challenge_spnego = encode_neg_token_resp(
                Some(SpnegoNegState::AcceptIncomplete),
                Some(true),
                Some(&challenge),
                None,
            )
            .expect("challenge spnego");
            let mut challenge_header =
                Smb2Header::new(Smb2Command::SessionSetup, first_header.message_id);
            challenge_header.status_or_channel_sequence = SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
            challenge_header.session_id = session_id;
            let challenge_response = response_packet(
                challenge_header,
                &[
                    session_setup_response_body(&challenge_spnego).as_slice(),
                    &challenge_spnego,
                ]
                .concat(),
            );
            transport
                .send_packet(&challenge_response)
                .await
                .expect("challenge response");

            let second = transport.recv_packet().await.expect("second request");
            let second_header = Smb2Header::decode(&second).expect("second header");
            assert_eq!(second_header.command, Smb2Command::SessionSetup);
            assert_eq!(second_header.session_id, session_id);
            assert_eq!(second_header.flags & SMB2_FLAGS_SIGNED, SMB2_FLAGS_SIGNED);
            assert!(
                verify_smb2_packet_hmac_sha256(&expected_session_base_key(), &second)
                    .expect("verify final session setup request")
            );
            let second_body =
                Smb2SessionSetupRequest::decode(&second[SMB2_HEADER_BYTES..]).expect("second body");
            assert!(second_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            let authenticate = decode_neg_token_resp(&second_body.security_buffer)
                .expect("authenticate spnego")
                .response_token
                .expect("authenticate token");
            assert!(authenticate.starts_with(b"NTLMSSP\0"));
            assert_eq!(
                u32::from_le_bytes([
                    authenticate[8],
                    authenticate[9],
                    authenticate[10],
                    authenticate[11]
                ]),
                3
            );

            let mut final_header =
                Smb2Header::new(Smb2Command::SessionSetup, second_header.message_id);
            final_header.status_or_channel_sequence = SmbNtStatus::SUCCESS.0;
            final_header.session_id = session_id;
            final_header.flags |= SMB2_FLAGS_SIGNED;
            let mut final_response = response_packet(
                final_header,
                &[session_setup_response_body(&[]).as_slice(), &[]].concat(),
            );
            sign_smb2_packet_hmac_sha256(&expected_session_base_key(), &mut final_response)
                .expect("sign final response");
            transport
                .send_packet(&final_response)
                .await
                .expect("final response");

            let signed = transport.recv_packet().await.expect("signed followup");
            let signed_header = Smb2Header::decode(&signed).expect("signed header");
            assert_eq!(signed_header.command, Smb2Command::Echo);
            assert_eq!(signed_header.flags & SMB2_FLAGS_SIGNED, SMB2_FLAGS_SIGNED);
            let signing_key = expected_session_base_key();
            assert!(verify_smb2_packet_hmac_sha256(&signing_key, &signed).expect("verify signed"));
            let mut echo_response_header =
                Smb2Header::new(Smb2Command::Echo, signed_header.message_id);
            echo_response_header.flags |= SMB2_FLAGS_SIGNED;
            echo_response_header.session_id = session_id;
            let mut echo_response =
                response_packet(echo_response_header, &[b"ok".as_slice(), &[]].concat());
            sign_smb2_packet_hmac_sha256(&signing_key, &mut echo_response)
                .expect("sign echo response");
            transport
                .send_packet(&echo_response)
                .await
                .expect("echo response");
        });

        let connection = SmbConnectionActor::spawn(client);
        let setup = NtlmV2SessionSetup {
            user: "User".into(),
            domain: "Domain".into(),
            password: "Password".into(),
            workstation: "WORKSTATION".into(),
            timestamp: 0x0123_4567_89ab_cdef,
            client_challenge: hex_array_8("ffffff0011223344"),
            target_name: "cifs/server".into(),
            security: Some(NtlmV2SecurityConfig {
                dialect: Smb2Dialect::Smb210,
                encrypt: false,
                cipher: Smb2EncryptionCipher::Aes128Gcm,
            }),
        };
        let session = authenticate_ntlm_v2(&connection, &setup)
            .await
            .expect("authenticate");

        assert_eq!(session.session_id, 0x0102_0304_0506_0708);
        assert_eq!(session.session_base_key, expected_session_base_key());
        connection
            .request(Smb2Command::Echo, b"probe".to_vec())
            .await
            .expect("signed followup");
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn actor_ntlm_v2_session_setup_enables_smb311_encryption() {
        let (client, server) = duplex(4096);
        let actor = SmbConnectionActor::spawn(client);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);

            let first = transport.recv_packet().await.expect("first request");
            let first_header = Smb2Header::decode(&first).expect("first header");
            assert_eq!(first_header.command, Smb2Command::SessionSetup);
            let first_body =
                Smb2SessionSetupRequest::decode(&first[SMB2_HEADER_BYTES..]).expect("first body");
            assert!(first_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            assert!(first_body.security_buffer.starts_with(&[0x60]));
            assert!(find_bytes(&first_body.security_buffer, b"NTLMSSP\0"));

            let session_id = 0x0102_0304_0506_0708;
            let challenge = ntlm_challenge_message();
            let challenge_spnego = encode_neg_token_resp(
                Some(SpnegoNegState::AcceptIncomplete),
                Some(true),
                Some(&challenge),
                None,
            )
            .expect("challenge spnego");
            let mut challenge_header =
                Smb2Header::new(Smb2Command::SessionSetup, first_header.message_id);
            challenge_header.status_or_channel_sequence = SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
            challenge_header.session_id = session_id;
            let challenge_response = response_packet(
                challenge_header,
                &[
                    session_setup_response_body(&challenge_spnego).as_slice(),
                    &challenge_spnego,
                ]
                .concat(),
            );
            transport
                .send_packet(&challenge_response)
                .await
                .expect("challenge response");

            let second = transport.recv_packet().await.expect("second request");
            let second_header = Smb2Header::decode(&second).expect("second header");
            assert_eq!(second_header.command, Smb2Command::SessionSetup);
            assert_eq!(second_header.session_id, session_id);
            assert_eq!(second_header.flags & SMB2_FLAGS_SIGNED, SMB2_FLAGS_SIGNED);
            let pre_final_hash =
                smb311_preauth_hash_sequence([first.as_slice(), challenge_response.as_slice()]);
            assert!(derive_smb2_signing_key(
                &expected_session_base_key(),
                Smb2Dialect::Smb311,
                &pre_final_hash,
            )
            .expect("derive pre-final signing key")
            .verify_packet(&second)
            .expect("verify final session setup request"));
            let second_body =
                Smb2SessionSetupRequest::decode(&second[SMB2_HEADER_BYTES..]).expect("second body");
            assert!(second_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            assert_eq!(second_body.previous_session_id, session_id);
            let authenticate = decode_neg_token_resp(&second_body.security_buffer)
                .expect("authenticate spnego")
                .response_token
                .expect("authenticate token");
            assert!(authenticate.starts_with(b"NTLMSSP\0"));

            let mut final_header =
                Smb2Header::new(Smb2Command::SessionSetup, second_header.message_id);
            final_header.status_or_channel_sequence = SmbNtStatus::SUCCESS.0;
            final_header.session_id = session_id;
            let preauth_hash = smb311_preauth_hash_sequence([
                first.as_slice(),
                challenge_response.as_slice(),
                second.as_slice(),
            ]);
            final_header.flags |= SMB2_FLAGS_SIGNED;
            let mut final_response = response_packet(
                final_header,
                &[session_setup_response_body(&[]).as_slice(), &[]].concat(),
            );
            derive_smb2_signing_key(
                &expected_session_base_key(),
                Smb2Dialect::Smb311,
                &preauth_hash,
            )
            .expect("derive signing key")
            .sign_packet(&mut final_response)
            .expect("sign final response");
            transport
                .send_packet(&final_response)
                .await
                .expect("final response");

            let client_keys = derive_smb311_encryption_keys(
                &expected_session_base_key(),
                Smb2Dialect::Smb311,
                &preauth_hash,
            )
            .expect("derive encryption keys");
            let mut encryption =
                Smb3EncryptionContext::new(client_keys.reversed()).expect("server encryption");
            let encrypted = transport.recv_packet().await.expect("encrypted followup");
            assert!(is_smb3_transform(&encrypted));
            let request = encryption
                .decrypt(session_id, &encrypted)
                .expect("decrypt followup");
            let request_header = Smb2Header::decode(&request).expect("request header");
            assert_eq!(request_header.command, Smb2Command::Echo);
            assert_eq!(request_header.session_id, session_id);
            assert_eq!(request_header.flags & SMB2_FLAGS_SIGNED, 0);
            let mut echo_response_header =
                Smb2Header::new(Smb2Command::Echo, request_header.message_id);
            echo_response_header.session_id = session_id;
            let echo_response =
                response_packet(echo_response_header, &[b"ok".as_slice(), &[]].concat());
            let encrypted_response = encryption
                .encrypt(session_id, &echo_response)
                .expect("encrypt response");
            transport
                .send_packet(&encrypted_response)
                .await
                .expect("echo response");
        });

        let setup = NtlmV2SessionSetup {
            user: "User".into(),
            domain: "Domain".into(),
            password: "Password".into(),
            workstation: "WORKSTATION".into(),
            timestamp: 0x0123_4567_89ab_cdef,
            client_challenge: hex_array_8("ffffff0011223344"),
            target_name: "cifs/server".into(),
            security: Some(NtlmV2SecurityConfig {
                dialect: Smb2Dialect::Smb311,
                encrypt: true,
                cipher: Smb2EncryptionCipher::Aes128Gcm,
            }),
        };
        let session = authenticate_ntlm_v2(&actor, &setup)
            .await
            .expect("authenticate");

        assert_eq!(session.session_id, 0x0102_0304_0506_0708);
        assert_eq!(session.session_base_key, expected_session_base_key());
        assert_eq!(
            actor.state().await.expect("state").session_id,
            0x0102_0304_0506_0708
        );
        assert!(actor.state().await.expect("state").encryption_enabled);
        actor
            .request(Smb2Command::Echo, b"probe".to_vec())
            .await
            .expect("encrypted followup");
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn ntlm_v2_session_setup_rejects_missing_challenge_token() {
        let (client, server) = duplex(2048);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);
            let first = transport.recv_packet().await.expect("first request");
            let first_header = Smb2Header::decode(&first).expect("first header");
            let empty_spnego = encode_neg_token_resp(
                Some(SpnegoNegState::AcceptIncomplete),
                Some(true),
                None,
                None,
            )
            .expect("empty spnego");
            let mut challenge_header =
                Smb2Header::new(Smb2Command::SessionSetup, first_header.message_id);
            challenge_header.status_or_channel_sequence = SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
            challenge_header.session_id = 1;
            transport
                .send_packet(&response_packet(
                    challenge_header,
                    &[
                        session_setup_response_body(&empty_spnego).as_slice(),
                        &empty_spnego,
                    ]
                    .concat(),
                ))
                .await
                .expect("challenge response");
        });

        let connection = SmbConnectionActor::spawn(client);
        let setup = NtlmV2SessionSetup {
            user: "User".into(),
            domain: "Domain".into(),
            password: "Password".into(),
            workstation: "WORKSTATION".into(),
            timestamp: 0x0123_4567_89ab_cdef,
            client_challenge: hex_array_8("ffffff0011223344"),
            target_name: "cifs/server".into(),
            security: None,
        };
        let error = authenticate_ntlm_v2(&connection, &setup)
            .await
            .expect_err("missing challenge");

        assert!(matches!(error, SmbSessionError::MissingChallengeToken));
        server_task.await.expect("server task");
    }

    fn ntlm_challenge_message() -> Vec<u8> {
        let target_name = encode_utf16le("Domain");
        let target_info = [0u8; 4];
        let target_name_offset = 48;
        let target_info_offset = target_name_offset + target_name.len();
        let mut bytes = Vec::with_capacity(target_info_offset + target_info.len());
        bytes.extend_from_slice(b"NTLMSSP\0");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        push_security_buffer(&mut bytes, target_name.len(), target_name_offset);
        let flags =
            default_negotiate_flags().0 & !crate::auth::ntlmssp::NtlmsspNegotiateFlags::KEY_EXCH.0;
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(&hex_array_8("0123456789abcdef"));
        bytes.extend_from_slice(&[0; 8]);
        push_security_buffer(&mut bytes, target_info.len(), target_info_offset);
        bytes.extend_from_slice(&target_name);
        bytes.extend_from_slice(&target_info);
        bytes
    }

    fn expected_session_base_key() -> [u8; 16] {
        let negotiate = NtlmsspNegotiate {
            flags: default_negotiate_flags(),
            domain: "Domain".into(),
            workstation: "WORKSTATION".into(),
        }
        .encode()
        .expect("negotiate");
        let challenge_bytes = ntlm_challenge_message();
        let challenge = NtlmsspChallenge::decode(&challenge_bytes).expect("challenge");
        ntlm_v2_authenticate_message(NtlmV2AuthenticateRequest {
            user: "User",
            domain: "Domain",
            password: "Password",
            workstation: "WORKSTATION",
            challenge: &challenge,
            timestamp: 0x0123_4567_89ab_cdef,
            client_challenge: hex_array_8("ffffff0011223344"),
            negotiate_message: &negotiate,
            target_name: "cifs/server",
        })
        .expect("authenticate")
        .exported_session_key
    }

    fn session_setup_response_body(security: &[u8]) -> Vec<u8> {
        let mut body = vec![0; 8];
        body[0..2].copy_from_slice(&9u16.to_le_bytes());
        if security.is_empty() {
            body[4..6].copy_from_slice(&0u16.to_le_bytes());
        } else {
            let security_offset = (SMB2_HEADER_BYTES + body.len()) as u16;
            body[4..6].copy_from_slice(&security_offset.to_le_bytes());
        }
        body[6..8].copy_from_slice(&(security.len() as u16).to_le_bytes());
        body
    }

    fn push_security_buffer(bytes: &mut Vec<u8>, length: usize, offset: usize) {
        bytes.extend_from_slice(&(length as u16).to_le_bytes());
        bytes.extend_from_slice(&(length as u16).to_le_bytes());
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|candidate| candidate == needle)
    }
}
