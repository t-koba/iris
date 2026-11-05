use super::helpers::navigate_to_path;
use super::pane::navigate_pane_to_path;
use crate::i18n::{tr, tr_with};
use crate::messages::{ConnectionMessage, Message};
use crate::runtime::{app_runtime, CloudSidebarLocation, SmbNativeConnectRequest};
use crate::state::{
    parse_smb_port_input, AppState, CloudConnectFormState, CloudProvider, DialogState,
    NetworkConnectFormState, NetworkProtocol, NotificationAction, NotificationSeverity,
    SidebarGroup, SidebarItem,
};
use iced::Task;
use iris_platform_integration::mount::MountedVolume;
use std::path::Path;

pub fn handle(state: &mut AppState, message: ConnectionMessage) -> Task<Message> {
    match message {
        ConnectionMessage::SetNetworkProtocol(protocol) => {
            set_network_protocol(state, protocol);
            Task::none()
        }
        ConnectionMessage::SetCloudProvider(provider) => {
            set_cloud_provider(state, provider);
            Task::none()
        }
        ConnectionMessage::NetworkServerChanged(value) => {
            network_server_changed(state, value);
            Task::none()
        }
        ConnectionMessage::NetworkSmbPortChanged(value) => {
            network_smb_port_changed(state, value);
            Task::none()
        }
        ConnectionMessage::NetworkShareChanged(value) => {
            network_share_changed(state, value);
            Task::none()
        }
        ConnectionMessage::NetworkUsernameChanged(value) => {
            network_username_changed(state, value);
            Task::none()
        }
        ConnectionMessage::NetworkSmbDomainChanged(value) => {
            network_smb_domain_changed(state, value);
            Task::none()
        }
        ConnectionMessage::NetworkSmbPasswordChanged(value) => {
            network_smb_password_changed(state, value);
            Task::none()
        }
        ConnectionMessage::NetworkSmbAllowGuestChanged(value) => {
            if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
                form.smb_allow_guest = value;
                form.error_message = None;
            }
            Task::none()
        }
        ConnectionMessage::CloudDisplayNameChanged(value) => {
            cloud_display_name_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudS3BucketChanged(value) => {
            cloud_s3_bucket_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudS3RegionChanged(value) => {
            cloud_s3_region_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudS3EndpointChanged(value) => {
            cloud_s3_endpoint_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudS3PrefixChanged(value) => {
            cloud_s3_prefix_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudPCloudClientIdChanged(value) => {
            cloud_pcloud_client_id_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudPCloudRedirectUrlChanged(value) => {
            cloud_pcloud_redirect_url_changed(state, value);
            Task::none()
        }
        ConnectionMessage::CloudPCloudCallbackChanged(value) => {
            cloud_pcloud_callback_changed(state, value);
            Task::none()
        }
        ConnectionMessage::StartCloudAuthorization => {
            start_cloud_authorization(state);
            Task::none()
        }
        ConnectionMessage::BrowseNetworkShares => browse_network_shares(state),
        ConnectionMessage::NetworkSharesLoaded(result) => network_shares_loaded(state, result),
        ConnectionMessage::NetworkShareSelected(share) => {
            network_share_selected(state, share);
            Task::none()
        }
        ConnectionMessage::PerformConnectNetwork => perform_connect_network(state),
        ConnectionMessage::PerformConnectCloud => perform_connect_cloud(state),
        ConnectionMessage::ConnectNetworkCompleted(result) => {
            connect_network_completed(state, result)
        }
        ConnectionMessage::ConnectCloudCompleted(result) => connect_cloud_completed(state, result),
        ConnectionMessage::UnmountVolume(path) => {
            let unmount_path = path;
            Task::perform(
                tokio::task::spawn_blocking(move || {
                    iris_platform_integration::mount::unmount_volume(Path::new(&unmount_path))
                        .map(|_| unmount_path)
                        .map_err(|error| error.to_string())
                }),
                |result| {
                    ConnectionMessage::UnmountVolumeCompleted(result.unwrap_or_else(|error| {
                        Err(tr_with(
                            "connections-internal-error",
                            &[("error", error.to_string().into())],
                        ))
                    }))
                    .into()
                },
            )
        }
        ConnectionMessage::UnmountVolumeCompleted(Ok(path)) => {
            app_runtime().unregister_mount(Path::new(&path));
            for group in &mut state.sidebar_groups {
                group.items.retain(|item| item.path != path);
            }
            leave_unmounted_volume(state, Path::new(&path))
        }
        ConnectionMessage::UnmountVolumeCompleted(Err(message)) => {
            // The volume is still mounted, so the listings stay as they are.
            state.add_notification(
                NotificationSeverity::Error,
                tr("notification-eject-failed-title"),
                message,
                vec![NotificationAction::Dismiss],
            );
            Task::none()
        }
    }
}

/// Moves every tab that shows the ejected volume to the home folder; active tabs reload now,
/// the others when they are next shown.
fn leave_unmounted_volume(state: &mut AppState, mount_path: &Path) -> Task<Message> {
    let fallback = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
        .to_string_lossy()
        .into_owned();
    let mut active_panes = Vec::new();
    for (pane_idx, is_active, tab) in state.all_tabs_mut() {
        if !Path::new(&tab.current_path).starts_with(mount_path) {
            continue;
        }
        if is_active {
            active_panes.push(pane_idx);
        } else {
            tab.current_path = fallback.clone();
            tab.push_to_history(fallback.clone());
        }
    }
    Task::batch(
        active_panes
            .into_iter()
            .map(|pane_idx| navigate_pane_to_path(state, pane_idx, fallback.clone()))
            .collect::<Vec<_>>(),
    )
}

pub fn set_network_protocol(state: &mut AppState, protocol: NetworkProtocol) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.protocol = protocol;
        form.available_shares = None;
        form.error_message = None;
    }
}

pub fn set_cloud_provider(state: &mut AppState, provider: CloudProvider) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.provider = provider;
        form.error_message = None;
    }
}

pub fn network_server_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.server = value;
        form.available_shares = None;
        form.error_message = None;
    }
}

