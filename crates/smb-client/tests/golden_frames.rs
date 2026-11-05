use iris_smb_client::{
    smb2_hmac_sha256_signature, Smb2Command, Smb2Dialect, Smb2Header, Smb2NegotiateResponse,
    Smb2QueryDirectoryResponse, Smb2ReadResponse, Smb2SecurityMode, Smb2SessionSetupResponse,
    Smb2WriteResponse,
};

#[test]
fn decodes_fixed_negotiate_response_frame() {
    let packet = decode_hex(include_str!("fixtures/negotiate_response.hex"));
    let response = Smb2NegotiateResponse::decode_packet(&packet).expect("decode fixture");

    assert_eq!(response.dialect, Smb2Dialect::Smb311);
    assert!(response
        .security_mode
        .contains(Smb2SecurityMode::SIGNING_REQUIRED));
    assert_eq!(response.capabilities, 0x40);
    assert_eq!(response.max_transact_size, 1024 * 1024);
    assert_eq!(response.max_read_size, 1024 * 1024);
    assert_eq!(response.max_write_size, 1024 * 1024);
}

#[test]
fn decodes_fixed_session_setup_response_frame() {
    let packet = decode_hex(include_str!("fixtures/session_setup_response.hex"));
    assert_eq!(
        Smb2Header::decode(&packet).expect("header").command,
        Smb2Command::SessionSetup
    );

    let response = Smb2SessionSetupResponse::decode_packet(&packet).expect("decode fixture");

    assert_eq!(response.session_flags, 0x0001);
    assert_eq!(response.security_buffer, [0xa1, 0x02, 0x30, 0x00]);
}

#[test]
fn decodes_fixed_query_directory_response_frame() {
    let packet = decode_hex(include_str!("fixtures/query_directory_response.hex"));
    assert_eq!(
        Smb2Header::decode(&packet).expect("header").command,
        Smb2Command::QueryDirectory
    );

    let response = Smb2QueryDirectoryResponse::decode_packet(&packet).expect("decode fixture");

    assert_eq!(response.output_buffer, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn decodes_fixed_read_response_frame() {
    let packet = decode_hex(include_str!("fixtures/read_response.hex"));
    assert_eq!(
        Smb2Header::decode(&packet).expect("header").command,
        Smb2Command::Read
    );

    let response = Smb2ReadResponse::decode_packet(&packet).expect("decode fixture");

    assert_eq!(response.data_remaining, 7);
    assert_eq!(response.data, b"hello");
    assert_eq!(
        hex(&smb2_hmac_sha256_signature(b"0123456789abcdef", &packet).expect("signature")),
        "4f5a4fb79bcad91052500ddf4bd8c268"
    );
}

#[test]
fn decodes_fixed_write_response_frame() {
    let packet = decode_hex(include_str!("fixtures/write_response.hex"));
    assert_eq!(
        Smb2Header::decode(&packet).expect("header").command,
        Smb2Command::Write
    );

    let response = Smb2WriteResponse::decode_packet(&packet).expect("decode fixture");

    assert_eq!(response.count, 1_048_576);
    assert_eq!(response.remaining, 2);
    assert_eq!(response.channel_info, [1, 2, 3]);
}

fn decode_hex(input: &str) -> Vec<u8> {
    let bytes = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    assert!(bytes.len().is_multiple_of(2));
    bytes
        .chunks_exact(2)
        .map(|pair| (hex_value(pair[0]) << 4) | hex_value(pair[1]))
        .collect()
}

fn hex_value(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => panic!("invalid fixture hex"),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
