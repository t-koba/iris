#![no_main]

use iris_smb_client::auth::{ntlmssp::NtlmsspChallenge, spnego::decode_neg_token_resp};
use iris_smb_client::{Smb2NegotiateRequest, Smb2NegotiateResponse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = Smb2NegotiateRequest::decode(data);
    let _ = Smb2NegotiateResponse::decode_packet(data);
    let _ = NtlmsspChallenge::decode(data);
    let _ = decode_neg_token_resp(data);
});
