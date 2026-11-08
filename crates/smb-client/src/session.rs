use crate::actor::SmbConnectionActor;
use crate::auth::ntlmssp::{
    default_negotiate_flags, mech_list_mic, ntlm_v2_authenticate_message, verify_mech_list_mic,
    NtlmV2AuthenticateRequest, NtlmsspChallenge, NtlmsspNegotiate, NtlmsspNegotiateFlags,
};
use crate::auth::spnego::{
    decode_neg_token_resp, encode_neg_token_resp, ntlm_mech_type_list,
    spnego_initial_context_token_ntlm, SpnegoNegState,
};
use crate::connection::SmbConnectionError;
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
    let exchange = send_session_setup(connection, Vec::new(), false, None).await?;
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
    }
    .encode()?;
    let security_buffer = spnego_initial_context_token_ntlm(&negotiate)?;
    let first = send_session_setup(connection, security_buffer, true, None).await?;
    if first.response.status() != SmbNtStatus::MORE_PROCESSING_REQUIRED {
        return Err(SmbSessionError::UnexpectedStatus {
            step: "challenge",
            status: first.response.status().0,
        });
    }

    let spnego = decode_neg_token_resp(&first.body.security_buffer)?;
    if !matches!(
        spnego.neg_state,
        Some(SpnegoNegState::AcceptIncomplete | SpnegoNegState::RequestMic)
    ) || spnego.mech_list_mic.is_some()
    {
        return Err(unsupported_continuation());
    }
    // RequestMic asks the peer to send its MIC; it neither promises nor requires
    // a MIC from the other side. Verify any server MIC that is present, but do
    // not require one on final success.
    let client_mic_sent = spnego.neg_state == Some(SpnegoNegState::RequestMic);
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
    let flags = NtlmsspNegotiateFlags(challenge.flags.0 & default_negotiate_flags().0);
    let mech_list = ntlm_mech_type_list();
    let client_mic = if client_mic_sent {
        Some(mech_list_mic(
            &authenticate.exported_session_key,
            flags,
            false,
            &mech_list,
        )?)
    } else {
        None
    };
    let authenticate_security = encode_neg_token_resp(
        None,
        None,
        Some(&authenticate.message),
        client_mic.as_ref().map(|mic| mic.as_slice()),
    )?;
    let signing = || {
        setup
            .security
            .map(|security| (authenticate.exported_session_key.to_vec(), security.dialect))
    };
    let mut second = send_session_setup(connection, authenticate_security, true, signing()).await?;
    let mut server_mic_verified = false;
    let needs_mic = process_ntlm_completion(
        &second,
        &authenticate.exported_session_key,
        flags,
        &mut server_mic_verified,
    )?;
    if needs_mic {
        // NTLM has no further mechanism token after AUTHENTICATE. The only
        // supported extra leg is the Samba-style AcceptIncomplete response
        // carrying a verified server MIC; an initial RequestMic was already
        // handled on the AUTHENTICATE token above.
        if client_mic_sent {
            // The client MIC was already sent (initial RequestMic); a second
            // MIC request has no supported continuation.
            return Err(unsupported_continuation());
        }
        let mic = mech_list_mic(&authenticate.exported_session_key, flags, false, &mech_list)?;
        let security_buffer = encode_neg_token_resp(None, None, None, Some(&mic))?;
        second = send_session_setup(connection, security_buffer, true, signing()).await?;
        if process_ntlm_completion(
            &second,
            &authenticate.exported_session_key,
            flags,
            &mut server_mic_verified,
        )? {
            return Err(unsupported_continuation());
        }
    }
    // A peer-provided MIC is verified when present; it does not by itself
    // require a client MIC after the peer has declared SPNEGO complete.
    // A requested client MIC does not require a server MIC on final success:
    // RequestMic asks the peer to send a MIC, it does not itself demand one.
    if second.body.session_flags & 0x0003 != 0 {
        return Err(SmbSessionError::AuthenticationDowngraded);
    }
    if let Some(security) = setup.security {
        let preauth_integrity_hash = connection.state().await?.preauth_integrity_hash;
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

fn unsupported_continuation() -> SmbSessionError {
    SmbClientError::InvalidAsn1Value {
        kind: "spnego.ntlm.continuation",
    }
    .into()
}

/// Feed GSS on both SMB success and MORE_PROCESSING_REQUIRED. SMB success is
/// final only if SPNEGO also completed; never treat an NTLM MIC leg as success.
fn process_ntlm_completion(
    exchange: &SessionSetupExchange,
    key: &[u8; 16],
    flags: NtlmsspNegotiateFlags,
    server_mic_verified: &mut bool,
) -> Result<bool, SmbSessionError> {
    let status = exchange.response.status();
    if !status.is_success() && status != SmbNtStatus::MORE_PROCESSING_REQUIRED {
        return Err(SmbSessionError::UnexpectedStatus {
            step: "authenticate",
            status: status.0,
        });
    }
    if exchange.body.security_buffer.is_empty() {
        return if status.is_success() {
            Ok(false)
        } else {
            Err(unsupported_continuation())
        };
    }
    let token = decode_neg_token_resp(&exchange.body.security_buffer)?;
    if token
        .response_token
        .as_ref()
        .is_some_and(|token| !token.is_empty())
    {
        return Err(unsupported_continuation());
    }
    if let Some(mic) = token.mech_list_mic {
        if *server_mic_verified {
            return Err(unsupported_continuation());
        }
        verify_mech_list_mic(key, flags, &ntlm_mech_type_list(), &mic)?;
        *server_mic_verified = true;
    }
    if status.is_success() {
        if !matches!(
            token.neg_state,
            None | Some(SpnegoNegState::AcceptCompleted)
        ) {
            return Err(unsupported_continuation());
        }
        Ok(false)
    } else if token.neg_state == Some(SpnegoNegState::AcceptIncomplete) && *server_mic_verified {
        Ok(true)
    } else {
        Err(unsupported_continuation())
    }
}

struct SessionSetupExchange {
    response: crate::SmbResponse,
    body: Smb2SessionSetupResponse,
}

/// `signing` supplies a pending response verification key; only final SMB
/// success installs it. It implies SIGNING_REQUIRED.
async fn send_session_setup(
    connection: &SmbConnectionActor,
    security_buffer: Vec<u8>,
    signing_required: bool,
    signing: Option<(Vec<u8>, Smb2Dialect)>,
) -> Result<SessionSetupExchange, SmbSessionError> {
    let request = Smb2SessionSetupRequest {
        flags: 0,
        security_mode: if signing_required || signing.is_some() {
            Smb2SessionSetupSecurityMode::SIGNING_REQUIRED
        } else {
            Smb2SessionSetupSecurityMode::SIGNING_ENABLED
        },
        capabilities: 0,
        channel: 0,
        // This is a new authentication; the continuation session ID belongs in the SMB2 header.
        previous_session_id: 0,
        security_buffer,
    };
    let response = connection
        .request_session_setup(request.encode(), signing)
        .await?;
    if !response.status().is_success() && response.status() != SmbNtStatus::MORE_PROCESSING_REQUIRED
    {
        return Err(SmbSessionError::UnexpectedStatus {
            step: "response",
            status: response.status().0,
        });
    }
    let body = Smb2SessionSetupResponse::decode_packet(&response.to_packet())?;
    Ok(SessionSetupExchange { response, body })
}

#[cfg(test)]
mod tests {
    #[test]
    fn ntlm_completion_rejects_unsupported_tokens_states_and_bad_mics_without_disclosure() {
        let key = [0x41; 16];
        let flags = default_negotiate_flags();
        let mic = mech_list_mic(&key, flags, true, &ntlm_mech_type_list()).unwrap();
        let encode = |state, token: Option<&[u8]>, mic: Option<&[u8]>| {
            encode_neg_token_resp(state, None, token, mic).unwrap()
        };
        for (status, security) in [
            (SmbNtStatus::MORE_PROCESSING_REQUIRED, vec![]),
            (SmbNtStatus::MORE_PROCESSING_REQUIRED, vec![0xa1, 0xff]),
            (
                SmbNtStatus::MORE_PROCESSING_REQUIRED,
                encode(Some(SpnegoNegState::Reject), None, None),
            ),
            (
                SmbNtStatus::MORE_PROCESSING_REQUIRED,
                encode(Some(SpnegoNegState::AcceptCompleted), None, None),
            ),
            (
                SmbNtStatus::MORE_PROCESSING_REQUIRED,
                encode(Some(SpnegoNegState::AcceptIncomplete), None, None),
            ),
            (
                SmbNtStatus::MORE_PROCESSING_REQUIRED,
                encode(Some(SpnegoNegState::RequestMic), None, None),
            ),
            (
                SmbNtStatus::MORE_PROCESSING_REQUIRED,
                encode(
                    Some(SpnegoNegState::RequestMic),
                    Some(b"SYNTHETIC_PRIVATE_TOKEN"),
                    None,
                ),
            ),
            (
                SmbNtStatus::MORE_PROCESSING_REQUIRED,
                encode(
                    Some(SpnegoNegState::RequestMic),
                    None,
                    Some(b"SYNTHETIC_PRIVATE_MIC"),
                ),
            ),
            (
                SmbNtStatus::SUCCESS,
                encode(Some(SpnegoNegState::RequestMic), None, None),
            ),
        ] {
            let mut header = Smb2Header::new(Smb2Command::SessionSetup, 1);
            header.status_or_channel_sequence = status.0;
            let exchange = SessionSetupExchange {
                response: crate::SmbResponse {
                    header,
                    body: vec![],
                },
                body: Smb2SessionSetupResponse {
                    session_flags: 0,
                    security_buffer: security,
                },
            };
            let error = process_ntlm_completion(&exchange, &key, flags, &mut false).unwrap_err();
            let rendered = format!("{error} {error:?}");
            assert!(rendered.len() < 256);
            assert!(!rendered.contains("SYNTHETIC_PRIVATE"));
            assert!(!rendered.contains("65, 65"));
        }
        for status in [SmbNtStatus::SUCCESS, SmbNtStatus::MORE_PROCESSING_REQUIRED] {
            let mut header = Smb2Header::new(Smb2Command::SessionSetup, 1);
            header.status_or_channel_sequence = status.0;
            let exchange = SessionSetupExchange {
                response: crate::SmbResponse {
                    header,
                    body: vec![],
                },
                body: Smb2SessionSetupResponse {
                    session_flags: 0,
                    security_buffer: encode(
                        Some(if status.is_success() {
                            SpnegoNegState::AcceptCompleted
                        } else {
                            SpnegoNegState::AcceptIncomplete
                        }),
                        None,
                        Some(&mic),
                    ),
                },
            };
            let mut verified = false;
            assert_eq!(
                process_ntlm_completion(&exchange, &key, flags, &mut verified).unwrap(),
                !status.is_success()
            );
            assert!(verified);
        }
    }

    use super::*;
    use crate::auth::ntlmssp::default_negotiate_flags;
    use crate::auth::spnego::{encode_neg_token_resp, SpnegoNegState};
    use crate::crypto::derive_smb2_signing_key;
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
        // No MIC, mutual MIC, and client-only MIC: a challenge RequestMic that
        // the server answers without its own MIC must still succeed.
        for (request_mic, server_returns_mic) in
            [(false, false), (false, true), (true, true), (true, false)]
        {
            ntlm_direct_completion(request_mic, server_returns_mic).await;
        }
    }

    async fn ntlm_direct_completion(request_mic: bool, server_returns_mic: bool) {
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
                Some(if request_mic {
                    SpnegoNegState::RequestMic
                } else {
                    SpnegoNegState::AcceptIncomplete
                }),
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
            assert_eq!(second_header.flags & SMB2_FLAGS_SIGNED, 0);
            let second_body =
                Smb2SessionSetupRequest::decode(&second[SMB2_HEADER_BYTES..]).expect("second body");
            assert!(second_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            let token =
                decode_neg_token_resp(&second_body.security_buffer).expect("authenticate spnego");
            let flags = NtlmsspChallenge::decode(&challenge).unwrap().flags;
            if request_mic {
                assert_eq!(
                    token.mech_list_mic.as_deref(),
                    Some(
                        mech_list_mic(
                            &expected_session_base_key(),
                            flags,
                            false,
                            &ntlm_mech_type_list()
                        )
                        .unwrap()
                        .as_slice()
                    )
                );
            } else {
                assert!(token.mech_list_mic.is_none());
            }
            let server_mic = if server_returns_mic {
                Some(
                    mech_list_mic(
                        &expected_session_base_key(),
                        flags,
                        true,
                        &ntlm_mech_type_list(),
                    )
                    .unwrap(),
                )
            } else {
                None
            };
            let authenticate = token.response_token.expect("authenticate token");
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
            let completion = if let Some(mic) = server_mic {
                encode_neg_token_resp(
                    Some(SpnegoNegState::AcceptCompleted),
                    None,
                    None,
                    Some(&mic),
                )
                .unwrap()
            } else {
                Vec::new()
            };
            let mut final_response = response_packet(
                final_header,
                &[session_setup_response_body(&completion), completion].concat(),
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
    async fn ntlm_v2_rejects_tampered_server_mic() {
        // The server answers SUCCESS with a tampered mechListMIC. The client
        // must verify the MIC, fail closed, and never return a session.
        let (client, server) = duplex(4096);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);

            let first = transport.recv_packet().await.expect("first request");
            let first_header = Smb2Header::decode(&first).expect("first header");
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
            let flags = NtlmsspChallenge::decode(&challenge).unwrap().flags;
            let mut bad = mech_list_mic(
                &expected_session_base_key(),
                flags,
                true,
                &ntlm_mech_type_list(),
            )
            .unwrap();
            bad[0] ^= 0xff;
            let completion = encode_neg_token_resp(
                Some(SpnegoNegState::AcceptCompleted),
                None,
                None,
                Some(&bad),
            )
            .expect("bad completion");
            let mut final_header =
                Smb2Header::new(Smb2Command::SessionSetup, second_header.message_id);
            final_header.status_or_channel_sequence = SmbNtStatus::SUCCESS.0;
            final_header.session_id = session_id;
            final_header.flags |= SMB2_FLAGS_SIGNED;
            let mut final_response = response_packet(
                final_header,
                &[session_setup_response_body(&completion), completion].concat(),
            );
            sign_smb2_packet_hmac_sha256(&expected_session_base_key(), &mut final_response)
                .expect("sign final response");
            transport
                .send_packet(&final_response)
                .await
                .expect("final response");
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
        let error = authenticate_ntlm_v2(&connection, &setup)
            .await
            .expect_err("tampered server MIC must fail");
        let rendered = format!("{error} {error:?}");
        assert!(rendered.contains("spnego"), "unexpected error: {rendered}");
        assert!(!rendered.contains("ffffff00"));
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn ntlm_v2_rejects_second_mic_request_after_client_mic() {
        // The challenge already requested the client MIC, so a further MPR
        // RequestMic has no supported continuation and must fail. The client
        // must not send a second MIC leg.
        let (client, server) = duplex(4096);
        let server_task = tokio::spawn(async move {
            let mut transport = DirectTcpTransport::new(server);

            let first = transport.recv_packet().await.expect("first request");
            let first_header = Smb2Header::decode(&first).expect("first header");
            let session_id = 0x0102_0304_0506_0708;
            let challenge = ntlm_challenge_message();
            let challenge_spnego = encode_neg_token_resp(
                Some(SpnegoNegState::RequestMic),
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
            let second_body =
                Smb2SessionSetupRequest::decode(&second[SMB2_HEADER_BYTES..]).expect("second body");
            let token =
                decode_neg_token_resp(&second_body.security_buffer).expect("authenticate spnego");
            assert!(token.mech_list_mic.is_some(), "client must send its MIC");

            let again = encode_neg_token_resp(Some(SpnegoNegState::RequestMic), None, None, None)
                .expect("second mic demand");
            let mut again_header =
                Smb2Header::new(Smb2Command::SessionSetup, second_header.message_id);
            again_header.session_id = session_id;
            again_header.status_or_channel_sequence = SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
            let again_response = response_packet(
                again_header,
                &[session_setup_response_body(&again), again].concat(),
            );
            transport
                .send_packet(&again_response)
                .await
                .expect("second mic demand");
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
            .expect_err("second MIC request must fail");
        let rendered = format!("{error} {error:?}");
        assert!(
            rendered.contains("continuation"),
            "unexpected error: {rendered}"
        );
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
            assert_eq!(second_header.flags & SMB2_FLAGS_SIGNED, 0);
            let second_body =
                Smb2SessionSetupRequest::decode(&second[SMB2_HEADER_BYTES..]).expect("second body");
            assert!(second_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            assert_eq!(second_body.previous_session_id, 0);
            let authenticate = decode_neg_token_resp(&second_body.security_buffer)
                .expect("authenticate spnego")
                .response_token
                .expect("authenticate token");
            assert!(authenticate.starts_with(b"NTLMSSP\0"));

            let flags = NtlmsspChallenge::decode(&challenge).unwrap().flags;
            let server_mic = mech_list_mic(
                &expected_session_base_key(),
                flags,
                true,
                &ntlm_mech_type_list(),
            )
            .unwrap();
            let mic_spnego = encode_neg_token_resp(
                Some(SpnegoNegState::AcceptIncomplete),
                None,
                None,
                Some(&server_mic),
            )
            .unwrap();
            let mut mic_header =
                Smb2Header::new(Smb2Command::SessionSetup, second_header.message_id);
            mic_header.session_id = session_id;
            mic_header.status_or_channel_sequence = SmbNtStatus::MORE_PROCESSING_REQUIRED.0;
            let mic_response = response_packet(
                mic_header,
                &[session_setup_response_body(&mic_spnego), mic_spnego].concat(),
            );
            // Samba sends this leg unsigned, even after successful NTLM AUTHENTICATE.
            transport.send_packet(&mic_response).await.unwrap();
            let third = transport
                .recv_packet()
                .await
                .expect("client MIC continuation");
            let third_header = Smb2Header::decode(&third).unwrap();
            assert_eq!(third_header.flags & SMB2_FLAGS_SIGNED, 0);
            assert_eq!(third_header.session_id, session_id);
            let third_body = Smb2SessionSetupRequest::decode(&third[SMB2_HEADER_BYTES..]).unwrap();
            assert!(third_body
                .security_mode
                .contains(Smb2SessionSetupSecurityMode::SIGNING_REQUIRED));
            let client_token = decode_neg_token_resp(&third_body.security_buffer).unwrap();
            assert!(client_token.response_token.is_none());
            assert_eq!(
                client_token.mech_list_mic.as_deref(),
                Some(
                    mech_list_mic(
                        &expected_session_base_key(),
                        flags,
                        false,
                        &ntlm_mech_type_list()
                    )
                    .unwrap()
                    .as_slice()
                )
            );

            let mut final_header =
                Smb2Header::new(Smb2Command::SessionSetup, third_header.message_id);
            final_header.status_or_channel_sequence = SmbNtStatus::SUCCESS.0;
            final_header.session_id = session_id;
            let preauth_hash = smb311_preauth_hash_sequence([
                first.as_slice(),
                challenge_response.as_slice(),
                second.as_slice(),
                mic_response.as_slice(),
                third.as_slice(),
            ]);
            final_header.flags |= SMB2_FLAGS_SIGNED;
            let completed =
                encode_neg_token_resp(Some(SpnegoNegState::AcceptCompleted), None, None, None)
                    .unwrap();
            let mut final_response = response_packet(
                final_header,
                &[session_setup_response_body(&completed), completed].concat(),
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
