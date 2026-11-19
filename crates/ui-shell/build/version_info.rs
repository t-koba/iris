use std::path::{Component, PathBuf};

const ICON_PATH_PLACEHOLDER: &str = "@IRIS_ICON_PATH@";
const FILE_VERSION_PLACEHOLDER: &str = "@IRIS_FILE_VERSION@";
const PRODUCT_VERSION_PLACEHOLDER: &str = "@IRIS_PRODUCT_VERSION@";
const FILE_VERSION_STRING_PLACEHOLDER: &str = "@IRIS_FILE_VERSION_STRING@";
const PRODUCT_VERSION_STRING_PLACEHOLDER: &str = "@IRIS_PRODUCT_VERSION_STRING@";

const VERSION_INFO_PLACEHOLDERS: [&str; 5] = [
    ICON_PATH_PLACEHOLDER,
    FILE_VERSION_PLACEHOLDER,
    PRODUCT_VERSION_PLACEHOLDER,
    FILE_VERSION_STRING_PLACEHOLDER,
    PRODUCT_VERSION_STRING_PLACEHOLDER,
];

/// Turn a three-part semver core (with an optional suffix) into a Windows
/// FILEVERSION quadruple. The fourth component is always zero.
fn windows_numeric_version(version: &str) -> Result<[u16; 4], String> {
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected exactly three numeric components, got {version:?}"
        ));
    }

    if core.len() != version.len() {
        let suffix = &version[core.len()..];
        let body = &suffix[1..];
        if body.is_empty()
            || !body.chars().any(|c| c.is_ascii_alphanumeric())
            || !body
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        {
            return Err(format!("invalid semver suffix in {version:?}"));
        }
    }

    let mut numeric = [0u16; 4];
    for (slot, part) in numeric[..3].iter_mut().zip(parts) {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("non-numeric version component in {version:?}"));
        }
        *slot = part
            .parse::<u16>()
            .map_err(|_| format!("version component exceeds 65535 in {version:?}"))?;
    }
    Ok(numeric)
}

/// Resolve `..` components without touching the filesystem (so Windows drive
/// prefixes and UNC paths survive on every host).
fn normalize_icon_path(path: &std::path::Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Replace every placeholder; refuse templates that are missing placeholders
/// or still contain unrecognized ones.
fn render_version_info(
    template: &str,
    icon_path: &str,
    numeric_version: &[u16; 4],
    version_string: &str,
) -> Result<String, String> {
    for placeholder in VERSION_INFO_PLACEHOLDERS {
        if !template.contains(placeholder) {
            return Err(format!("template is missing placeholder {placeholder}"));
        }
    }

    let numeric = format!(
        "{},{},{},{}",
        numeric_version[0], numeric_version[1], numeric_version[2], numeric_version[3]
    );
    let file_version_string = format!(
        "{}.{}.{}",
        numeric_version[0], numeric_version[1], numeric_version[2]
    );
    let rendered = template
        .replace(ICON_PATH_PLACEHOLDER, icon_path)
        .replace(FILE_VERSION_STRING_PLACEHOLDER, &file_version_string)
        .replace(PRODUCT_VERSION_STRING_PLACEHOLDER, version_string)
        .replace(FILE_VERSION_PLACEHOLDER, &numeric)
        .replace(PRODUCT_VERSION_PLACEHOLDER, &numeric);

    if rendered.contains("@IRIS_") {
        return Err("rendered template still contains placeholders".to_string());
    }
    Ok(rendered)
}
