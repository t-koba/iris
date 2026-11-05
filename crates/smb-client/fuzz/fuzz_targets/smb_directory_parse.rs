#![no_main]

use iris_smb_client::decode_file_id_both_directory_entries;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_file_id_both_directory_entries(data);
});
