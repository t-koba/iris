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

/// Why a name typed by the user cannot name a file or folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileNameProblem {
    Empty,
    /// `/` or `\` would address another folder.
    PathSeparator,
    /// `.` and `..` name the folder itself and its parent.
    Reserved,
    /// Control characters anywhere; on Windows also `<>:"|?*` and a trailing dot or space.
    InvalidCharacter,
}

/// Checks a single file or folder name (not a path) before it is created or renamed to.
pub fn file_name_problem(name: &str) -> Option<FileNameProblem> {
    if name.trim().is_empty() {
        return Some(FileNameProblem::Empty);
    }
    if name.contains(['/', '\\']) {
        return Some(FileNameProblem::PathSeparator);
    }
    if name == "." || name == ".." {
        return Some(FileNameProblem::Reserved);
    }
    if name.chars().any(char::is_control)
        || (cfg!(windows)
            && (name.contains(['<', '>', ':', '"', '|', '?', '*']) || name.ends_with(['.', ' '])))
    {
        return Some(FileNameProblem::InvalidCharacter);
    }
    None
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

#[cfg(test)]
mod file_name_tests {
    use super::*;

    #[test]
    fn file_name_problem_rejects_names_that_address_other_files() {
        assert_eq!(file_name_problem("  "), Some(FileNameProblem::Empty));
        assert_eq!(
            file_name_problem("a/b"),
            Some(FileNameProblem::PathSeparator)
        );
        assert_eq!(
            file_name_problem("a\\b"),
            Some(FileNameProblem::PathSeparator)
        );
        assert_eq!(file_name_problem(".."), Some(FileNameProblem::Reserved));
        assert_eq!(file_name_problem("."), Some(FileNameProblem::Reserved));
        assert_eq!(
            file_name_problem("a\0b"),
            Some(FileNameProblem::InvalidCharacter)
        );
        assert_eq!(
            file_name_problem("line\nbreak"),
            Some(FileNameProblem::InvalidCharacter)
        );
        assert_eq!(file_name_problem(".hidden"), None);
        assert_eq!(file_name_problem("...and more"), None);
        assert_eq!(file_name_problem("Café 2024"), None);
    }
}
