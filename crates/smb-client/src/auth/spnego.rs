use crate::SmbClientError;

const TAG_SEQUENCE: u8 = 0x30;
const TAG_OBJECT_IDENTIFIER: u8 = 0x06;
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_ENUMERATED: u8 = 0x0a;
const TAG_APPLICATION_0: u8 = 0x60;
const TAG_NEG_TOKEN_INIT: u8 = 0xa0;
const TAG_NEG_TOKEN_RESP: u8 = 0xa1;
const TAG_CONTEXT_0: u8 = 0xa0;
const TAG_CONTEXT_1: u8 = 0xa1;
const TAG_CONTEXT_2: u8 = 0xa2;
const TAG_CONTEXT_3: u8 = 0xa3;
const SPNEGO_OID_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
const NTLMSSP_OID_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpnegoNegState {
    AcceptCompleted,
    AcceptIncomplete,
    Reject,
    RequestMic,
}

impl SpnegoNegState {
    fn from_wire(value: u32) -> Result<Self, SmbClientError> {
        match value {
            0 => Ok(Self::AcceptCompleted),
            1 => Ok(Self::AcceptIncomplete),
            2 => Ok(Self::Reject),
            3 => Ok(Self::RequestMic),
            state => Err(SmbClientError::UnknownSpnegoNegState { state }),
        }
    }

