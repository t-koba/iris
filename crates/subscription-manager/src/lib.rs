use iris_platform_integration::file_watch::{
    watch_directory, watch_path, FileWatchHandle, WatchEvent,
};
use iris_platform_integration::mount::{list_mounted_volumes, MountedVolume};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

pub struct WatchHandle {
    pub cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    file_watch: Option<FileWatchHandle>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.file_watch.take();
    }
}

pub fn watch_directory_changes(
    path: &Path,
    sender: mpsc::Sender<WatchEvent>,
) -> Result<WatchHandle, iris_platform_integration::error::PlatformError> {
    let cancel = CancellationToken::new();
    let file_watch = watch_directory(path, sender)?;
    Ok(WatchHandle {
        cancel,
        task: None,
        file_watch: Some(file_watch),
    })
}

pub fn watch_mounted_volumes(sender: mpsc::Sender<Vec<MountedVolume>>) -> WatchHandle {
    let cancel = CancellationToken::new();
    let watch_cancel = cancel.child_token();

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let (file_watch, mut mount_events) = {
        let (event_sender, event_receiver) = mpsc::channel(16);
        let file_watch = watch_path(mount_watch_path(), event_sender).ok();
        (file_watch, event_receiver)
    };

    let task = tokio::spawn(async move {
        let mut last_volumes: Option<Vec<MountedVolume>> = None;

        if !emit_mounted_volumes_if_changed(&sender, &mut last_volumes).await {
            return;
        }

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            while !watch_cancel.is_cancelled() {
                tokio::select! {
                    _ = watch_cancel.cancelled() => break,
                    event = mount_events.recv() => {
                        if event.is_none() {
                            break;
                        }
                    }
                }

                tokio::select! {
                    _ = watch_cancel.cancelled() => break,
                    _ = sleep(Duration::from_millis(250)) => {}
                }
                while mount_events.try_recv().is_ok() {}

                if !emit_mounted_volumes_if_changed(&sender, &mut last_volumes).await {
                    break;
                }
            }
        }

        #[cfg(target_os = "windows")]
        loop {
            if watch_cancel.is_cancelled() {
                break;
            }

            if !emit_mounted_volumes_if_changed(&sender, &mut last_volumes).await {
                break;
            }

            tokio::select! {
                _ = watch_cancel.cancelled() => break,
                _ = sleep(Duration::from_secs(1)) => {}
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = watch_cancel;
        }
    });

    WatchHandle {
        cancel,
        task: Some(task),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        file_watch,
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        file_watch: None,
    }
}

pub fn watch_cloud_backends(sender: mpsc::Sender<()>) -> WatchHandle {
    let cancel = CancellationToken::new();
    let watch_cancel = cancel.child_token();
    let cloud_accounts_path = cloud_accounts_path();
    let (file_watch, mut cloud_events) = {
        let (event_sender, event_receiver) = mpsc::channel(16);
        let file_watch = cloud_accounts_path.parent().and_then(|parent| {
            let _ = std::fs::create_dir_all(parent);
            watch_path(parent, event_sender).ok()
        });
        (file_watch, event_receiver)
    };

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = watch_cancel.cancelled() => break,
                event = cloud_events.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    if !watch_event_touches_path(&event, &cloud_accounts_path) {
                        continue;
                    }
                }
            }

            tokio::select! {
                _ = watch_cancel.cancelled() => break,
                _ = sleep(Duration::from_millis(250)) => {}
            }
            while cloud_events.try_recv().is_ok() {}

            if sender.send(()).await.is_err() {
                break;
            }
        }
    });

    WatchHandle {
        cancel,
        task: Some(task),
        file_watch,
    }
}

fn watch_event_touches_path(event: &WatchEvent, target: &Path) -> bool {
    match event {
        WatchEvent::Created { path }
        | WatchEvent::Modified { path }
        | WatchEvent::Removed { path } => path == target,
        WatchEvent::Renamed { from, to } => from == target || to == target,
        WatchEvent::Rescan => true,
    }
}

async fn emit_mounted_volumes_if_changed(
    sender: &mpsc::Sender<Vec<MountedVolume>>,
    last_volumes: &mut Option<Vec<MountedVolume>>,
) -> bool {
    match tokio::task::spawn_blocking(list_mounted_volumes).await {
        Ok(Ok(volumes)) => {
            if last_volumes.as_ref() != Some(&volumes) {
                *last_volumes = Some(volumes.clone());
                return sender.send(volumes).await.is_ok();
            }
        }
        Ok(Err(error)) => {
            tracing::debug!("failed to list mounts: {}", error);
        }
        Err(error) => {
            tracing::debug!("mount listing task failed: {}", error);
        }
    }
    true
}

#[cfg(target_os = "macos")]
fn mount_watch_path() -> &'static Path {
    Path::new("/Volumes")
}

#[cfg(target_os = "linux")]
fn mount_watch_path() -> &'static Path {
    Path::new("/proc/mounts")
}

fn cloud_accounts_path() -> PathBuf {
    cloud_accounts_dir().join("cloud-accounts.json")
}

fn cloud_accounts_dir() -> PathBuf {
    dirs_next::data_local_dir()
        .or_else(dirs_next::config_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("iris")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_event_touches_path_matches_rename_endpoints() {
        let target = PathBuf::from("/tmp/iris/cloud-accounts.json");
        assert!(watch_event_touches_path(
            &WatchEvent::Renamed {
                from: PathBuf::from("/tmp/iris/cloud-accounts.json.tmp"),
                to: target.clone(),
            },
            &target,
        ));
        assert!(watch_event_touches_path(
            &WatchEvent::Renamed {
                from: target.clone(),
                to: PathBuf::from("/tmp/iris/cloud-accounts.json.old"),
            },
            &target,
        ));
    }

    #[test]
    fn watch_event_touches_path_ignores_sibling_files() {
        let target = PathBuf::from("/tmp/iris/cloud-accounts.json");
        assert!(!watch_event_touches_path(
            &WatchEvent::Modified {
                path: PathBuf::from("/tmp/iris/layout.json"),
            },
            &target,
        ));
    }

    #[test]
    fn watch_event_touches_path_treats_rescan_as_match() {
        let target = PathBuf::from("/tmp/iris/cloud-accounts.json");
        assert!(watch_event_touches_path(&WatchEvent::Rescan, &target));
    }
}
