#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Deserialize)]
struct SettingsFile {
    schema_version: u64,
    #[serde(default)]
    show_hidden_files: bool,
    #[serde(default)]
    thumbnail_cache_limit_bytes: Option<usize>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    shortcut_overrides: Vec<ShortcutOverride>,
}

#[derive(Debug, Deserialize)]
struct ShortcutOverride {
    action: String,
    combo: String,
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }

    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(SCHEMA_VERSION)
    {
        return;
    }
    let parsed = serde_json::from_value::<SettingsFile>(value);
    if let Ok(settings) = parsed {
        let _ = (
            settings.schema_version,
            settings.show_hidden_files,
            settings.thumbnail_cache_limit_bytes,
            settings.theme.as_deref(),
            settings.locale.as_deref(),
        );
        for shortcut in settings.shortcut_overrides {
            let _ = (shortcut.action, shortcut.combo);
        }
    }
});
