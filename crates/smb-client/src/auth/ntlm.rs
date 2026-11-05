use crate::wire::encode_utf16le;
use crate::SmbClientError;
use hmac::{Hmac, KeyInit, Mac};
use md5::Md5;

type HmacMd5 = Hmac<Md5>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtlmV2Response {
    pub nt_challenge_response: Vec<u8>,
    pub lm_challenge_response: [u8; 24],
    pub session_base_key: [u8; 16],
}

pub fn nt_hash(password: &str) -> [u8; 16] {
    md4(&encode_utf16le(password))
}

pub fn ntlm_v2_hash(user: &str, target: &str, password: &str) -> Result<[u8; 16], SmbClientError> {
    let nt_hash = nt_hash(password);
    let mut identity = encode_utf16le(&user.to_uppercase());
    identity.extend_from_slice(&encode_utf16le(target));
    hmac_md5(&nt_hash, &identity)
}

pub fn lm_v2_response(
    response_key_nt: &[u8; 16],
    server_challenge: [u8; 8],
    client_challenge: [u8; 8],
) -> Result<[u8; 24], SmbClientError> {
    let mut input = Vec::with_capacity(16);
    input.extend_from_slice(&server_challenge);
    input.extend_from_slice(&client_challenge);
    let mac = hmac_md5(response_key_nt, &input)?;

    let mut response = [0; 24];
    response[..16].copy_from_slice(&mac);
    response[16..].copy_from_slice(&client_challenge);
    Ok(response)
}

pub fn ntlm_v2_temp(timestamp: u64, client_challenge: [u8; 8], target_info: &[u8]) -> Vec<u8> {
    let mut temp = Vec::with_capacity(32 + target_info.len());
    temp.push(0x01);
    temp.push(0x01);
    temp.extend_from_slice(&[0; 6]);
    temp.extend_from_slice(&timestamp.to_le_bytes());
    temp.extend_from_slice(&client_challenge);
    temp.extend_from_slice(&[0; 4]);
    temp.extend_from_slice(target_info);
    temp.extend_from_slice(&[0; 4]);
    temp
}

pub fn ntlm_v2_response(
    response_key_nt: &[u8; 16],
    server_challenge: [u8; 8],
    client_challenge: [u8; 8],
    timestamp: u64,
    target_info: &[u8],
) -> Result<NtlmV2Response, SmbClientError> {
    let temp = ntlm_v2_temp(timestamp, client_challenge, target_info);
    let nt_proof = nt_proof_str(response_key_nt, server_challenge, &temp)?;
    let session_base_key = session_base_key(response_key_nt, nt_proof)?;
    let lm_challenge_response =
        lm_v2_response(response_key_nt, server_challenge, client_challenge)?;

    let mut nt_challenge_response = Vec::with_capacity(16 + temp.len());
    nt_challenge_response.extend_from_slice(&nt_proof);
    nt_challenge_response.extend_from_slice(&temp);

    Ok(NtlmV2Response {
        nt_challenge_response,
        lm_challenge_response,
        session_base_key,
    })
}

pub fn session_base_key(
    response_key_nt: &[u8; 16],
    nt_proof_str: [u8; 16],
) -> Result<[u8; 16], SmbClientError> {
    hmac_md5(response_key_nt, &nt_proof_str)
}

fn nt_proof_str(
    response_key_nt: &[u8; 16],
    server_challenge: [u8; 8],
    temp: &[u8],
) -> Result<[u8; 16], SmbClientError> {
    let mut input = Vec::with_capacity(8 + temp.len());
    input.extend_from_slice(&server_challenge);
    input.extend_from_slice(temp);
    hmac_md5(response_key_nt, &input)
}

fn hmac_md5(key: &[u8], data: &[u8]) -> Result<[u8; 16], SmbClientError> {
    let mut mac = HmacMd5::new_from_slice(key).map_err(|_| SmbClientError::InvalidHmacKey)?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().into())
}

