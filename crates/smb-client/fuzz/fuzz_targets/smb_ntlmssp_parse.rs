#![no_main]

use iris_smb_client::NtlmsspChallenge;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = NtlmsspChallenge::decode(data);
});
