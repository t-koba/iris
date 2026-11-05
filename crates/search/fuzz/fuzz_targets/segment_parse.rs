#![no_main]

use iris_search::segment::Segment;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(segment) = Segment::from_bytes(data) else {
        return;
    };

    let encoded = segment.to_bytes().expect("parsed segment should re-encode");
    let reparsed = Segment::from_bytes(&encoded).expect("re-encoded segment should parse");
    assert_eq!(segment.records(), reparsed.records());
});
