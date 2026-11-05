use crate::ngram::grams;

pub const BLOOM_BYTES: usize = 256;
const BLOOM_BITS: u64 = (BLOOM_BYTES * 8) as u64;
const HASH_SEEDS: [u64; 4] = [
    0xcbf2_9ce4_8422_2325,
    0x9e37_79b9_7f4a_7c15,
    0x517c_c1b7_2722_0a95,
    0x94d0_49bb_1331_11eb,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BloomSignature {
    bytes: [u8; BLOOM_BYTES],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BloomQuery {
    grams: Vec<String>,
}

impl BloomQuery {
    pub fn new(query: &str) -> Self {
        Self {
            grams: grams(query),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.grams.is_empty()
    }
}

impl Default for BloomSignature {
    fn default() -> Self {
        Self {
            bytes: [0; BLOOM_BYTES],
        }
    }
}

impl BloomSignature {
    pub fn from_content(text: &str) -> Self {
        let mut signature = Self::default();
        for gram in grams(text) {
            signature.insert(gram.as_bytes());
        }
        signature
    }

    pub fn might_contain_query(&self, query: &str) -> bool {
        self.might_contain(&BloomQuery::new(query))
    }

    pub fn might_contain(&self, query: &BloomQuery) -> bool {
        !query.is_empty()
            && query
                .grams
                .iter()
                .all(|gram| self.contains(gram.as_bytes()))
    }

    pub fn as_bytes(&self) -> &[u8; BLOOM_BYTES] {
        &self.bytes
    }

    pub fn from_bytes(bytes: [u8; BLOOM_BYTES]) -> Self {
        Self { bytes }
    }

    fn insert(&mut self, value: &[u8]) {
        for seed in HASH_SEEDS {
            let bit = hash_with_seed(value, seed) % BLOOM_BITS;
            self.bytes[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }

    fn contains(&self, value: &[u8]) -> bool {
        HASH_SEEDS.iter().all(|seed| {
            let bit = hash_with_seed(value, *seed) % BLOOM_BITS;
            self.bytes[(bit / 8) as usize] & (1 << (bit % 8)) != 0
        })
    }
}

fn hash_with_seed(value: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in value {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
        hash ^= hash >> 32;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_present_ascii_and_cjk_terms() {
        let signature = BloomSignature::from_content("Preview 日本語 PDF text");
        assert!(signature.might_contain_query("preview"));
        assert!(signature.might_contain_query("日本語"));
        assert!(signature.might_contain_query("PDF"));
        assert!(!signature.might_contain_query("absent-term-with-many-grams"));
    }

    #[test]
    fn serialized_signature_round_trips() {
        let signature = BloomSignature::from_content("camera lens metadata");
        let restored = BloomSignature::from_bytes(*signature.as_bytes());
        assert_eq!(signature, restored);
        assert!(restored.might_contain_query("metadata"));
    }

    #[test]
    fn precompiled_query_matches_signature() {
        let signature = BloomSignature::from_content("日本語 PDF metadata");
        let query = BloomQuery::new("日本語");
        assert!(signature.might_contain(&query));
    }
}
