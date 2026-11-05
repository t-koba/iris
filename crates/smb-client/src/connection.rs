//! Error and response types shared by the connection actor and the operations built on it.

use crate::packet::header::{Smb2Command, Smb2Header, SmbNtStatus, SMB2_HEADER_BYTES};
use crate::transport::SmbTransportError;
use crate::SmbClientError;

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
