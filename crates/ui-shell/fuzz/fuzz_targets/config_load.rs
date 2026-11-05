#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    if let Ok(content) = std::str::from_utf8(data) {
        iris_ui_shell::config::parse_config_files(content);
    }
});
