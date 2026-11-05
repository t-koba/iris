use crate::config::ThemeChoice;
use crate::messages::{DirectoryMessage, JobMessage, Message};
use crate::runtime::app_runtime;
use crate::state::AppState;
use iced::keyboard;
use iced::stream;
use iced::{window, Subscription};
use iris_core_domain::event::DomainEvent;
use std::any::TypeId;
use std::hash::Hash;
use std::time::Duration;

pub fn subscription(state: &AppState) -> Subscription<Message> {
    let mut subscriptions = vec![
        keyboard::listen().filter_map(|event| match event {
            keyboard::Event::KeyPressed { key, modifiers, .. } => {
                Some(Message::KeyPressed(key, modifiers))
            }
            _ => None,
        }),
        iced::event::listen_raw(|event, _status, _window| match event {
            iced::event::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) => {
                Some(Message::ModifiersChanged(modifiers))
            }
            iced::event::Event::Mouse(iced::mouse::Event::ButtonReleased(
                iced::mouse::Button::Left,
            )) => Some(Message::ResizeEnd),
            iced::event::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                Some(Message::MouseMoved(position))
            }
            _ => None,
        }),
        window::resize_events().map(|(_id, size)| Message::WindowResized(size)),
        window::events().map(|(id, event)| Message::WindowEvent(id, event)),
    ];

    for pane_idx in 0..2 {
        let current_path = &state.pane(pane_idx).current_path;
        if !current_path.is_empty() && !app_runtime().is_virtual_display_path(current_path) {
            subscriptions.push(directory_watch_subscription(pane_idx, current_path.clone()));
        }
    }

    subscriptions.push(runtime_event_subscription());
    subscriptions.push(config_file_watch_subscription());
    subscriptions.push(mounted_volumes_subscription());
    subscriptions.push(cloud_backends_subscription());
    if state.theme_choice == ThemeChoice::System {
        subscriptions.push(iced::system::theme_changes().map(Message::SystemThemeChanged));
    }
    if let Some((notification_id, delay)) = state.next_notification_expiry() {
        subscriptions.push(notification_expiry_subscription(notification_id, delay));
    }
    #[cfg(target_os = "macos")]
    subscriptions.push(macos_menu_command_subscription());

    Subscription::batch(subscriptions)
}

struct IdentifiedStream<I, S> {
    id: I,
    stream: S,
}

impl<I, S, T> iced::advanced::subscription::Recipe for IdentifiedStream<I, S>
where
    I: Hash + 'static,
    S: iced::futures::Stream<Item = T> + Send + 'static,
    T: Send + 'static,
{
    type Output = T;

    fn hash(&self, state: &mut iced::advanced::subscription::Hasher) {
        TypeId::of::<I>().hash(state);
        self.id.hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: iced::advanced::subscription::EventStream,
    ) -> iced::futures::stream::BoxStream<'static, Self::Output> {
        use iced::futures::StreamExt;
        self.stream.boxed()
    }
}

fn run_with_id<I, S, T>(id: I, stream: S) -> Subscription<T>
where
    I: Hash + 'static,
    S: iced::futures::Stream<Item = T> + Send + 'static,
    T: Send + 'static,
{
    iced::advanced::subscription::from_recipe(IdentifiedStream { id, stream })
}

fn notification_expiry_subscription(
    notification_id: u64,
    delay: Duration,
) -> Subscription<Message> {
    use iced::futures::SinkExt;

    run_with_id(
        ("notification-expiry", notification_id),
        stream::channel::<Message>(
            1,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                tokio::time::sleep(delay).await;
                let _ = output.send(Message::PruneExpiredNotifications).await;
            },
        ),
    )
}

