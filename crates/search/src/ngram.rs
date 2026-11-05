use unicode_normalization::UnicodeNormalization;

pub fn normalize_text(input: &str) -> String {
    input
        .nfkc()
        .flat_map(char::to_lowercase)
        .filter_map(fold_char)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Content n-grams: bigrams for CJK, trigrams otherwise; tokens of up to 3 chars are kept whole.
pub fn grams(input: &str) -> Vec<String> {
    let normalized = normalize_text(input);
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut output = Vec::new();
    for token in normalized.split_whitespace() {
        let chars: Vec<char> = token.chars().collect();
        if chars.iter().any(|ch| is_cjk(*ch)) {
            output.extend(windowed_grams(&chars, 2));
            continue;
        }

        if chars.len() <= 3 {
            output.push(token.to_string());
        } else {
            output.extend(windowed_grams(&chars, 3));
        }
    }
    output
}

fn windowed_grams(chars: &[char], width: usize) -> Vec<String> {
    if chars.len() <= width {
        return vec![chars.iter().collect()];
    }
    chars
        .windows(width)
        .map(|window| window.iter().collect())
        .collect()
}

fn fold_char(ch: char) -> Option<char> {
    if ch.is_control() {
        return None;
    }
    if ('\u{3041}'..='\u{3096}').contains(&ch) {
        return char::from_u32(ch as u32 + 0x60);
    }
    Some(ch)
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_nfkc_case_and_kana() {
        assert_eq!(normalize_text(" ＡＢＣ かな ｶﾅ "), "abc カナ カナ");
    }

    #[test]
    fn content_uses_ascii_trigrams() {
        assert_eq!(grams("Preview"), vec!["pre", "rev", "evi", "vie", "iew"]);
    }

    #[test]
    fn cjk_uses_bigrams() {
        assert_eq!(grams("日本語 PDF"), vec!["日本", "本語", "pdf"]);
    }

    #[test]
    fn short_terms_are_kept_as_single_candidates() {
        assert_eq!(grams("ab"), vec!["ab"]);
    }
}