pub fn network_smb_port_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.smb_port_input = value;
        form.error_message = None;
    }
}

pub fn network_share_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.share_path = value;
        form.error_message = None;
    }
}

pub fn network_username_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.username = value;
        form.available_shares = None;
        form.error_message = None;
    }
}

pub fn browse_network_shares(state: &mut AppState) -> Task<Message> {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        if form.server.is_empty() {
            form.error_message = Some(tr("connections-server-required"));
            return Task::none();
        }

        form.browsing_shares = true;
        form.error_message = None;
        let protocol = form.protocol;
        let server = form.server.clone();
        let username = form.username.clone();
        return Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    iris_platform_integration::mount::list_network_shares(
                        to_mount_protocol(protocol),
                        &server,
                        if username.is_empty() {
                            None
                        } else {
                            Some(username.as_str())
                        },
                    )
                    .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| {
                    tr_with(
                        "connections-internal-error",
                        &[("error", error.to_string().into())],
                    )
                })?
            },
            |result| ConnectionMessage::NetworkSharesLoaded(result).into(),
        );
    }

    Task::none()
}

pub fn network_shares_loaded(
    state: &mut AppState,
    result: Result<Vec<String>, String>,
) -> Task<Message> {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.browsing_shares = false;
        match result {
            Ok(shares) => {
                if shares.is_empty() {
                    form.error_message = Some(tr("connections-shares-empty"));
                    form.available_shares = None;
                } else {
                    if form.share_path.is_empty() {
                        form.share_path = shares[0].clone();
                    }
                    form.available_shares = Some(shares);
                    form.error_message = None;
                }
            }
            Err(error) => {
                form.error_message = Some(tr_with(
                    "connections-browse-shares-failed",
                    &[("error", (&error).into())],
                ));
                form.available_shares = None;
            }
        }
    }

    Task::none()
}

pub fn network_share_selected(state: &mut AppState, share: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.share_path = share;
        form.error_message = None;
    }
}

pub fn network_smb_domain_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.smb_domain = value;
        form.error_message = None;
    }
}

pub fn network_smb_password_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
        form.smb_password = value;
        form.error_message = None;
    }
}

pub fn cloud_display_name_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.display_name = value;
        form.error_message = None;
    }
}

pub fn cloud_s3_bucket_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.s3_bucket = value;
        form.error_message = None;
    }
}