fn runtime_event_subscription() -> Subscription<Message> {
    use iced::futures::SinkExt;

    run_with_id(
        "runtime-events",
        stream::channel::<Message>(
            32,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let mut receiver = app_runtime().subscribe();
                loop {
                    match receiver.recv().await {
                        Ok(DomainEvent::JobProgress { job_id, progress }) => {
                            if output
                                .send(JobMessage::RuntimeProgress(job_id, progress).into())
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(DomainEvent::JobStateChanged { job_id, new_state }) => {
                            if output
                                .send(JobMessage::RuntimeState(job_id, new_state).into())
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            },
        ),
    )
}

fn directory_watch_subscription(pane_idx: usize, path: String) -> Subscription<Message> {
    use iced::futures::SinkExt;
    use iris_platform_integration::file_watch::WatchEvent;
    use tokio::sync::mpsc;

    run_with_id(
        ("file-watch", pane_idx, path.clone()),
        stream::channel::<Message>(
            32,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let (sender, mut receiver) = mpsc::channel(64);
                let Ok(_watch_handle) = iris_subscription_manager::watch_directory_changes(
                    std::path::Path::new(&path),
                    sender,
                ) else {
                    return;
                };

                let mut refresh_pending = false;
                loop {
                    let event = if refresh_pending {
                        tokio::select! {
                            event = receiver.recv() => event,
                            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                                refresh_pending = false;
                                let message = DirectoryMessage::RefreshRequested {
                                    pane_idx,
                                    path: path.clone(),
                                };
                                if output.send(message.into()).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                        }
                    } else {
                        receiver.recv().await
                    };

                    let Some(event) = event else {
                        break;
                    };
                    let should_refresh = matches!(
                        event,
                        WatchEvent::Created { .. }
                            | WatchEvent::Removed { .. }
                            | WatchEvent::Renamed { .. }
                            | WatchEvent::Rescan
                    );

                    if should_refresh {
                        refresh_pending = true;
                    }
                }
            },
        ),
    )
}

fn config_file_watch_subscription() -> Subscription<Message> {
    use iced::futures::SinkExt;
    use iris_platform_integration::file_watch::watch_path;
    use tokio::sync::mpsc;

    run_with_id(
        "config-file-watch",
        stream::channel::<Message>(
            8,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let Some(config_dir) = dirs_next::config_dir().map(|dir| dir.join("iris")) else {
                    return;
                };
                let config_path = config_dir.join("layout.json");
                if tokio::fs::create_dir_all(&config_dir).await.is_err() {
                    return;
                }
                let (sender, mut receiver) = mpsc::channel(16);
                let Ok(_watch_handle) = watch_path(&config_dir, sender) else {
                    return;
                };

                let mut change_pending = false;
                loop {
                    let event = if change_pending {
                        tokio::select! {
                            event = receiver.recv() => event,
                            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                                change_pending = false;
                                if output.send(Message::ConfigFileChanged).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                        }
                    } else {
                        receiver.recv().await
                    };

                    let Some(event) = event else {
                        break;
                    };
                    if watch_event_touches_path(&event, &config_path) {
                        change_pending = true;
                    }
                }
            },
        ),
    )
}

fn mounted_volumes_subscription() -> Subscription<Message> {
    use iced::futures::SinkExt;
    use tokio::sync::mpsc;

    run_with_id(
        "mounted-volumes-watch",
        stream::channel::<Message>(
            8,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let (sender, mut receiver) = mpsc::channel(4);
                let _watch_handle = iris_subscription_manager::watch_mounted_volumes(sender);

                while let Some(volumes) = receiver.recv().await {
                    if output
                        .send(DirectoryMessage::MountedVolumesChanged(volumes).into())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            },
        ),
    )
}

fn cloud_backends_subscription() -> Subscription<Message> {
    use iced::futures::SinkExt;
    use tokio::sync::mpsc;

    run_with_id(
        "cloud-backends-watch",
        stream::channel::<Message>(
            8,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let (sender, mut receiver) = mpsc::channel(4);
                let _watch_handle = iris_subscription_manager::watch_cloud_backends(sender);

                while receiver.recv().await.is_some() {
                    let items = app_runtime()
                        .restore_cloud_backends()
                        .await
                        .into_iter()
                        .map(|location| crate::state::SidebarItem {
                            name: location.name,
                            path: location.path,
                            badge: "Cl".to_string(),
                            badge_color: [0.2, 0.6, 0.85, 1.0],
                            ejectable: false,
                        })
                        .collect();
                    if output
                        .send(DirectoryMessage::CloudBackendsLoaded(items).into())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            },
        ),
    )
}

fn watch_event_touches_path(
    event: &iris_platform_integration::file_watch::WatchEvent,
    target: &std::path::Path,
) -> bool {
    match event {
        iris_platform_integration::file_watch::WatchEvent::Created { path }
        | iris_platform_integration::file_watch::WatchEvent::Modified { path }
        | iris_platform_integration::file_watch::WatchEvent::Removed { path } => path == target,
        iris_platform_integration::file_watch::WatchEvent::Renamed { from, to } => {
            from == target || to == target
        }
        iris_platform_integration::file_watch::WatchEvent::Rescan => true,
    }
}

#[cfg(target_os = "macos")]
fn macos_menu_command_subscription() -> Subscription<Message> {
    use iced::futures::SinkExt;

    run_with_id(
        "macos-app-menu",
        stream::channel::<Message>(
            16,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let mut receiver = crate::macos_menu::subscribe();
                loop {
                    match receiver.recv().await {
                        Ok(command) => {
                            if output.send(Message::AppMenuCommand(command)).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_platform_integration::file_watch::WatchEvent;
    use std::path::PathBuf;

    #[test]
    fn watch_event_touches_path_matches_direct_and_rename_events() {
        let target = PathBuf::from("/tmp/iris/layout.json");

        assert!(watch_event_touches_path(
            &WatchEvent::Modified {
                path: target.clone(),
            },
            &target,
        ));
        assert!(watch_event_touches_path(
            &WatchEvent::Renamed {
                from: PathBuf::from("/tmp/iris/layout.json.tmp"),
                to: target.clone(),
            },
            &target,
        ));
    }

    #[test]
    fn watch_event_touches_path_ignores_other_files() {
        let target = PathBuf::from("/tmp/iris/layout.json");

        assert!(!watch_event_touches_path(
            &WatchEvent::Modified {
                path: PathBuf::from("/tmp/iris/cloud-accounts.json"),
            },
            &target,
        ));
    }

    #[test]
    fn watch_event_touches_path_treats_rescan_as_match() {
        let target = PathBuf::from("/tmp/iris/layout.json");
        assert!(watch_event_touches_path(&WatchEvent::Rescan, &target));
    }
}