fn md4(input: &[u8]) -> [u8; 16] {
    let mut message = Vec::with_capacity(input.len() + 72);
    message.extend_from_slice(input);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut a = 0x6745_2301u32;
    let mut b = 0xefcd_ab89u32;
    let mut c = 0x98ba_dcfeu32;
    let mut d = 0x1032_5476u32;

    for block in message.chunks_exact(64) {
        let mut x = [0; 16];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            x[index] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }

        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        macro_rules! r1 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $k:expr, $s:expr) => {
                $a = $a
                    .wrapping_add(md4_f($b, $c, $d))
                    .wrapping_add(x[$k])
                    .rotate_left($s);
            };
        }
        macro_rules! r2 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $k:expr, $s:expr) => {
                $a = $a
                    .wrapping_add(md4_g($b, $c, $d))
                    .wrapping_add(x[$k])
                    .wrapping_add(0x5a82_7999)
                    .rotate_left($s);
            };
        }
        macro_rules! r3 {
            ($a:ident, $b:ident, $c:ident, $d:ident, $k:expr, $s:expr) => {
                $a = $a
                    .wrapping_add(md4_h($b, $c, $d))
                    .wrapping_add(x[$k])
                    .wrapping_add(0x6ed9_eba1)
                    .rotate_left($s);
            };
        }

        r1!(a, b, c, d, 0, 3);
        r1!(d, a, b, c, 1, 7);
        r1!(c, d, a, b, 2, 11);
        r1!(b, c, d, a, 3, 19);
        r1!(a, b, c, d, 4, 3);
        r1!(d, a, b, c, 5, 7);
        r1!(c, d, a, b, 6, 11);
        r1!(b, c, d, a, 7, 19);
        r1!(a, b, c, d, 8, 3);
        r1!(d, a, b, c, 9, 7);
        r1!(c, d, a, b, 10, 11);
        r1!(b, c, d, a, 11, 19);
        r1!(a, b, c, d, 12, 3);
        r1!(d, a, b, c, 13, 7);
        r1!(c, d, a, b, 14, 11);
        r1!(b, c, d, a, 15, 19);

        r2!(a, b, c, d, 0, 3);
        r2!(d, a, b, c, 4, 5);
        r2!(c, d, a, b, 8, 9);
        r2!(b, c, d, a, 12, 13);
        r2!(a, b, c, d, 1, 3);
        r2!(d, a, b, c, 5, 5);
        r2!(c, d, a, b, 9, 9);
        r2!(b, c, d, a, 13, 13);
        r2!(a, b, c, d, 2, 3);
        r2!(d, a, b, c, 6, 5);
        r2!(c, d, a, b, 10, 9);
        r2!(b, c, d, a, 14, 13);
        r2!(a, b, c, d, 3, 3);
        r2!(d, a, b, c, 7, 5);
        r2!(c, d, a, b, 11, 9);
        r2!(b, c, d, a, 15, 13);

        r3!(a, b, c, d, 0, 3);
        r3!(d, a, b, c, 8, 9);
        r3!(c, d, a, b, 4, 11);
        r3!(b, c, d, a, 12, 15);
        r3!(a, b, c, d, 2, 3);
        r3!(d, a, b, c, 10, 9);
        r3!(c, d, a, b, 6, 11);
        r3!(b, c, d, a, 14, 15);
        r3!(a, b, c, d, 1, 3);
        r3!(d, a, b, c, 9, 9);
        r3!(c, d, a, b, 5, 11);
        r3!(b, c, d, a, 13, 15);
        r3!(a, b, c, d, 3, 3);
        r3!(d, a, b, c, 11, 9);
        r3!(c, d, a, b, 7, 11);
        r3!(b, c, d, a, 15, 15);

        a = a.wrapping_add(aa);
        b = b.wrapping_add(bb);
        c = c.wrapping_add(cc);
        d = d.wrapping_add(dd);
    }

    let mut digest = [0; 16];
    digest[0..4].copy_from_slice(&a.to_le_bytes());
    digest[4..8].copy_from_slice(&b.to_le_bytes());
    digest[8..12].copy_from_slice(&c.to_le_bytes());
    digest[12..16].copy_from_slice(&d.to_le_bytes());
    digest
}

