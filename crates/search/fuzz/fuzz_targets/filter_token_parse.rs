#![no_main]

use iris_search::token::{format_filter_tokens, parse_filter_tokens};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let parsed = parse_filter_tokens(&input);
    let formatted = format_filter_tokens(&parsed.model);
    let reparsed = parse_filter_tokens(&formatted);

    assert_eq!(parsed.model, reparsed.model);
    assert!(reparsed.warnings.is_empty());
});
