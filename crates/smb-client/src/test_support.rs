//! Wire-format builders for tests of this crate and of the SMB backend.

use crate::packet::close::Smb2CloseFlags;
use crate::packet::create::{Smb2CreateAction, Smb2FileAttributes, Smb2FileId};
use crate::packet::header::{Smb2Header, SMB2_HEADER_BYTES};
use crate::packet::negotiate::{Smb2Dialect, Smb2SecurityMode};
use crate::packet::tree_connect::{Smb2ShareCapabilities, Smb2ShareFlags, Smb2ShareType};
pub use crate::wire::{encode_utf16le, le_u16, le_u32, le_u64, put_u16, put_u32, put_u64};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn hex_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
        .collect()
}

pub fn hex_array_8(hex: &str) -> [u8; 8] {
    hex_bytes(hex).try_into().expect("eight bytes of hex")
}

pub fn response_packet(header: Smb2Header, body: &[u8]) -> Vec<u8> {
    [header.encode().as_slice(), body].concat()
}

/// Responses chained with NextCommand; every segment but the last is padded to 8 bytes.
pub fn compound_response_packet(parts: Vec<(Smb2Header, Vec<u8>)>) -> Vec<u8> {
    let mut packet = Vec::new();
    let part_count = parts.len();
    for (index, (mut header, body)) in parts.into_iter().enumerate() {
        let is_last = index + 1 == part_count;
        let unpadded_len = SMB2_HEADER_BYTES + body.len();
        let segment_len = if is_last {
            unpadded_len
        } else {
            unpadded_len.div_ceil(8) * 8
        };
        header.next_command = if is_last { 0 } else { segment_len as u32 };
        packet.extend_from_slice(&header.encode());
        packet.extend_from_slice(&body);
        packet.resize(packet.len() + segment_len - unpadded_len, 0);
    }
    packet
}

/// SMB 3.1.1 with signing required and 1 MiB transfer sizes.
pub fn negotiate_response_body() -> Vec<u8> {
    let mut body = vec![0; 65];
    put_u16(&mut body, 0, 65);
    put_u16(
        &mut body,
        2,
        Smb2SecurityMode::SIGNING_ENABLED.0 | Smb2SecurityMode::SIGNING_REQUIRED.0,
    );
    put_u16(&mut body, 4, Smb2Dialect::Smb311.to_wire());
    body[8..24].copy_from_slice(&[0x55; 16]);
    for offset in [28, 32, 36] {
        put_u32(&mut body, offset, 1_048_576);
    }
    let security_offset = (SMB2_HEADER_BYTES + body.len()) as u16;
    put_u16(&mut body, 56, security_offset);
    body
}

pub fn tree_connect_response_body(share_flags: Smb2ShareFlags) -> Vec<u8> {
    let mut body = vec![0; 16];
    put_u16(&mut body, 0, 16);
    body[2] = Smb2ShareType::Disk.to_wire();
    put_u32(&mut body, 4, share_flags.0);
    put_u32(&mut body, 8, Smb2ShareCapabilities::DFS.0);
    put_u32(&mut body, 12, 0x001f_01ff);
    body
}

pub fn create_response_body(
    file_id: Smb2FileId,
    attributes: Smb2FileAttributes,
    end_of_file: u64,
) -> Vec<u8> {
    let mut body = vec![0; 88];
    put_u16(&mut body, 0, 89);
    put_u32(&mut body, 4, Smb2CreateAction::Opened.to_wire());
    put_u64(&mut body, 40, 4096);
    put_u64(&mut body, 48, end_of_file);
    put_u32(&mut body, 56, attributes.0);
    put_u64(&mut body, 64, file_id.persistent);
    put_u64(&mut body, 72, file_id.volatile);
    body
}

pub fn close_response_body(attributes: Smb2FileAttributes, end_of_file: u64) -> Vec<u8> {
    let mut body = vec![0; 60];
    put_u16(&mut body, 0, 60);
    put_u16(&mut body, 2, Smb2CloseFlags::POSTQUERY_ATTRIB.0);
    put_u64(&mut body, 40, 4096);
    put_u64(&mut body, 48, end_of_file);
    put_u32(&mut body, 56, attributes.0);
    body
}

pub fn query_directory_response_body(buffer: &[u8]) -> Vec<u8> {
    let mut body = vec![0; 8];
    put_u16(&mut body, 0, 9);
    put_u16(&mut body, 2, (SMB2_HEADER_BYTES + 8) as u16);
    put_u32(&mut body, 4, buffer.len() as u32);
    [body, buffer.to_vec()].concat()
}

pub fn read_response_body(data: &[u8]) -> Vec<u8> {
    let mut body = vec![0; 16];
    put_u16(&mut body, 0, 17);
    body[2] = (SMB2_HEADER_BYTES + 16) as u8;
    put_u32(&mut body, 4, data.len() as u32);
    [body, data.to_vec()].concat()
}

pub fn write_response_body(count: u32) -> Vec<u8> {
    let mut body = vec![0; 16];
    put_u16(&mut body, 0, 17);
    put_u32(&mut body, 4, count);
    body
}

pub fn set_info_response_body() -> Vec<u8> {
    let mut body = vec![0; 2];
    put_u16(&mut body, 0, 2);
    body
}
