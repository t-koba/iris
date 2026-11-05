//! Native SMB2/3 client protocol foundations.
//!
//! This crate keeps wire encoding/decoding isolated from backend policy so the
//! protocol layer can be fuzzed and tested independently.

pub mod actor;
pub mod auth;
pub mod connection;
pub mod copychunk;
pub mod crypto;
pub mod dir;
pub mod file;
pub mod netbios;
pub mod packet;
pub mod session;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod transform;
pub mod transport;
pub mod tree;
mod wire;

pub use actor::{
    decode_compound_responses, SmbCompoundRequest, SmbConnectionActor, SmbConnectionActorState,
};
pub use auth::ntlm::{
    lm_v2_response, nt_hash, ntlm_v2_hash, ntlm_v2_response, ntlm_v2_temp, session_base_key,
    NtlmV2Response,
};
pub use auth::ntlmssp::{
    default_negotiate_flags, ntlm_v2_authenticate_message, NtlmV2AuthenticateRequest,
    NtlmsspAuthenticate, NtlmsspAuthenticateResult, NtlmsspChallenge, NtlmsspNegotiate,
    NtlmsspNegotiateFlags,
};
pub use auth::spnego::{
    decode_neg_token_resp, encode_neg_token_resp, spnego_initial_context_token_ntlm,
    spnego_neg_token_resp_ntlm, SpnegoNegState, SpnegoNegTokenResp,
};
pub use connection::{SmbConnectionError, SmbResponse};
pub use copychunk::{
    copy_chunks, copy_file_chunks, decode_copychunk_response, decode_resume_key_response,
    request_resume_key, smb2_srv_copychunk_copy, SmbCopyChunk, SmbCopyChunkResponse,
    DEFAULT_COPYCHUNK_CHUNK_BYTES, DEFAULT_COPYCHUNK_MAX_CHUNKS, FSCTL_SRV_COPYCHUNK,
    FSCTL_SRV_REQUEST_RESUME_KEY, SMB2_RESUME_KEY_BYTES,
};
pub use crypto::{
    derive_smb2_signing_key, sign_smb2_packet_aes_cmac, sign_smb2_packet_hmac_sha256,
    smb2_aes_cmac_signature, smb2_hmac_sha256_signature, smb311_initial_preauth_hash,
    smb311_preauth_hash_sequence, smb311_update_preauth_hash, sp800_108_counter_hmac_sha256,
    verify_smb2_packet_aes_cmac, verify_smb2_packet_hmac_sha256, SmbSigningKey,
    SMB311_PREAUTH_HASH_BYTES,
};
pub use dir::{
    create_directory, decode_file_id_both_directory_entries, list_directory_once,
    SmbDirectoryEntry, DEFAULT_DIRECTORY_OUTPUT_BUFFER_LENGTH,
};
pub use file::{
    close, delete, open_file_and_read, open_file_for_read, open_file_for_write,
    read_file_ranges_pipelined, rename, set_basic_info, write_file_at, write_file_chunks_pipelined,
    SmbHandle, SmbReadRange, DEFAULT_READ_CHUNK_LENGTH, DEFAULT_READ_PIPELINE_DEPTH,
    DEFAULT_WRITE_CHUNK_LENGTH,
};
pub use netbios::{decode_direct_tcp_header, encode_direct_tcp_header};
pub use packet::close::{Smb2CloseFlags, Smb2CloseRequest, Smb2CloseResponse};
pub use packet::create::{
    Smb2CreateAction, Smb2CreateDisposition, Smb2CreateOptions, Smb2CreateRequest,
    Smb2CreateResponse, Smb2DesiredAccess, Smb2FileAttributes, Smb2FileId, Smb2ImpersonationLevel,
    Smb2OplockLevel, Smb2ShareAccess,
};
pub use packet::header::{
    smb2_credit_charge_for_payload, Smb2Command, Smb2Header, SmbNtStatus, SMB2_CREDIT_UNIT_BYTES,
    SMB2_FLAGS_SIGNED,
};
pub use packet::ioctl::{
    smb2_ioctl_response_body, Smb2IoctlFlags, Smb2IoctlRequest, Smb2IoctlResponse,
};
pub use packet::negotiate::{
    Smb2Dialect, Smb2EncryptionCipher, Smb2NegotiateContext, Smb2NegotiateRequest,
    Smb2NegotiateResponse, Smb2SecurityMode,
};
pub use packet::query_directory::{
    Smb2FileInformationClass, Smb2QueryDirectoryFlags, Smb2QueryDirectoryRequest,
    Smb2QueryDirectoryResponse,
};
pub use packet::read::{Smb2ReadChannel, Smb2ReadFlags, Smb2ReadRequest, Smb2ReadResponse};
pub use packet::session_setup::{
    Smb2SessionSetupRequest, Smb2SessionSetupResponse, Smb2SessionSetupSecurityMode,
};
pub use packet::set_info::{
    smb2_file_basic_information, smb2_file_disposition_information, smb2_file_rename_information,
    Smb2FileBasicInformation, Smb2FileInfoClass, Smb2InfoType, Smb2SetInfoRequest,
    Smb2SetInfoResponse,
};
pub use packet::tree_connect::{
    Smb2ShareCapabilities, Smb2ShareFlags, Smb2ShareType, Smb2TreeConnectFlags,
    Smb2TreeConnectRequest, Smb2TreeConnectResponse,
};
pub use packet::write::{Smb2WriteChannel, Smb2WriteFlags, Smb2WriteRequest, Smb2WriteResponse};
pub use session::{
    authenticate_guest, authenticate_ntlm_v2, NtlmV2SecurityConfig, NtlmV2Session,
    NtlmV2SessionSetup, SmbSessionError,
};
pub use transform::{
    derive_smb311_encryption_keys, derive_smb3_encryption_keys, Smb3EncryptionContext,
    Smb3EncryptionKeys, SMB3_TRANSFORM_HEADER_BYTES,
};
pub use transport::{DirectTcpTransport, SmbTransportError};
pub use tree::{tree_connect_share, tree_connect_unc, unc_share_path, SmbTree, SmbTreeError};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SmbClientError {
    #[error("frame is too short: expected at least {expected} bytes, got {actual}")]
    ShortFrame { expected: usize, actual: usize },

    #[error("invalid direct TCP message type: 0x{message_type:02x}")]
    InvalidDirectTcpMessageType { message_type: u8 },

    #[error("direct TCP payload is too large: {length} bytes")]
    DirectTcpPayloadTooLarge { length: usize },

    #[error("SMB2 signing key is empty")]
    EmptySigningKey,

    #[error("SMB KDF key is empty")]
    EmptyKdfKey,

    #[error("invalid SMB KDF output length: {length}")]
    InvalidKdfOutputLength { length: usize },

    #[error("SMB encryption requires dialect 3.1.1")]
    UnsupportedEncryptionDialect,

    #[error("unsupported SMB encryption cipher: 0x{cipher:04x}")]
    UnsupportedEncryptionCipher { cipher: u16 },

    #[error("invalid SMB encryption key length")]
    InvalidEncryptionKey,

    #[error("SMB encryption nonce generation failed")]
    EncryptionNonceGenerationFailed,

    #[error("SMB encryption nonce space exhausted")]
    EncryptionNonceExhausted,

    #[error("invalid SMB3 transform protocol id")]
    InvalidTransformProtocolId,

    #[error("invalid SMB3 transform flags: 0x{flags:04x}")]
    InvalidTransformFlags { flags: u16 },

    #[error("SMB3 transform reserved field is not zero")]
    InvalidTransformReserved,

    #[error("SMB3 transform session mismatch: expected {expected}, got {actual}")]
    TransformSessionMismatch { expected: u64, actual: u64 },

    #[error("SMB3 transform size mismatch: declared {declared}, actual {actual}")]
    TransformSizeMismatch { declared: usize, actual: usize },

    #[error("SMB3 transform authentication failed")]
    TransformAuthenticationFailed,

    #[error("expected an encrypted SMB3 transform response")]
    ExpectedEncryptedTransform,

    #[error("invalid HMAC key")]
    InvalidHmacKey,

    #[error("invalid NTLMSSP signature")]
    InvalidNtlmsspSignature,

    #[error("unknown NTLMSSP message type: {message_type}")]
    UnknownNtlmsspMessageType { message_type: u32 },

    #[error("invalid ASN.1 DER tag: expected 0x{expected:02x}, got 0x{actual:02x}")]
    InvalidAsn1Tag { expected: u8, actual: u8 },

    #[error("invalid ASN.1 DER length")]
    InvalidAsn1Length,

    #[error("invalid ASN.1 DER value: {kind}")]
    InvalidAsn1Value { kind: &'static str },

    #[error("unknown SPNEGO negotiation state: {state}")]
    UnknownSpnegoNegState { state: u32 },

    #[error("invalid SMB2 protocol id")]
    InvalidProtocolId,

    #[error("SMB2 compound request must contain at least one operation")]
    EmptyCompoundRequest,

    #[error("SMB2 compound request mixes related and unrelated operation styles")]
    MixedCompoundRequestStyles,

    #[error("invalid SMB2 compound next-command offset {offset} with {remaining} bytes remaining")]
    InvalidCompoundNextCommand { offset: usize, remaining: usize },

    #[error("invalid SMB2 structure size: {size}")]
    InvalidStructureSize { size: u16 },

    #[error("unknown SMB2 command: 0x{command:04x}")]
    UnknownCommand { command: u16 },

    #[error("unknown SMB2 dialect: 0x{dialect:04x}")]
    UnknownDialect { dialect: u16 },

    #[error("invalid SMB3 negotiate context: {reason}")]
    InvalidNegotiateContext { reason: &'static str },

    #[error("unknown SMB2 share type: 0x{share_type:02x}")]
    UnknownShareType { share_type: u8 },

    #[error("unknown SMB2 oplock level: 0x{level:02x}")]
    UnknownOplockLevel { level: u8 },

    #[error("unknown SMB2 create action: 0x{action:08x}")]
    UnknownCreateAction { action: u32 },

    #[error("unknown SMB2 impersonation level: 0x{level:08x}")]
    UnknownImpersonationLevel { level: u32 },

    #[error("unknown SMB2 create disposition: 0x{disposition:08x}")]
    UnknownCreateDisposition { disposition: u32 },

    #[error("unknown SMB2 info type: 0x{info_type:02x}")]
    UnknownInfoType { info_type: u8 },

    #[error("SMB2 field {field} is too large: {length} bytes, max {max}")]
    FieldTooLarge {
        field: &'static str,
        length: usize,
        max: usize,
    },

    #[error("SMB2 UTF-16LE field has odd byte length: {length}")]
    InvalidUtf16Length { length: usize },

    #[error("SMB2 path is not valid UTF-16")]
    InvalidUtf16Path,

    #[error(
        "buffer range is outside packet: offset {offset}, length {length}, available {available}"
    )]
    BufferRangeOutOfBounds {
        offset: usize,
        length: usize,
        available: usize,
    },
}
