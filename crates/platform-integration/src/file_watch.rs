use crate::error::PlatformError;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchEvent {
    Created { path: PathBuf },
    Modified { path: PathBuf },
    Removed { path: PathBuf },
    Renamed { from: PathBuf, to: PathBuf },
    Rescan,
}

/// Watching stops when the handle is dropped.
pub struct FileWatchHandle {
    _watcher: RecommendedWatcher,
}

/// Watches a directory with the OS-native API (via `notify`) and sends events to a channel.
pub fn watch_directory(
    path: &Path,
    sender: mpsc::Sender<WatchEvent>,
) -> Result<FileWatchHandle, PlatformError> {
    watch_path(path, sender)
}

/// The callback cannot await, so when the bounded channel is full new events are merged into pending ones.
pub fn watch_path(
    path: &Path,
    sender: mpsc::Sender<WatchEvent>,
) -> Result<FileWatchHandle, PlatformError> {
    let runtime_handle = tokio::runtime::Handle::try_current().ok();
    let rescan_pending = Arc::new(AtomicBool::new(false));
    let mut watcher = RecommendedWatcher::new(
        {
            let rescan_pending = Arc::clone(&rescan_pending);
            move |result: Result<Event, notify::Error>| match result {
                Ok(event) => {
                    let events = convert_event(event);
                    for ev in events {
                        if !try_send_watch_event(
                            &sender,
                            ev,
                            &rescan_pending,
                            runtime_handle.as_ref(),
                        ) {
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!("file watch error: {}", e);
                    schedule_rescan(
                        sender.clone(),
                        Arc::clone(&rescan_pending),
                        runtime_handle.clone(),
                    );
                }
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| PlatformError::FileWatch {
        message: e.to_string(),
    })?;

    watcher
        .watch(path, RecursiveMode::NonRecursive)
        .map_err(|e| PlatformError::FileWatch {
            message: format!("{}: {}", path.display(), e),
        })?;

    Ok(FileWatchHandle { _watcher: watcher })
}

fn try_send_watch_event(
    sender: &mpsc::Sender<WatchEvent>,
    event: WatchEvent,
    rescan_pending: &Arc<AtomicBool>,
    runtime_handle: Option<&tokio::runtime::Handle>,
) -> bool {
    match sender.try_send(event) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            schedule_rescan(
                sender.clone(),
                Arc::clone(rescan_pending),
                runtime_handle.cloned(),
            );
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn schedule_rescan(
    sender: mpsc::Sender<WatchEvent>,
    rescan_pending: Arc<AtomicBool>,
    runtime_handle: Option<tokio::runtime::Handle>,
) {
    if rescan_pending.swap(true, Ordering::AcqRel) {
        return;
    }

    if let Some(handle) = runtime_handle {
        handle.spawn(async move {
            let _ = sender.send(WatchEvent::Rescan).await;
            rescan_pending.store(false, Ordering::Release);
        });
    } else {
        std::thread::spawn(move || {
            let _ = sender.blocking_send(WatchEvent::Rescan);
            rescan_pending.store(false, Ordering::Release);
        });
    }
}

fn convert_event(event: Event) -> Vec<WatchEvent> {
    let mut results = Vec::new();
    match event.kind {
        EventKind::Create(_) => {
            for path in event.paths {
                results.push(WatchEvent::Created { path });
            }
        }
        EventKind::Modify(ModifyKind::Name(
            RenameMode::Both
            | RenameMode::From
            | RenameMode::To
            | RenameMode::Any
            | RenameMode::Other,
        )) => {
            if event.paths.len() >= 2 {
                results.push(WatchEvent::Renamed {
                    from: event.paths[0].clone(),
                    to: event.paths[1].clone(),
                });
            } else {
                for path in event.paths {
                    results.push(WatchEvent::Modified { path });
                }
            }
        }
        EventKind::Modify(_) => {
            for path in event.paths {
                results.push(WatchEvent::Modified { path });
            }
        }
        EventKind::Remove(_) => {
            for path in event.paths {
                results.push(WatchEvent::Removed { path });
            }
        }
        _ => {}
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{ModifyKind, RenameMode};

    #[test]
    fn convert_event_emits_renamed_variant() {
        let from = PathBuf::from("/tmp/from.txt");
        let to = PathBuf::from("/tmp/to.txt");
        let events = convert_event(Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            paths: vec![from.clone(), to.clone()],
            attrs: Default::default(),
        });

        assert!(matches!(
            events.as_slice(),
            [WatchEvent::Renamed { from: actual_from, to: actual_to }]
                if actual_from == &from && actual_to == &to
        ));
    }

    #[tokio::test]
    async fn full_channel_schedules_single_rescan_marker() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(WatchEvent::Modified {
                path: PathBuf::from("/tmp/existing.txt"),
            })
            .expect("fill channel");
        let pending = Arc::new(AtomicBool::new(false));
        let handle = tokio::runtime::Handle::current();

        assert!(try_send_watch_event(
            &sender,
            WatchEvent::Created {
                path: PathBuf::from("/tmp/a.txt"),
            },
            &pending,
            Some(&handle),
        ));
        assert!(try_send_watch_event(
            &sender,
            WatchEvent::Removed {
                path: PathBuf::from("/tmp/b.txt"),
            },
            &pending,
            Some(&handle),
        ));

        assert!(pending.load(Ordering::Acquire));
        assert_eq!(
            receiver.recv().await,
            Some(WatchEvent::Modified {
                path: PathBuf::from("/tmp/existing.txt"),
            })
        );
        assert_eq!(receiver.recv().await, Some(WatchEvent::Rescan));
        assert!(!pending.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
    }
}