pub fn cloud_s3_region_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.s3_region = value;
        form.error_message = None;
    }
}

pub fn cloud_s3_endpoint_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.s3_endpoint_url = value;
        form.error_message = None;
    }
}

pub fn cloud_s3_prefix_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.s3_prefix = value;
        form.error_message = None;
    }
}

pub fn cloud_pcloud_client_id_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.pcloud_client_id = value;
        form.error_message = None;
    }
}

pub fn cloud_pcloud_redirect_url_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.pcloud_redirect_url = value;
        form.error_message = None;
    }
}

pub fn cloud_pcloud_callback_changed(state: &mut AppState, value: String) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        form.pcloud_callback_url = value;
        form.error_message = None;
    }
}

pub fn start_cloud_authorization(state: &mut AppState) {
    if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
        if form.provider == CloudProvider::PCloud {
            match app_runtime()
                .begin_pcloud_authorization(&form.pcloud_client_id, &form.pcloud_redirect_url)
            {
                Ok(url) => {
                    form.pcloud_auth_url = Some(url.clone());
                    form.error_message = iris_platform_integration::default_app::open_url(&url)
                        .err()
                        .map(|error| error.to_string());
                }
                Err(error) => {
                    form.error_message = Some(error);
                }
            }
        }
    }
}

pub fn perform_connect_network(state: &mut AppState) -> Task<Message> {
    if let Some(form) = active_network_form(&state.active_dialog) {
        let protocol = form.protocol;
        let server = form.server.clone();
        let share_name = form.share_path.clone();
        let username = form.username.clone();
        let display_name = format_network_display_name(&protocol, &server, &share_name);

        if protocol == NetworkProtocol::Smb {
            let Some(port) = parse_smb_port_input(&form.smb_port_input) else {
                if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
                    form.error_message = Some(tr("connections-smb-port-invalid"));
                }
                return Task::none();
            };
            if !form.smb_credentials_complete() {
                if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
                    form.error_message = Some(tr("connections-smb-credentials-incomplete"));
                }
                return Task::none();
            }
            let domain = form.smb_domain.clone();
            let password = form.smb_password.clone();
            let allow_guest = form.smb_allow_guest;
            return Task::perform(
                async move {
                    app_runtime()
                        .connect_smb_native(SmbNativeConnectRequest {
                            display_name,
                            server,
                            port,
                            share_name,
                            username: (!username.trim().is_empty()).then_some(username),
                            domain: (!domain.trim().is_empty()).then_some(domain),
                            password: (!password.is_empty()).then_some(password),
                            allow_guest,
                        })
                        .await
                        .map(network_sidebar_item)
                },
                |result| ConnectionMessage::ConnectNetworkCompleted(result).into(),
            );
        }

        return Task::perform(
            async move {
                let mounted = tokio::task::spawn_blocking(move || {
                    iris_platform_integration::mount::mount_network_share(
                        &iris_platform_integration::mount::NetworkMountRequest {
                            protocol: to_mount_protocol(protocol),
                            server,
                            share_path: share_name.clone(),
                            username: if username.is_empty() {
                                None
                            } else {
                                Some(username)
                            },
                        },
                    )
                })
                .await
                .map_err(|error| {
                    tr_with(
                        "connections-internal-error",
                        &[("error", error.to_string().into())],
                    )
                })?
                .map_err(|error| error.to_string())?;
                let _ = app_runtime()
                    .register_network_mount(&mounted.mount_path, to_mount_protocol(protocol));

                Ok(SidebarItem {
                    name: display_name,
                    path: mounted.mount_path.to_string_lossy().to_string(),
                    badge: "N".to_string(),
                    badge_color: [0.5, 0.5, 0.5, 1.0],
                    ejectable: false,
                })
            },
            |result| ConnectionMessage::ConnectNetworkCompleted(result).into(),
        );
    }

    Task::none()
}

fn network_sidebar_item(location: CloudSidebarLocation) -> SidebarItem {
    SidebarItem {
        name: location.name,
        path: location.path,
        badge: "N".to_string(),
        badge_color: [0.5, 0.5, 0.5, 1.0],
        ejectable: false,
    }
}