    fn to_wire(self) -> u32 {
        match self {
            Self::AcceptCompleted => 0,
            Self::AcceptIncomplete => 1,
            Self::Reject => 2,
            Self::RequestMic => 3,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SpnegoNegTokenResp {
    pub neg_state: Option<SpnegoNegState>,
    pub supported_mech_is_ntlm: bool,
    pub response_token: Option<Vec<u8>>,
    pub mech_list_mic: Option<Vec<u8>>,
}

pub fn spnego_initial_context_token_ntlm(
    ntlm_negotiate_message: &[u8],
) -> Result<Vec<u8>, SmbClientError> {
    let mech_type_list = der_sequence(&[der_object_identifier(NTLMSSP_OID_DER)]);
    let mech_types = der_tag(TAG_CONTEXT_0, &mech_type_list)?;
    let mech_token = der_tag(TAG_CONTEXT_2, &der_octet_string(ntlm_negotiate_message)?)?;
    let neg_token_init = der_sequence(&[mech_types, mech_token]);
    let negotiation_token = der_tag(TAG_NEG_TOKEN_INIT, &neg_token_init)?;
    let inner = join(&[der_object_identifier(SPNEGO_OID_DER), negotiation_token]);
    der_tag(TAG_APPLICATION_0, &inner)
}

pub fn spnego_neg_token_resp_ntlm(response_token: &[u8]) -> Result<Vec<u8>, SmbClientError> {
    encode_neg_token_resp(None, None, Some(response_token), None)
}

pub fn encode_neg_token_resp(
    neg_state: Option<SpnegoNegState>,
    supported_ntlm: Option<bool>,
    response_token: Option<&[u8]>,
    mech_list_mic: Option<&[u8]>,
) -> Result<Vec<u8>, SmbClientError> {
    let mut fields = Vec::new();
    if let Some(neg_state) = neg_state {
        fields.push(der_tag(
            TAG_CONTEXT_0,
            &der_enumerated(neg_state.to_wire())?,
        )?);
    }
    if supported_ntlm.unwrap_or(false) {
        fields.push(der_tag(
            TAG_CONTEXT_1,
            &der_object_identifier(NTLMSSP_OID_DER),
        )?);
    }
    if let Some(response_token) = response_token {
        fields.push(der_tag(TAG_CONTEXT_2, &der_octet_string(response_token)?)?);
    }
    if let Some(mech_list_mic) = mech_list_mic {
        fields.push(der_tag(TAG_CONTEXT_3, &der_octet_string(mech_list_mic)?)?);
    }
    der_tag(TAG_NEG_TOKEN_RESP, &der_sequence(&fields))
}

pub fn decode_neg_token_resp(bytes: &[u8]) -> Result<SpnegoNegTokenResp, SmbClientError> {
    let token = unwrap_initial_context_token(bytes)?;
    let mut outer = DerReader::new(token);
    let choice = outer.read_tlv()?;
    outer.finish()?;
    if choice.tag != TAG_NEG_TOKEN_RESP {
        return Err(SmbClientError::InvalidAsn1Tag {
            expected: TAG_NEG_TOKEN_RESP,
            actual: choice.tag,
        });
    }

    let mut choice_reader = DerReader::new(choice.value);
    let sequence = choice_reader.read_tlv()?;
    choice_reader.finish()?;
    if sequence.tag != TAG_SEQUENCE {
        return Err(SmbClientError::InvalidAsn1Tag {
            expected: TAG_SEQUENCE,
            actual: sequence.tag,
        });
    }

    let mut fields = DerReader::new(sequence.value);
    let mut response = SpnegoNegTokenResp::default();
    while !fields.is_empty() {
        let field = fields.read_tlv()?;
        match field.tag {
            TAG_CONTEXT_0 => {
                let state = read_explicit_enumerated(field.value)?;
                response.neg_state = Some(SpnegoNegState::from_wire(state)?);
            }
            TAG_CONTEXT_1 => {
                let oid = read_explicit_oid(field.value)?;
                response.supported_mech_is_ntlm = oid == NTLMSSP_OID_DER;
            }
            TAG_CONTEXT_2 => {
                response.response_token = Some(read_explicit_octet_string(field.value)?.to_vec());
            }
            TAG_CONTEXT_3 => {
                response.mech_list_mic = Some(read_explicit_octet_string(field.value)?.to_vec());
            }
            _ => {}
        }
    }
    Ok(response)
}

fn unwrap_initial_context_token(bytes: &[u8]) -> Result<&[u8], SmbClientError> {
    if bytes.first().copied() != Some(TAG_APPLICATION_0) {
        return Ok(bytes);
    }

    let mut reader = DerReader::new(bytes);
    let token = reader.read_tlv()?;
    reader.finish()?;
    let mut token_reader = DerReader::new(token.value);
    let oid = token_reader.expect_tlv(TAG_OBJECT_IDENTIFIER)?;
    if oid != SPNEGO_OID_DER {
        return Err(SmbClientError::InvalidAsn1Value { kind: "spnego.oid" });
    }
    let start = token_reader.position();
    let remaining = &token.value[start..];
    if remaining.is_empty() {
        return Err(SmbClientError::InvalidAsn1Length);
    }
    Ok(remaining)
}

fn read_explicit_enumerated(bytes: &[u8]) -> Result<u32, SmbClientError> {
    let mut reader = DerReader::new(bytes);
    let value = reader.expect_tlv(TAG_ENUMERATED)?;
    reader.finish()?;
    der_integer_u32(value)
}

fn read_explicit_oid(bytes: &[u8]) -> Result<&[u8], SmbClientError> {
    let mut reader = DerReader::new(bytes);
    let value = reader.expect_tlv(TAG_OBJECT_IDENTIFIER)?;
    reader.finish()?;
    Ok(value)
}

fn read_explicit_octet_string(bytes: &[u8]) -> Result<&[u8], SmbClientError> {
    let mut reader = DerReader::new(bytes);
    let value = reader.expect_tlv(TAG_OCTET_STRING)?;
    reader.finish()?;
    Ok(value)
}

fn der_sequence(elements: &[Vec<u8>]) -> Vec<u8> {
    let content = join(elements);
    let mut output = Vec::with_capacity(1 + 5 + content.len());
    output.push(TAG_SEQUENCE);
    output.extend_from_slice(&der_length(content.len()));
    output.extend_from_slice(&content);
    output
}

fn der_tag(tag: u8, content: &[u8]) -> Result<Vec<u8>, SmbClientError> {
    let mut output = Vec::with_capacity(1 + 5 + content.len());
    output.push(tag);
    output.extend_from_slice(&der_length_checked(content.len())?);
    output.extend_from_slice(content);
    Ok(output)
}

fn der_object_identifier(oid_content: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(1 + 5 + oid_content.len());
    output.push(TAG_OBJECT_IDENTIFIER);
    output.extend_from_slice(&der_length(oid_content.len()));
    output.extend_from_slice(oid_content);
    output
}

fn der_octet_string(value: &[u8]) -> Result<Vec<u8>, SmbClientError> {
    let mut output = Vec::with_capacity(1 + 5 + value.len());
    output.push(TAG_OCTET_STRING);
    output.extend_from_slice(&der_length_checked(value.len())?);
    output.extend_from_slice(value);
    Ok(output)
}

fn der_enumerated(value: u32) -> Result<Vec<u8>, SmbClientError> {
    let content = der_u32_content(value);
    let mut output = Vec::with_capacity(1 + 5 + content.len());
    output.push(TAG_ENUMERATED);
    output.extend_from_slice(&der_length_checked(content.len())?);
    output.extend_from_slice(&content);
    Ok(output)
}

fn der_integer_u32(bytes: &[u8]) -> Result<u32, SmbClientError> {
    if bytes.is_empty() || bytes.len() > 5 {
        return Err(SmbClientError::InvalidAsn1Value { kind: "enumerated" });
    }
    if bytes[0] & 0x80 != 0 {
        return Err(SmbClientError::InvalidAsn1Value {
            kind: "enumerated-negative",
        });
    }
    let mut value = 0u32;
    for byte in bytes {
        value = value
            .checked_mul(256)
            .and_then(|current| current.checked_add(*byte as u32))
            .ok_or(SmbClientError::InvalidAsn1Value {
                kind: "enumerated-overflow",
            })?;
    }
    Ok(value)
}

fn der_u32_content(value: u32) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first_non_zero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    let content = &bytes[first_non_zero..];
    if content[0] & 0x80 == 0 {
        content.to_vec()
    } else {
        let mut padded = Vec::with_capacity(content.len() + 1);
        padded.push(0);
        padded.extend_from_slice(content);
        padded
    }
}

fn der_length_checked(length: usize) -> Result<Vec<u8>, SmbClientError> {
    if length > u32::MAX as usize {
        return Err(SmbClientError::FieldTooLarge {
            field: "asn1.length",
            length,
            max: u32::MAX as usize,
        });
    }
    Ok(der_length(length))
}

fn der_length(length: usize) -> Vec<u8> {
    if length < 0x80 {
        return vec![length as u8];
    }

    let bytes = (length as u64).to_be_bytes();
    let first_non_zero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    let content = &bytes[first_non_zero..];
    let mut output = Vec::with_capacity(1 + content.len());
    output.push(0x80 | content.len() as u8);
    output.extend_from_slice(content);
    output
}

fn join(elements: &[Vec<u8>]) -> Vec<u8> {
    let len = elements.iter().map(Vec::len).sum();
    let mut output = Vec::with_capacity(len);
    for element in elements {
        output.extend_from_slice(element);
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DerTlv<'a> {
    tag: u8,
    value: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
struct DerReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> DerReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn is_empty(self) -> bool {
        self.position == self.bytes.len()
    }

    fn position(self) -> usize {
        self.position
    }

    fn finish(self) -> Result<(), SmbClientError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(SmbClientError::InvalidAsn1Length)
        }
    }

    fn expect_tlv(&mut self, expected_tag: u8) -> Result<&'a [u8], SmbClientError> {
        let tlv = self.read_tlv()?;
        if tlv.tag != expected_tag {
            return Err(SmbClientError::InvalidAsn1Tag {
                expected: expected_tag,
                actual: tlv.tag,
            });
        }
        Ok(tlv.value)
    }

