#![no_main]

use iris_thumb_preview::raw;
use libfuzzer_sys::fuzz_target;
use tempfile::NamedTempFile;

fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * 1024 * 1024 {
        return;
    }

    let file = NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), data).expect("write temp file");
    let path = file.path().with_extension("cr3");
    std::fs::rename(file.path(), &path).expect("rename temp file");
    let _ = raw::extract_raw_thumbnail(&path, 256);
    let _ = std::fs::remove_file(path);
});