pub fn perform_connect_cloud(state: &mut AppState) -> Task<Message> {
    if let Some(form) = active_cloud_form(&state.active_dialog) {
        let provider = form.provider;
        let display_name = form.display_name.clone();
        let s3_bucket = form.s3_bucket.clone();
        let s3_region = form.s3_region.clone();
        let s3_endpoint_url = form.s3_endpoint_url.clone();
        let s3_prefix = form.s3_prefix.clone();
        let pcloud_callback_url = form.pcloud_callback_url.clone();
        return Task::perform(
            async move {
                match provider {
                    CloudProvider::S3 => app_runtime()
                        .connect_s3_account(
                            display_name,
                            s3_bucket,
                            s3_region,
                            (!s3_endpoint_url.is_empty()).then_some(s3_endpoint_url),
                            (!s3_prefix.is_empty()).then_some(s3_prefix),
                        )
                        .await
                        .map(cloud_sidebar_item),
                    CloudProvider::PCloud => app_runtime()
                        .connect_pcloud_account(display_name, pcloud_callback_url)
                        .await
                        .map(cloud_sidebar_item),
                }
            },
            |result| ConnectionMessage::ConnectCloudCompleted(result).into(),
        );
    }

    Task::none()
}

pub fn connect_network_completed(
    state: &mut AppState,
    result: Result<SidebarItem, String>,
) -> Task<Message> {
    match result {
        Ok(item) => {
            add_item_to_group(state, &tr("sidebar-group-volumes"), item.clone());
            state.active_dialog = None;
            navigate_to_path(state, item.path)
        }
        Err(message) => {
            if let Some(form) = active_network_form_mut(&mut state.active_dialog) {
                form.error_message = Some(message);
            }
            Task::none()
        }
    }
}

pub fn connect_cloud_completed(
    state: &mut AppState,
    result: Result<SidebarItem, String>,
) -> Task<Message> {
    match result {
        Ok(item) => {
            add_item_to_group(state, &tr("sidebar-group-cloud"), item.clone());
            state.active_dialog = None;
            navigate_to_path(state, item.path)
        }
        Err(message) => {
            if let Some(form) = active_cloud_form_mut(&mut state.active_dialog) {
                form.error_message = Some(message);
            }
            Task::none()
        }
    }
}

pub fn sync_mounted_volume_group(state: &mut AppState, volumes: Vec<MountedVolume>) {
    let items = volumes
        .into_iter()
        .map(|volume| SidebarItem {
            name: volume.name,
            path: volume.mount_path.to_string_lossy().to_string(),
            badge: "V".to_string(),
            badge_color: [0.5, 0.5, 0.5, 1.0],
            ejectable: volume.ejectable,
        })
        .collect();

    if let Some(group) = state
        .sidebar_groups
        .iter_mut()
        .find(|group| group.system && group.name == tr("sidebar-group-volumes"))
    {
        group.items = items;
        return;
    }

    let insert_at = state
        .sidebar_groups
        .iter()
        .position(|group| !group.system)
        .unwrap_or(state.sidebar_groups.len());
    state.sidebar_groups.insert(
        insert_at,
        SidebarGroup {
            name: tr("sidebar-group-volumes"),
            collapsed: false,
            items,
            system: true,
        },
    );
}

pub fn cloud_backends_loaded(state: &mut AppState, items: Vec<SidebarItem>) {
    let index = state
        .sidebar_groups
        .iter()
        .position(|group| group.name == tr("sidebar-group-cloud"));
    let index = match index {
        Some(index) => index,
        None => {
            state.sidebar_groups.push(SidebarGroup {
                name: tr("sidebar-group-cloud"),
                collapsed: false,
                items: Vec::new(),
                system: false,
            });
            state.sidebar_groups.len() - 1
        }
    };

    state.sidebar_groups[index].items = items;
}

pub fn add_item_to_group(state: &mut AppState, group_name: &str, item: SidebarItem) {
    add_item_to_group_batch(state, group_name, vec![item]);
}

