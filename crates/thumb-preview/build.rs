//! Compile-time binding for preview identity: Cargo profile and effective opt-level.
//!
//! Build-script `OPT_LEVEL` sees profile overrides but not a raw `-C opt-level`
//! or `-O` shorthand in RUSTFLAGS, so such overrides are rejected instead of
//! letting a build claim settings it was not compiled with.

use std::env;

const ENV_KEYS: [&str; 4] = [
    "PROFILE",
    "OPT_LEVEL",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
];

fn is_target_rustflags(name: &str) -> bool {
    name.starts_with("CARGO_TARGET_") && name.ends_with("_RUSTFLAGS")
}

fn codegen_opt_level_override(flags: &str) -> bool {
    // CARGO_ENCODED_RUSTFLAGS separates arguments with U+001F; its written form
    // is accepted too so encoded flag strings cannot hide an override.
    let flags = flags.replace("\\x1f", " ");
    let tokens: Vec<&str> = flags
        .split(|c: char| c.is_whitespace() || c == '\u{1f}')
        .filter(|token| !token.is_empty())
        .collect();
    tokens.contains(&"-O")
        || tokens.iter().enumerate().any(|(index, token)| {
            let payload = token
                .strip_prefix("-C")
                .or_else(|| token.strip_prefix("--codegen"))
                .map(|rest| rest.strip_prefix('=').unwrap_or(rest));
            match payload {
                Some("") => tokens
                    .get(index + 1)
                    .is_some_and(|next| next.starts_with("opt-level")),
                Some(rest) => rest.starts_with("opt-level"),
                None => false,
            }
        })
}

fn rustflags() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        if let Some(value) = env::var_os(name) {
            let value = value
                .to_str()
                .unwrap_or_else(|| panic!("{name} is not valid UTF-8"));
            found.push((name.to_owned(), value.to_owned()));
        }
    }
    for (name, value) in env::vars_os() {
        let name = name.to_string_lossy();
        if is_target_rustflags(&name) {
            let value = value
                .to_str()
                .unwrap_or_else(|| panic!("{name} is not valid UTF-8"));
            found.push((name.into_owned(), value.to_owned()));
        }
    }
    found
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for name in ENV_KEYS {
        println!("cargo:rerun-if-env-changed={name}");
    }
    for key in ["TARGET", "HOST"] {
        if let Ok(value) = env::var(key) {
            println!(
                "cargo:rerun-if-env-changed=CARGO_TARGET_{}_RUSTFLAGS",
                value.to_uppercase().replace('-', "_")
            );
        }
    }
    for (name, flags) in rustflags() {
        assert!(
            !codegen_opt_level_override(&flags),
            "{name} overrides opt-level; preview identity must use the Cargo profile opt-level"
        );
    }

    let profile = env::var("PROFILE").expect("PROFILE is required for preview identity");
    assert!(
        !profile.is_empty()
            && profile.len() <= 64
            && profile
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "unexpected PROFILE value for preview identity"
    );
    let opt_level = env::var("OPT_LEVEL").expect("OPT_LEVEL is required for preview identity");
    assert!(
        matches!(opt_level.as_str(), "0" | "1" | "2" | "3" | "s" | "z"),
        "unexpected OPT_LEVEL value for preview identity: {opt_level}"
    );
    println!("cargo:rustc-env=IRIS_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=IRIS_BUILD_OPT_LEVEL={opt_level}");
}

#[cfg(test)]
mod tests {
    use super::{codegen_opt_level_override, is_target_rustflags};

    #[test]
    fn rejects_opt_level_codegen_forms() {
        for flags in [
            "-C opt-level=3",
            "-Copt-level=0",
            "--codegen opt-level=z",
            "--codegen=opt-level=s",
            "-C=opt-level=1",
            "-C opt-level 2",
            "-Clto=thin -Copt-level=1",
            "-Clto=thin\u{1f}--codegen\u{1f}opt-level=2",
            "-Clto=thin\\x1f-Copt-level=z",
            "-O",
            "-Clto=thin -O",
            "-Clto=thin\\x1f-O",
        ] {
            assert!(
                codegen_opt_level_override(flags),
                "missed override: {flags}"
            );
        }
    }

    #[test]
    fn ignores_unrelated_flags() {
        for flags in [
            "",
            "-C debuginfo=2",
            "--codegen lto=thin",
            "-C link-arg=-Wl,-O2",
            "-Z unstable-options opt-level",
            "-Cpanic=abort -Cstrip=symbols",
        ] {
            assert!(
                !codegen_opt_level_override(flags),
                "false positive: {flags}"
            );
        }
    }

    #[test]
    fn recognizes_target_specific_rustflag_names() {
        assert!(is_target_rustflags(
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS"
        ));
        assert!(is_target_rustflags(
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS"
        ));
        assert!(!is_target_rustflags("CARGO_ENCODED_RUSTFLAGS"));
        assert!(!is_target_rustflags("CARGO_TARGET_DIR"));
    }
}
