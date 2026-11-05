use std::path::{Component, Path, PathBuf};

pub fn normalize_relative_display_path(path: &str) -> String {
    let mut normalized = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => {
                normalized.push(segment.to_string_lossy().to_string());
            }
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized.join("/")
}

pub fn normalize_local_path(path: &Path) -> String {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
        }
    }

    let normalized = normalized.to_string_lossy().to_string();
    if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    }
}

pub fn expand_home_path(path: &str) -> Option<String> {
    if path == "~" {
        return dirs_next::home_dir().map(|home| home.to_string_lossy().to_string());
    }

    path.strip_prefix("~/").and_then(|rest| {
        dirs_next::home_dir().map(|home| home.join(rest).to_string_lossy().to_string())
    })
}

pub fn sanitize_file_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "download".to_string()
    } else {
        sanitized
    }
}
