use crate::i18n::tr;
use crate::messages::{DirectoryMessage, Message};
use crate::runtime::{app_runtime, CloudSidebarLocation};
use crate::state::{AppState, PaneLoadState, SidebarGroup, SidebarItem};
use crate::update::helpers::{load_directory_progressively, refresh_breadcrumbs};
use iced::Task;
use std::path::PathBuf;

pub fn init() -> (AppState, Task<Message>) {
    let _ = app_runtime();
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let path_str = home.to_string_lossy().to_string();

    let mut state = AppState::default();
    state.pane_mut(0).current_path = path_str.clone();
    state.pane_mut(0).load_state = PaneLoadState::LoadingInitial;
    state.sidebar_visible = true;
    state.sidebar_groups = init_sidebar_groups();
    state.pane_mut(0).push_to_history(path_str.clone());
    refresh_breadcrumbs(&mut state);

    let dir_task = load_directory_progressively(path_str, state.show_hidden_files);
    let config_task = Task::perform(crate::config::load_config(), Message::LayoutConfigLoaded);
    let restore_jobs_task = Task::perform(async move { app_runtime().restore_jobs() }, |jobs| {
        DirectoryMessage::RestoredJobsLoaded(jobs).into()
    });
    let restore_cloud_task = Task::perform(
        async move {
            app_runtime()
                .restore_cloud_backends()
                .await
                .into_iter()
                .map(cloud_sidebar_item)
                .collect::<Vec<_>>()
        },
        |items| DirectoryMessage::CloudBackendsLoaded(items).into(),
    );
    let system_theme_task = iced::system::theme().map(Message::SystemThemeChanged);

    (
        state,
        Task::batch([
            dir_task,
            config_task,
            restore_jobs_task,
            restore_cloud_task,
            system_theme_task,
        ]),
    )
}

fn init_sidebar_groups() -> Vec<SidebarGroup> {
    let mut groups = Vec::new();
    let iris_theme = crate::views::theme::iris_dark();
    let palette = iris_theme.extended_palette();
    let mut places = Vec::new();

    if let Some(home) = dirs_next::home_dir() {
        places.push(SidebarItem {
            name: tr("sidebar-place-home"),
            path: home.to_string_lossy().to_string(),
            badge: "~".to_string(),
            badge_color: color_to_array(palette.primary.base.color),
            ejectable: false,
        });
    }
    if let Some(path) = dirs_next::desktop_dir() {
        places.push(SidebarItem {
            name: tr("sidebar-place-desktop"),
            path: path.to_string_lossy().to_string(),
            badge: "Dt".to_string(),
            badge_color: color_to_array(palette.secondary.base.color),
            ejectable: false,
        });
    }
    if let Some(path) = dirs_next::document_dir() {
        places.push(SidebarItem {
            name: tr("sidebar-place-documents"),
            path: path.to_string_lossy().to_string(),
            badge: "Dc".to_string(),
            badge_color: color_to_array(palette.primary.weak.color),
            ejectable: false,
        });
    }
    if let Some(path) = dirs_next::download_dir() {
        places.push(SidebarItem {
            name: tr("sidebar-place-downloads"),
            path: path.to_string_lossy().to_string(),
            badge: "Dl".to_string(),
            badge_color: [0.3, 0.7, 0.4, 1.0],
            ejectable: false,
        });
    }
    if let Some(path) = dirs_next::picture_dir() {
        places.push(SidebarItem {
            name: tr("sidebar-place-pictures"),
            path: path.to_string_lossy().to_string(),
            badge: "Px".to_string(),
            badge_color: color_to_array(palette.secondary.weak.color),
            ejectable: false,
        });
    }
    if let Some(path) = dirs_next::video_dir() {
        places.push(SidebarItem {
            name: tr("sidebar-place-movies"),
            path: path.to_string_lossy().to_string(),
            badge: "Vd".to_string(),
            badge_color: color_to_array(palette.danger.weak.color),
            ejectable: false,
        });
    }

    groups.push(SidebarGroup {
        name: tr("sidebar-group-places"),
        collapsed: false,
        items: places,
        system: true,
    });

    let mut volumes = Vec::new();
    match iris_platform_integration::mount::list_mounted_volumes() {
        Ok(mounted_volumes) => {
            for volume in mounted_volumes {
                volumes.push(SidebarItem {
                    name: volume.name,
                    path: volume.mount_path.to_string_lossy().to_string(),
                    badge: "V".to_string(),
                    badge_color: [0.5, 0.5, 0.5, 1.0],
                    ejectable: volume.ejectable,
                });
            }
        }
        Err(error) => {
            tracing::warn!("failed to list mounted volumes: {}", error);
        }
    }
    groups.push(SidebarGroup {
        name: tr("sidebar-group-volumes"),
        collapsed: false,
        items: volumes,
        system: true,
    });

    groups
}

fn color_to_array(color: iced::Color) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

fn cloud_sidebar_item(location: CloudSidebarLocation) -> SidebarItem {
    SidebarItem {
        name: location.name,
        path: location.path,
        badge: "Cl".to_string(),
        badge_color: [0.2, 0.6, 0.85, 1.0],
        ejectable: false,
    }
}
