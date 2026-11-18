//! Equality-only path identity. Never use these keys as locators or display paths.

use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;

pub const PATH_IDENTITY_COLLATION: &str = "IRIS_PATH_IDENTITY";

/// Equality policy for display paths. Only normalization is a policy here; case
/// sensitivity and path spelling are never decided by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathEquality {
    /// Distinct UTF-8 spellings are distinct paths, whatever the client OS.
    Bytes,
    /// Canonically equivalent Unicode spellings are the same path. Use only for
    /// namespaces whose normalization-insensitive semantics are verified.
    Nfc,
}

/// Byte-exact, platform-independent equality key.
pub fn equality_key(path: &str) -> String {
    equality_key_with_policy(path, PathEquality::Bytes)
}

/// Applies an explicit normalization policy without changing case or path syntax.
pub fn equality_key_with_policy(path: &str, policy: PathEquality) -> String {
    match policy {
        PathEquality::Bytes => path.to_owned(),
        PathEquality::Nfc => path.nfc().collect(),
    }
}

/// SQLite collation using the same byte equality as the default in-memory key.
pub fn compare(a: &str, b: &str) -> Ordering {
    a.as_bytes().cmp(b.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NFC: &str = "Café.jpg";
    const NFD: &str = "Cafe\u{301}.jpg";

    #[test]
    fn default_key_keeps_canonically_equivalent_spellings_distinct() {
        // No cfg!(target_os): the byte policy is the same on every target.
        assert_eq!(equality_key(NFC).as_bytes(), NFC.as_bytes());
        assert_eq!(equality_key(NFD).as_bytes(), NFD.as_bytes());
        assert_ne!(equality_key(NFC), equality_key(NFD));
        assert_eq!(
            equality_key(NFC),
            equality_key_with_policy(NFC, PathEquality::Bytes)
        );
        assert_eq!(
            equality_key(NFD),
            equality_key_with_policy(NFD, PathEquality::Bytes)
        );
    }

    #[test]
    fn nfc_policy_collapses_only_canonical_equivalence() {
        assert_eq!(equality_key_with_policy(NFC, PathEquality::Nfc), NFC);
        assert_eq!(equality_key_with_policy(NFD, PathEquality::Nfc), NFC);
        assert_eq!(
            equality_key_with_policy(NFC, PathEquality::Nfc),
            equality_key_with_policy(NFD, PathEquality::Nfc)
        );
    }

    #[test]
    fn neither_policy_folds_case_or_rewrites_display_spelling() {
        for policy in [PathEquality::Bytes, PathEquality::Nfc] {
            assert_ne!(
                equality_key_with_policy(NFC, policy),
                equality_key_with_policy("CAFÉ.jpg", policy)
            );
            assert_ne!(
                equality_key_with_policy(NFC, policy),
                equality_key_with_policy("café.jpg", policy)
            );
        }
        // Keys are comparison-only: neither policy is a locator or display string.
        assert_eq!(NFC, "Caf\u{e9}.jpg");
        assert_eq!(NFD, "Cafe\u{301}.jpg");
    }

    #[test]
    fn mixed_byte_sensitive_and_nfc_scope_pairs_differ() {
        // Canonically equivalent spellings: equal only under the Nfc policy.
        assert_ne!(equality_key(NFC), equality_key(NFD));
        assert_eq!(
            equality_key_with_policy(NFC, PathEquality::Nfc),
            equality_key_with_policy(NFD, PathEquality::Nfc)
        );
        // Case-only spellings: distinct under both policies.
        let uppercase = "CAFÉ.jpg";
        assert_ne!(equality_key(NFC), equality_key(uppercase));
        assert_ne!(
            equality_key_with_policy(NFC, PathEquality::Nfc),
            equality_key_with_policy(uppercase, PathEquality::Nfc)
        );
    }

    #[test]
    fn collation_compares_raw_bytes_on_every_platform() {
        assert_eq!(compare(NFC, NFC), Ordering::Equal);
        assert_ne!(compare(NFC, NFD), Ordering::Equal);
        assert_eq!(compare(NFC, NFD), NFC.as_bytes().cmp(NFD.as_bytes()));
        assert_eq!(compare(NFD, NFC), NFD.as_bytes().cmp(NFC.as_bytes()));
        assert_eq!(compare(NFC, NFD), equality_key(NFC).cmp(&equality_key(NFD)));
    }
}
