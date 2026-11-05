use crate::packet::create::Smb2FileId;
use crate::packet::header::SMB2_HEADER_BYTES;
use crate::wire::{checked_slice, le_u16, le_u32, le_u64};
use crate::SmbClientError;

const IOCTL_REQUEST_STRUCTURE_SIZE: u16 = 57;
const IOCTL_RESPONSE_STRUCTURE_SIZE: u16 = 49;
const IOCTL_REQUEST_FIXED_BYTES: usize = 56;
const IOCTL_RESPONSE_FIXED_BYTES: usize = 48;
const U32_MAX_USIZE: usize = u32::MAX as usize;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Smb2IoctlFlags(pub u32);

impl Smb2IoctlFlags {
    pub const IS_FSCTL: Self = Self(0x0000_0001);

    pub fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2IoctlRequest {
    pub ctl_code: u32,
    pub file_id: Smb2FileId,
    pub input: Vec<u8>,
    pub max_input_response: u32,
    pub output: Vec<u8>,
    pub max_output_response: u32,
    pub flags: Smb2IoctlFlags,
}

impl Smb2IoctlRequest {
    pub fn encode(&self) -> Result<Vec<u8>, SmbClientError> {
        let input_count = checked_u32_len("ioctl.input", self.input.len())?;
        let output_count = checked_u32_len("ioctl.output", self.output.len())?;
        let input_offset = if self.input.is_empty() {
            0
        } else {
            SMB2_HEADER_BYTES + IOCTL_REQUEST_FIXED_BYTES
        };
        let output_offset = if self.output.is_empty() {
            0
        } else {
            input_offset + self.input.len()
        };
        let input_offset = checked_u32_len("ioctl.input_offset", input_offset)?;
        let output_offset = checked_u32_len("ioctl.output_offset", output_offset)?;
        let mut bytes =
            Vec::with_capacity(IOCTL_REQUEST_FIXED_BYTES + self.input.len() + self.output.len());

        push_u16(&mut bytes, IOCTL_REQUEST_STRUCTURE_SIZE);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, self.ctl_code);
        push_u64(&mut bytes, self.file_id.persistent);
        push_u64(&mut bytes, self.file_id.volatile);
        push_u32(&mut bytes, input_offset);
        push_u32(&mut bytes, input_count);
        push_u32(&mut bytes, self.max_input_response);
        push_u32(&mut bytes, output_offset);
        push_u32(&mut bytes, output_count);
        push_u32(&mut bytes, self.max_output_response);
        push_u32(&mut bytes, self.flags.0);
        push_u32(&mut bytes, 0);
        bytes.extend_from_slice(&self.input);
        bytes.extend_from_slice(&self.output);
        Ok(bytes)
    }

