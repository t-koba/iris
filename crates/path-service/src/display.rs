use std::path::Path;

pub fn parent_relative_display_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }

    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
}

pub fn join_display_path(base: &str, child: &str) -> String {
    if base.is_empty() || base == "/" {
        format!("/{}", child.trim_start_matches('/'))
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            child.trim_start_matches('/')
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{join_display_path, parent_relative_display_path};

    #[test]
    fn join_display_path_preserves_virtual_root_shape() {
        assert_eq!(
            join_display_path("/__iris_remote__/pcloud:42", "photos"),
            "/__iris_remote__/pcloud:42/photos"
        );
        assert_eq!(join_display_path("/", "tmp"), "/tmp");
    }

    #[test]
    fn parent_relative_display_path_handles_nested_paths() {
        assert_eq!(
            parent_relative_display_path("photos/2024/raw"),
            Some("photos/2024".to_string())
        );
        assert_eq!(parent_relative_display_path("photos"), Some(String::new()));
        assert_eq!(parent_relative_display_path(""), None);
    }
}