    fn read_tlv(&mut self) -> Result<DerTlv<'a>, SmbClientError> {
        if self.position >= self.bytes.len() {
            return Err(SmbClientError::InvalidAsn1Length);
        }
        let tag = self.bytes[self.position];
        self.position += 1;
        let length = self.read_length()?;
        let end = self
            .position
            .checked_add(length)
            .ok_or(SmbClientError::InvalidAsn1Length)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SmbClientError::InvalidAsn1Length)?;
        self.position = end;
        Ok(DerTlv { tag, value })
    }

    fn read_length(&mut self) -> Result<usize, SmbClientError> {
        let Some(first) = self.bytes.get(self.position).copied() else {
            return Err(SmbClientError::InvalidAsn1Length);
        };
        self.position += 1;
        if first & 0x80 == 0 {
            return Ok(first as usize);
        }
        let count = (first & 0x7f) as usize;
        if count == 0 || count > std::mem::size_of::<usize>() {
            return Err(SmbClientError::InvalidAsn1Length);
        }
        let end = self
            .position
            .checked_add(count)
            .ok_or(SmbClientError::InvalidAsn1Length)?;
        let raw = self
            .bytes
            .get(self.position..end)
            .ok_or(SmbClientError::InvalidAsn1Length)?;
        if raw[0] == 0 && count > 1 {
            return Err(SmbClientError::InvalidAsn1Length);
        }
        let mut length = 0usize;
        for byte in raw {
            length = length
                .checked_mul(256)
                .and_then(|current| current.checked_add(*byte as usize))
                .ok_or(SmbClientError::InvalidAsn1Length)?;
        }
        if length < 0x80 {
            return Err(SmbClientError::InvalidAsn1Length);
        }
        self.position = end;
        Ok(length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_context_token_wraps_ntlm_negotiate() {
        let token = spnego_initial_context_token_ntlm(b"ntlm").expect("token");

        assert_eq!(
            hex(&token),
            "602406062b0601050502a01a3018a00e300c060a2b06010401823702020aa20604046e746c6d"
        );
    }

    #[test]
    fn neg_token_resp_wraps_ntlm_authenticate_without_gss_framing() {
        let token = spnego_neg_token_resp_ntlm(b"auth").expect("token");

        assert_eq!(hex(&token), "a10a3008a206040461757468");
    }

    #[test]
    fn decode_neg_token_resp_extracts_state_mech_and_response_token() {
        let token = encode_neg_token_resp(
            Some(SpnegoNegState::AcceptIncomplete),
            Some(true),
            Some(b"ntlm-challenge"),
            Some(b"mic"),
        )
        .expect("token");

        let decoded = decode_neg_token_resp(&token).expect("decode");

        assert_eq!(decoded.neg_state, Some(SpnegoNegState::AcceptIncomplete));
        assert!(decoded.supported_mech_is_ntlm);
        assert_eq!(
            decoded.response_token.as_deref(),
            Some(b"ntlm-challenge".as_slice())
        );
        assert_eq!(decoded.mech_list_mic.as_deref(), Some(b"mic".as_slice()));
    }

    #[test]
    fn decode_neg_token_resp_unwraps_initial_context_token_when_present() {
        let inner = encode_neg_token_resp(None, None, Some(b"token"), None).expect("inner");
        let outer_content = join(&[der_object_identifier(SPNEGO_OID_DER), inner]);
        let outer = der_tag(TAG_APPLICATION_0, &outer_content).expect("outer");

        let decoded = decode_neg_token_resp(&outer).expect("decode");

        assert_eq!(decoded.response_token.as_deref(), Some(b"token".as_slice()));
    }

    #[test]
    fn decode_neg_token_resp_rejects_unknown_state() {
        let state = der_tag(TAG_CONTEXT_0, &der_enumerated(4).expect("enum")).expect("state");
        let sequence = der_sequence(&[state]);
        let token = der_tag(TAG_NEG_TOKEN_RESP, &sequence).expect("token");

        assert_eq!(
            decode_neg_token_resp(&token),
            Err(SmbClientError::UnknownSpnegoNegState { state: 4 })
        );
    }

    #[test]
    fn decode_neg_token_resp_rejects_wrong_choice_tag() {
        let token = spnego_initial_context_token_ntlm(b"ntlm").expect("token");

        assert_eq!(
            decode_neg_token_resp(&token),
            Err(SmbClientError::InvalidAsn1Tag {
                expected: TAG_NEG_TOKEN_RESP,
                actual: TAG_NEG_TOKEN_INIT
            })
        );
    }

    #[test]
    fn decode_neg_token_resp_rejects_non_der_length() {
        let token = [TAG_NEG_TOKEN_RESP, 0x81, 0x01, 0x30];

        assert_eq!(
            decode_neg_token_resp(&token),
            Err(SmbClientError::InvalidAsn1Length)
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
}