    pub fn decode(body: &[u8]) -> Result<Self, SmbClientError> {
        if body.len() < IOCTL_REQUEST_FIXED_BYTES {
            return Err(SmbClientError::ShortFrame {
                expected: IOCTL_REQUEST_FIXED_BYTES,
                actual: body.len(),
            });
        }
        let structure_size = le_u16(body, 0);
        if structure_size != IOCTL_REQUEST_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        let input = ioctl_buffer(
            body,
            le_u32(body, 24) as usize,
            le_u32(body, 28) as usize,
            IOCTL_REQUEST_FIXED_BYTES,
        )?;
        let output = ioctl_buffer(
            body,
            le_u32(body, 36) as usize,
            le_u32(body, 40) as usize,
            IOCTL_REQUEST_FIXED_BYTES,
        )?;

        Ok(Self {
            ctl_code: le_u32(body, 4),
            file_id: Smb2FileId {
                persistent: le_u64(body, 8),
                volatile: le_u64(body, 16),
            },
            input,
            max_input_response: le_u32(body, 32),
            output,
            max_output_response: le_u32(body, 44),
            flags: Smb2IoctlFlags(le_u32(body, 48)),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Smb2IoctlResponse {
    pub ctl_code: u32,
    pub file_id: Smb2FileId,
    pub input: Vec<u8>,
    pub output: Vec<u8>,
    pub flags: Smb2IoctlFlags,
}

impl Smb2IoctlResponse {
    pub fn decode_packet(packet: &[u8]) -> Result<Self, SmbClientError> {
        let body_offset = SMB2_HEADER_BYTES;
        let required_len = body_offset + IOCTL_RESPONSE_FIXED_BYTES;
        if packet.len() < required_len {
            return Err(SmbClientError::ShortFrame {
                expected: required_len,
                actual: packet.len(),
            });
        }
        let body = &packet[body_offset..];
        let structure_size = le_u16(body, 0);
        if structure_size != IOCTL_RESPONSE_STRUCTURE_SIZE {
            return Err(SmbClientError::InvalidStructureSize {
                size: structure_size,
            });
        }

        let input = ioctl_buffer(
            body,
            le_u32(body, 24) as usize,
            le_u32(body, 28) as usize,
            IOCTL_RESPONSE_FIXED_BYTES,
        )?;
        let output = ioctl_buffer(
            body,
            le_u32(body, 32) as usize,
            le_u32(body, 36) as usize,
            IOCTL_RESPONSE_FIXED_BYTES,
        )?;

        Ok(Self {
            ctl_code: le_u32(body, 4),
            file_id: Smb2FileId {
                persistent: le_u64(body, 8),
                volatile: le_u64(body, 16),
            },
            input,
            output,
            flags: Smb2IoctlFlags(le_u32(body, 40)),
        })
    }
}

pub fn smb2_ioctl_response_body(
    ctl_code: u32,
    file_id: Smb2FileId,
    output: &[u8],
    flags: Smb2IoctlFlags,
) -> Result<Vec<u8>, SmbClientError> {
    let output_count = checked_u32_len("ioctl.response.output", output.len())?;
    let output_offset = if output.is_empty() {
        0
    } else {
        checked_u32_len(
            "ioctl.response.output_offset",
            SMB2_HEADER_BYTES + IOCTL_RESPONSE_FIXED_BYTES,
        )?
    };
    let mut bytes = Vec::with_capacity(IOCTL_RESPONSE_FIXED_BYTES + output.len());
    push_u16(&mut bytes, IOCTL_RESPONSE_STRUCTURE_SIZE);
    push_u16(&mut bytes, 0);
    push_u32(&mut bytes, ctl_code);
    push_u64(&mut bytes, file_id.persistent);
    push_u64(&mut bytes, file_id.volatile);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, output_offset);
    push_u32(&mut bytes, output_count);
    push_u32(&mut bytes, flags.0);
    push_u32(&mut bytes, 0);
    bytes.extend_from_slice(output);
    Ok(bytes)
}

fn ioctl_buffer(
    body: &[u8],
    absolute_offset: usize,
    length: usize,
    fixed_bytes: usize,
) -> Result<Vec<u8>, SmbClientError> {
    if length == 0 {
        return Ok(Vec::new());
    }
    let min_offset = SMB2_HEADER_BYTES + fixed_bytes;
    if absolute_offset < min_offset {
        return Err(SmbClientError::BufferRangeOutOfBounds {
            offset: absolute_offset,
            length,
            available: SMB2_HEADER_BYTES + body.len(),
        });
    }
    checked_slice(body, absolute_offset - SMB2_HEADER_BYTES, length).map(Vec::from)
}

fn checked_u32_len(field: &'static str, length: usize) -> Result<u32, SmbClientError> {
    u32::try_from(length).map_err(|_| SmbClientError::FieldTooLarge {
        field,
        length,
        max: U32_MAX_USIZE,
    })
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::{Smb2Command, Smb2Header};

    #[test]
    fn ioctl_request_round_trips_input_buffer() {
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let request = Smb2IoctlRequest {
            ctl_code: 0x0014_40f2,
            file_id,
            input: b"copychunk".to_vec(),
            max_input_response: 4,
            output: Vec::new(),
            max_output_response: 12,
            flags: Smb2IoctlFlags::IS_FSCTL,
        };

        let encoded = request.encode().expect("encode");
        let decoded = Smb2IoctlRequest::decode(&encoded).expect("decode");

        assert_eq!(decoded, request);
        assert_eq!(le_u16(&encoded, 0), IOCTL_REQUEST_STRUCTURE_SIZE);
        assert_eq!(
            le_u32(&encoded, 24),
            (SMB2_HEADER_BYTES + IOCTL_REQUEST_FIXED_BYTES) as u32
        );
        assert_eq!(le_u32(&encoded, 28), b"copychunk".len() as u32);
        assert_eq!(le_u32(&encoded, 44), 12);
        assert!(Smb2IoctlFlags(le_u32(&encoded, 48)).contains(Smb2IoctlFlags::IS_FSCTL));
    }

    #[test]
    fn ioctl_request_rejects_input_inside_fixed_body() {
        let mut encoded = empty_ioctl_request().encode().expect("encode");
        encoded[24..28].copy_from_slice(&(SMB2_HEADER_BYTES as u32 + 1).to_le_bytes());
        encoded[28..32].copy_from_slice(&4u32.to_le_bytes());

        assert_eq!(
            Smb2IoctlRequest::decode(&encoded),
            Err(SmbClientError::BufferRangeOutOfBounds {
                offset: SMB2_HEADER_BYTES + 1,
                length: 4,
                available: SMB2_HEADER_BYTES + encoded.len()
            })
        );
    }

    #[test]
    fn ioctl_response_decodes_output_buffer() {
        let file_id = Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        };
        let mut packet = Smb2Header::new(Smb2Command::Ioctl, 12).encode().to_vec();
        packet.extend_from_slice(
            &smb2_ioctl_response_body(0x0014_0078, file_id, b"resume", Smb2IoctlFlags::IS_FSCTL)
                .expect("response body"),
        );

        let response = Smb2IoctlResponse::decode_packet(&packet).expect("decode");

        assert_eq!(response.ctl_code, 0x0014_0078);
        assert_eq!(response.file_id, file_id);
        assert_eq!(response.input, Vec::<u8>::new());
        assert_eq!(response.output, b"resume");
        assert!(response.flags.contains(Smb2IoctlFlags::IS_FSCTL));
    }

    #[test]
    fn ioctl_response_rejects_bad_structure_size() {
        let mut packet = Smb2Header::new(Smb2Command::Ioctl, 12).encode().to_vec();
        packet.extend_from_slice(&48u16.to_le_bytes());
        packet.resize(SMB2_HEADER_BYTES + IOCTL_RESPONSE_FIXED_BYTES, 0);

        assert_eq!(
            Smb2IoctlResponse::decode_packet(&packet),
            Err(SmbClientError::InvalidStructureSize { size: 48 })
        );
    }

    fn empty_ioctl_request() -> Smb2IoctlRequest {
        Smb2IoctlRequest {
            ctl_code: 0x0014_0078,
            file_id: Smb2FileId {
                persistent: 1,
                volatile: 2,
            },
            input: Vec::new(),
            max_input_response: 0,
            output: Vec::new(),
            max_output_response: 32,
            flags: Smb2IoctlFlags::IS_FSCTL,
        }
    }
}
