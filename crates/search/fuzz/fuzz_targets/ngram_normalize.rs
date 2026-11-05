#![no_main]

use iris_search::ngram::{grams, normalize_text};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let normalized = normalize_text(&input);
    assert_eq!(normalized, normalize_text(&normalized));

    assert!(grams(&input).iter().all(|gram| !gram.is_empty()));
});
