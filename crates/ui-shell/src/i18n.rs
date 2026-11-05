use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use std::cell::RefCell;
use unic_langid::LanguageIdentifier;

const EN_FTL: &str = include_str!("../i18n/en/iris.ftl");
const JA_FTL: &str = include_str!("../i18n/ja/iris.ftl");

thread_local! {
    static I18N: RefCell<I18n> = RefCell::new(I18n::new(system_locale()));
}

struct I18n {
    locale: Locale,
    en: FluentBundle<FluentResource>,
    ja: FluentBundle<FluentResource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Locale {
    En,
    Ja,
}

impl Locale {
    fn from_tag(tag: &str) -> Self {
        if tag.to_ascii_lowercase().starts_with("ja") {
            Locale::Ja
        } else {
            Locale::En
        }
    }
}

pub fn initialize(locale_override: Option<&str>) {
    let locale = locale_override
        .map(Locale::from_tag)
        .unwrap_or_else(system_locale);
    I18N.with(|i18n| {
        i18n.borrow_mut().locale = locale;
    });
}

pub fn tr(key: &str) -> String {
    tr_args(key, None)
}

pub fn tr_args(key: &str, args: Option<&FluentArgs<'_>>) -> String {
    I18N.with(|i18n| i18n.borrow().format(key, args))
}

pub fn tr_with(key: &str, args: &[(&'static str, FluentValue<'_>)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, value.clone());
    }
    tr_args(key, Some(&fluent_args))
}

/// `key` in every bundled language, for recognising text that was saved in another language.
pub fn translations_of(key: &str) -> Vec<String> {
    I18N.with(|i18n| {
        let i18n = i18n.borrow();
        [&i18n.en, &i18n.ja]
            .into_iter()
            .filter_map(|bundle| i18n.format_from(bundle, key, None))
            .collect()
    })
}

pub fn is_japanese_locale() -> bool {
    I18N.with(|i18n| i18n.borrow().locale == Locale::Ja)
}

fn system_locale() -> Locale {
    sys_locale::get_locale()
        .as_deref()
        .map(Locale::from_tag)
        .unwrap_or(Locale::En)
}

impl I18n {
    fn new(locale: Locale) -> Self {
        Self {
            locale,
            en: build_bundle("en-US", EN_FTL),
            ja: build_bundle("ja-JP", JA_FTL),
        }
    }

    fn format(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        self.format_from(self.primary_bundle(), key, args)
            .or_else(|| self.format_from(&self.en, key, args))
            .unwrap_or_else(|| {
                #[cfg(not(test))]
                debug_assert!(false, "missing i18n key: {key}");
                key.to_string()
            })
    }

    fn primary_bundle(&self) -> &FluentBundle<FluentResource> {
        match self.locale {
            Locale::En => &self.en,
            Locale::Ja => &self.ja,
        }
    }

    fn format_from(
        &self,
        bundle: &FluentBundle<FluentResource>,
        key: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<String> {
        let message = bundle.get_message(key)?;
        let pattern = message.value()?;
        let mut errors = Vec::new();
        Some(
            bundle
                .format_pattern(pattern, args, &mut errors)
                .to_string(),
        )
    }
}

fn build_bundle(locale: &str, source: &str) -> FluentBundle<FluentResource> {
    let lang = locale
        .parse::<LanguageIdentifier>()
        .expect("bundled locale tag must be valid");
    let resource =
        FluentResource::try_new(source.to_string()).expect("bundled Fluent resource must parse");
    let mut bundle = FluentBundle::new(vec![lang]);
    bundle
        .add_resource(resource)
        .expect("bundled Fluent resource keys must be unique");
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn bundled_ftl_key_sets_match() {
        assert_eq!(ftl_keys(EN_FTL), ftl_keys(JA_FTL));
    }

    #[test]
    fn locale_override_selects_japanese() {
        let i18n = I18n::new(Locale::Ja);
        assert_eq!(i18n.format("settings-title", None), "\u{8a2d}\u{5b9a}");
    }

    #[test]
    fn rust_string_literals_do_not_embed_japanese_text() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations = Vec::new();
        collect_japanese_string_literals(&src_dir, &mut violations);
        assert!(
            violations.is_empty(),
            "Japanese text must live in FTL resources, not Rust string literals:\n{}",
            violations.join("\n")
        );
    }

    fn ftl_keys(source: &str) -> HashSet<String> {
        source
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with('.') {
                    return None;
                }
                let (key, _) = line.split_once('=')?;
                Some(key.trim().to_string())
            })
            .collect()
    }

    fn collect_japanese_string_literals(path: &Path, violations: &mut Vec<String>) {
        let entries = std::fs::read_dir(path).expect("source directory must be readable");
        for entry in entries {
            let entry = entry.expect("source directory entry must be readable");
            let path = entry.path();
            if path.is_dir() {
                collect_japanese_string_literals(&path, violations);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let source = std::fs::read_to_string(&path).expect("source file must be readable");
                for (line, literal) in japanese_string_literals(&source) {
                    violations.push(format!("{}:{}: {}", path.display(), line, literal));
                }
            }
        }
    }

    fn japanese_string_literals(source: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut chars = source.char_indices().peekable();
        let mut line = 1usize;
        let mut in_line_comment = false;
        let mut block_comment_depth = 0usize;

        while let Some((_, ch)) = chars.next() {
            if ch == '\n' {
                line += 1;
                in_line_comment = false;
                continue;
            }
            if in_line_comment {
                continue;
            }
            if block_comment_depth > 0 {
                if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '*') {
                    chars.next();
                    block_comment_depth += 1;
                } else if ch == '*' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                    chars.next();
                    block_comment_depth -= 1;
                }
                continue;
            }
            if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                chars.next();
                in_line_comment = true;
                continue;
            }
            if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '*') {
                chars.next();
                block_comment_depth = 1;
                continue;
            }
            if ch != '"' {
                continue;
            }

            let literal_line = line;
            let mut literal = String::new();
            let mut escaped = false;
            for (_, literal_ch) in chars.by_ref() {
                if literal_ch == '\n' {
                    line += 1;
                }
                if escaped {
                    escaped = false;
                    continue;
                }
                if literal_ch == '\\' {
                    escaped = true;
                    continue;
                }
                if literal_ch == '"' {
                    break;
                }
                literal.push(literal_ch);
            }
            if literal.chars().any(is_japanese_char) {
                out.push((literal_line, literal));
            }
        }

        out
    }

    fn is_japanese_char(ch: char) -> bool {
        matches!(
            ch,
            '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
        )
    }
}
