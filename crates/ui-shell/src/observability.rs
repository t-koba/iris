use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
const LOG_RETENTION: Duration = Duration::from_secs(14 * 24 * 60 * 60);

pub fn init_tracing() {
    let default_filter = "warn";
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    let Some(log_dir) = log_dir() else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .try_init()
            .ok();
        return;
    };

    if let Err(error) = std::fs::create_dir_all(&log_dir) {
        eprintln!("failed to create Iris log directory: {error}");
    }
    cleanup_old_logs(&log_dir, SystemTime::now());

    let file_appender = tracing_appender::rolling::daily(&log_dir, "iris.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
    let _ = LOG_GUARD.set(guard);

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_target(true)
        .with_ansi(false);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_writer(std::io::stderr)
        .with_target(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init()
        .ok();
}

pub fn log_dir() -> Option<PathBuf> {
    dirs_next::data_dir().map(|dir| dir.join("iris").join("logs"))
}

fn cleanup_old_logs(log_dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_iris_log_file(&path) {
            continue;
        }
        let should_remove = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > LOG_RETENTION);
        if should_remove {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn is_iris_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "iris.log" || name.starts_with("iris.log."))
}

pub fn path_hash(path: impl AsRef<Path>) -> String {
    stable_hash(path.as_ref().as_os_str().as_encoded_bytes())
}

pub fn stable_hash(bytes: impl AsRef<[u8]>) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.as_ref().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iris_log_file_detection_is_scoped() {
        assert!(is_iris_log_file(Path::new("iris.log")));
        assert!(is_iris_log_file(Path::new("iris.log.2026-06-14")));
        assert!(!is_iris_log_file(Path::new("other.log")));
        assert!(!is_iris_log_file(Path::new("iris.txt")));
    }
}
