use crate::SmbClientError;

const DIRECT_TCP_HEADER_BYTES: usize = 4;
const DIRECT_TCP_SESSION_MESSAGE: u8 = 0x00;
pub const DIRECT_TCP_MAX_PAYLOAD_BYTES: usize = 0x00ff_ffff;

pub fn encode_direct_tcp_header(payload_len: usize) -> Result<[u8; 4], SmbClientError> {
    if payload_len > DIRECT_TCP_MAX_PAYLOAD_BYTES {
        return Err(SmbClientError::DirectTcpPayloadTooLarge {
            length: payload_len,
        });
    }

    Ok([
        DIRECT_TCP_SESSION_MESSAGE,
        ((payload_len >> 16) & 0xff) as u8,
        ((payload_len >> 8) & 0xff) as u8,
        (payload_len & 0xff) as u8,
    ])
}

pub fn decode_direct_tcp_header(header: &[u8]) -> Result<usize, SmbClientError> {
    if header.len() < DIRECT_TCP_HEADER_BYTES {
        return Err(SmbClientError::ShortFrame {
            expected: DIRECT_TCP_HEADER_BYTES,
            actual: header.len(),
        });
    }
    if header[0] != DIRECT_TCP_SESSION_MESSAGE {
        return Err(SmbClientError::InvalidDirectTcpMessageType {
            message_type: header[0],
        });
    }

    Ok(((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_tcp_header_round_trips_payload_length() {
        let encoded = encode_direct_tcp_header(0x12_3456).expect("encode");

        assert_eq!(encoded, [0x00, 0x12, 0x34, 0x56]);
        assert_eq!(decode_direct_tcp_header(&encoded), Ok(0x12_3456));
    }

    #[test]
    fn direct_tcp_header_rejects_non_session_message() {
        let error = decode_direct_tcp_header(&[0x81, 0x00, 0x00, 0x00]).expect_err("reject");

        assert_eq!(
            error,
            SmbClientError::InvalidDirectTcpMessageType { message_type: 0x81 }
        );
    }

    #[test]
    fn direct_tcp_header_rejects_oversized_payload() {
        let error = encode_direct_tcp_header(DIRECT_TCP_MAX_PAYLOAD_BYTES + 1).expect_err("reject");

        assert_eq!(
            error,
            SmbClientError::DirectTcpPayloadTooLarge {
                length: DIRECT_TCP_MAX_PAYLOAD_BYTES + 1
            }
        );
    }
}