fn add_item_to_group_batch(state: &mut AppState, group_name: &str, items: Vec<SidebarItem>) {
    let index = state
        .sidebar_groups
        .iter()
        .position(|group| group.name == group_name);
    let index = match index {
        Some(index) => index,
        None => {
            state.sidebar_groups.push(SidebarGroup {
                name: group_name.to_string(),
                collapsed: false,
                items: Vec::new(),
                system: false,
            });
            state.sidebar_groups.len() - 1
        }
    };

    for item in items {
        let already_exists = state.sidebar_groups[index]
            .items
            .iter()
            .any(|existing| existing.path == item.path);
        if !already_exists {
            state.sidebar_groups[index].items.push(item);
        }
    }
}

fn active_network_form(dialog: &Option<DialogState>) -> Option<&NetworkConnectFormState> {
    match dialog {
        Some(DialogState::Settings { network_form, .. }) => Some(network_form),
        Some(DialogState::ConnectNetwork { form }) => Some(form),
        _ => None,
    }
}

fn active_network_form_mut(
    dialog: &mut Option<DialogState>,
) -> Option<&mut NetworkConnectFormState> {
    match dialog {
        Some(DialogState::Settings { network_form, .. }) => Some(network_form),
        Some(DialogState::ConnectNetwork { form }) => Some(form),
        _ => None,
    }
}

fn active_cloud_form(dialog: &Option<DialogState>) -> Option<&CloudConnectFormState> {
    match dialog {
        Some(DialogState::Settings { cloud_form, .. }) => Some(cloud_form.as_ref()),
        Some(DialogState::ConnectCloud { form }) => Some(form),
        _ => None,
    }
}

fn active_cloud_form_mut(dialog: &mut Option<DialogState>) -> Option<&mut CloudConnectFormState> {
    match dialog {
        Some(DialogState::Settings { cloud_form, .. }) => Some(cloud_form.as_mut()),
        Some(DialogState::ConnectCloud { form }) => Some(form),
        _ => None,
    }
}

fn format_network_display_name(protocol: &NetworkProtocol, server: &str, share: &str) -> String {
    let share_clean = share.trim_start_matches('/');
    match protocol {
        NetworkProtocol::Smb => format!("{} ({})", share_clean, server),
        NetworkProtocol::Nfs => format!("{}:{}", server, share_clean),
    }
}

fn to_mount_protocol(
    protocol: NetworkProtocol,
) -> iris_platform_integration::mount::NetworkMountProtocol {
    match protocol {
        NetworkProtocol::Smb => iris_platform_integration::mount::NetworkMountProtocol::Smb,
        NetworkProtocol::Nfs => iris_platform_integration::mount::NetworkMountProtocol::Nfs,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PaneLoadState;

    fn home() -> String {
        dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/"))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn ejecting_a_volume_moves_every_tab_inside_it_home() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/Volumes/Card/DCIM".to_string();
        state.open_tab(0);
        state.pane_mut(0).current_path = "/Users/me".to_string();
        state.pane_mut(1).current_path = "/Volumes/Card".to_string();
        state.open_tab(1);
        // Shares a string prefix with the ejected volume but is a different volume.
        state.pane_mut(1).current_path = "/Volumes/Card2".to_string();
        let _ = state.switch_tab(1, 0);

        let _ = handle(
            &mut state,
            ConnectionMessage::UnmountVolumeCompleted(Ok("/Volumes/Card".to_string())),
        );

        let pane0 = state.pane_tabs(0).tabs();
        assert_eq!(
            pane0[0].current_path,
            home(),
            "inactive tab leaves the volume"
        );
        assert_eq!(pane0[1].current_path, "/Users/me");
        let pane1 = state.pane_tabs(1).tabs();
        assert_eq!(
            pane1[0].current_path,
            home(),
            "active tab of pane 1 leaves too"
        );
        assert_eq!(pane1[1].current_path, "/Volumes/Card2");
    }

    #[test]
    fn failed_eject_reports_a_notification_and_keeps_the_listing() {
        let mut state = AppState::default();
        state.pane_mut(0).current_path = "/Volumes/Card".to_string();
        state.pane_mut(0).load_state = PaneLoadState::Loaded;

        let _ = handle(
            &mut state,
            ConnectionMessage::UnmountVolumeCompleted(Err("busy".to_string())),
        );

        assert_eq!(state.pane(0).load_state, PaneLoadState::Loaded);
        assert_eq!(state.notifications.len(), 1);
        assert_eq!(state.notifications[0].message, "busy");
    }
}
