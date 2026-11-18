//! Equality-only path identity. Never use these keys as locators or display paths.

use std::cmp::Ordering;

pub const PATH_IDENTITY_COLLATION: &str = "IRIS_PATH_IDENTITY";

/// Byte-exact, platform-independent equality key.
pub fn equality_key(path: &str) -> String {
    path.to_owned()
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
    fn key_keeps_canonically_equivalent_spellings_distinct() {
        // No OS-dependent policy: byte equality is the same on every target.
        assert_eq!(equality_key(NFC).as_bytes(), NFC.as_bytes());
        assert_eq!(equality_key(NFD).as_bytes(), NFD.as_bytes());
        assert_ne!(equality_key(NFC), equality_key(NFD));
    }

    #[test]
    fn key_does_not_fold_case_or_rewrite_display_spelling() {
        assert_ne!(equality_key(NFC), equality_key("CAFÉ.jpg"));
        assert_ne!(equality_key(NFC), equality_key("café.jpg"));
        assert_eq!(NFC, "Caf\u{e9}.jpg");
        assert_eq!(NFD, "Cafe\u{301}.jpg");
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
