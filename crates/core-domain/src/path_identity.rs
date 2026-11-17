//! Equality-only path identity. Never use these keys as locators or display paths.

use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;

pub const PATH_IDENTITY_COLLATION: &str = "IRIS_PATH_IDENTITY";

/// macOS path equality is normalization-insensitive; other platforms retain byte equality.
/// This does not change case sensitivity or the original path spelling.
pub fn equality_key(path: &str) -> String {
    equality_key_with_normalization(path, cfg!(target_os = "macos"))
}

/// Applies an explicit normalization policy without changing case or path syntax.
pub fn equality_key_with_normalization(path: &str, normalize: bool) -> String {
    if normalize {
        path.nfc().collect()
    } else {
        path.to_owned()
    }
}

/// SQLite collation using the same equality policy as in-memory keys.
pub fn compare(a: &str, b: &str) -> Ordering {
    equality_key(a).cmp(&equality_key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_explicit_and_equality_only() {
        let nfc = "Café.jpg";
        let nfd = "Cafe\u{301}.jpg";
        assert_eq!(equality_key_with_normalization(nfc, true), nfc);
        assert_eq!(equality_key_with_normalization(nfd, true), nfc);
        assert_eq!(equality_key_with_normalization(nfc, false), nfc);
        assert_eq!(equality_key_with_normalization(nfd, false), nfd);
        assert_ne!(
            equality_key_with_normalization(nfc, false),
            equality_key_with_normalization(nfd, false)
        );
        assert_ne!(
            equality_key_with_normalization(nfc, true),
            equality_key_with_normalization("CAFÉ.jpg", true)
        );
        assert_eq!(
            equality_key(nfd),
            if cfg!(target_os = "macos") { nfc } else { nfd }
        );
        assert_eq!(compare(nfc, nfd), equality_key(nfc).cmp(&equality_key(nfd)));
    }
}