fn md4_f(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | (!x & z)
}

fn md4_g(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | (x & z) | (y & z)
}

fn md4_h(x: u32, y: u32, z: u32) -> u32 {
    x ^ y ^ z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md4_matches_rfc_1320_vectors() {
        assert_eq!(hex(&md4(b"")), "31d6cfe0d16ae931b73c59d7e0c089c0");
        assert_eq!(hex(&md4(b"a")), "bde52cb31de33e46245e05fbdbd6fb24");
        assert_eq!(hex(&md4(b"abc")), "a448017aaf21d8525fc10ae87aa6729d");
        assert_eq!(
            hex(&md4(b"message digest")),
            "d9130a8164549fe818874806e1c7014b"
        );
    }

    #[test]
    fn nt_hash_matches_known_password_vector() {
        assert_eq!(
            hex(&nt_hash("password")),
            "8846f7eaee8fb117ad06bdd830b7586c"
        );
        assert_eq!(
            hex(&nt_hash("Password")),
            "a4f49c406510bdcab6824ee7c30fd852"
        );
    }

    #[test]
    fn ntlm_v2_hash_matches_known_user_domain_vector() {
        let hash = ntlm_v2_hash("User", "Domain", "Password").expect("hash");

        assert_eq!(hex(&hash), "0c868a403bfd7a93a3001ef22ef02e3f");
    }

    #[test]
    fn lm_v2_response_matches_known_challenge_vector() {
        let response_key_nt = ntlm_v2_hash("User", "Domain", "Password").expect("hash");
        let response = lm_v2_response(
            &response_key_nt,
            hex_array_8("0123456789abcdef"),
            hex_array_8("ffffff0011223344"),
        )
        .expect("response");

        assert_eq!(
            hex(&response),
            "683b04f879b28ebe848df8e4f9f2dc4cffffff0011223344"
        );
    }

    #[test]
    fn ntlm_v2_temp_has_expected_wire_layout() {
        let temp = ntlm_v2_temp(
            0x0123_4567_89ab_cdef,
            hex_array_8("ffffff0011223344"),
            b"target-info",
        );

        assert_eq!(
            hex(&temp),
            "0101000000000000efcdab8967452301ffffff001122334400000000\
             7461726765742d696e666f00000000"
        );
    }

    #[test]
    fn ntlm_v2_response_uses_proof_temp_and_session_key() {
        let response_key_nt = ntlm_v2_hash("User", "Domain", "Password").expect("hash");
        let response = ntlm_v2_response(
            &response_key_nt,
            hex_array_8("0123456789abcdef"),
            hex_array_8("ffffff0011223344"),
            0x0123_4567_89ab_cdef,
            b"target-info",
        )
        .expect("response");

        let temp = ntlm_v2_temp(
            0x0123_4567_89ab_cdef,
            hex_array_8("ffffff0011223344"),
            b"target-info",
        );
        let proof = &response.nt_challenge_response[..16];
        assert_eq!(&response.nt_challenge_response[16..], temp);
        assert_eq!(
            response.session_base_key,
            session_base_key(&response_key_nt, proof.try_into().expect("proof")).expect("key")
        );
        assert_eq!(
            hex(&response.lm_challenge_response),
            "683b04f879b28ebe848df8e4f9f2dc4cffffff0011223344"
        );
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

    fn hex_array_8(hex: &str) -> [u8; 8] {
        let bytes = hex_bytes(hex);
        bytes.try_into().expect("hex should decode to eight bytes")
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        assert!(hex.len().is_multiple_of(2));
        let mut bytes = Vec::with_capacity(hex.len() / 2);
        let raw = hex.as_bytes();
        for index in (0..raw.len()).step_by(2) {
            let high = hex_value(raw[index]);
            let low = hex_value(raw[index + 1]);
            bytes.push((high << 4) | low);
        }
        bytes
    }

    fn hex_value(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => panic!("invalid hex digit"),
        }
    }
}
