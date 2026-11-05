use crate::i18n::tr;
use crate::messages::{Message, RemoteTransportMessage, SettingsMessage};
use crate::remote_transport::{
    RemoteTransportKind, RemoteTransportMapping, RemoteTransportValidationError, SshAuthKind,
};
use crate::views::theme;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Element, Length};
use std::fmt;

pub(super) fn build_remote_transport_list<'a>(
    mappings: &'a [RemoteTransportMapping],
    remote_copy_undo_enabled: bool,
) -> Element<'a, Message> {
    let header = row![
        text(tr("settings-remote-transfer-mappings")).size(12),
        iced::widget::space::horizontal(),
        button(text(tr("settings-redetect")).size(11))
            .on_press(RemoteTransportMessage::Redetect.into())
            .padding([4, 10]),
        button(text(tr("settings-add")).size(11))
            .on_press(RemoteTransportMessage::AddNew.into())
            .padding([4, 10]),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let description = text(tr("settings-remote-transfer-list-description"))
        .size(10)
        .style(theme::muted_text_style);

    let mut list = column![].spacing(4);
    if mappings.is_empty() {
        list = list.push(
            text(tr("settings-remote-transfer-empty"))
                .size(11)
                .style(theme::muted_text_style),
        );
    } else {
        for mapping in mappings {
            list = list.push(build_remote_transport_row(mapping));
        }
    }

    let remote_undo_toggle = checkbox(remote_copy_undo_enabled)
        .label(tr("settings-remote-transfer-remote-undo-enabled"))
        .on_toggle(|enabled| SettingsMessage::SetRemoteCopyUndoEnabled(enabled).into())
        .text_size(11);
    let remote_undo_help = text(tr("settings-remote-transfer-remote-undo-help"))
        .size(10)
        .style(theme::muted_text_style);

    let scroll = scrollable(list).height(Length::Fixed(180.0));
    column![
        header,
        description,
        container(column![remote_undo_toggle, remote_undo_help].spacing(4))
            .padding(8)
            .style(theme::dialog_card_style),
        scroll
    ]
    .spacing(8)
    .into()
}

pub(super) fn build_remote_transport_row<'a>(
    mapping: &'a RemoteTransportMapping,
) -> Element<'a, Message> {
    let marker = if mapping.auto_detected { "⚙" } else { "★" };

    let title_line = row![
        text(marker).size(11),
        text(mapping.local_mount_path.clone()).size(12),
        text("  →  ").size(11).style(theme::muted_text_style),
        text(format!(
            "{}{}:{}",
            if mapping.ssh_username.is_empty() {
                String::new()
            } else {
                format!("{}@", mapping.ssh_username)
            },
            mapping.host,
            mapping.remote_base_path
        ))
        .size(11)
        .style(theme::muted_text_style),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let transport_line = row![
        text(format!(
            "{}: {}",
            tr("settings-remote-transfer-transport-label"),
            tr(mapping.transport.label_key())
        ))
        .size(10)
        .style(theme::muted_text_style),
        iced::widget::space::horizontal(),
        checkbox(mapping.enabled)
            .label(tr("settings-remote-transfer-enabled"))
            .on_toggle(move |_| RemoteTransportMessage::ToggleEnabled(mapping.id.clone()).into())
            .size(12),
        button(text(tr("common-edit")).size(10))
            .on_press(RemoteTransportMessage::BeginEdit(mapping.id.clone()).into())
            .padding([2, 8]),
        button(text(tr("common-delete")).size(10))
            .on_press(RemoteTransportMessage::Remove(mapping.id.clone()).into())
            .padding([2, 8])
            .style(button::danger),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    container(column![title_line, transport_line].spacing(3))
        .padding(6)
        .style(theme::dialog_card_style)
        .into()
}

fn labeled_input<'a>(
    label: String,
    placeholder: &'a str,
    value: &'a str,
    field: crate::remote_transport::EditField,
    input_width: Option<Length>,
) -> iced::widget::Row<'a, Message> {
    let mut input = text_input(placeholder, value)
        .on_input(move |updated| RemoteTransportMessage::EditField(field, updated).into())
        .size(11);
    if let Some(width) = input_width {
        input = input.width(width);
    }

    row![text(label).size(11).width(Length::Fixed(140.0)), input]
        .spacing(6)
        .align_y(iced::Alignment::Center)
}

fn labeled_picker<'a, T, F>(
    label: String,
    options: &'static [T],
    selected: T,
    on_select: F,
) -> iced::widget::Row<'a, Message>
where
    T: RemoteOptionLabel + 'static,
    F: Fn(T) -> Message + 'a,
{
    let options = options
        .iter()
        .copied()
        .map(LocalizedRemoteOption)
        .collect::<Vec<_>>();
    let picker = pick_list(
        options,
        Some(LocalizedRemoteOption(selected)),
        move |option| on_select(option.0),
    )
    .text_size(11)
    .padding(4);

    row![text(label).size(11).width(Length::Fixed(140.0)), picker,]
        .spacing(6)
        .align_y(iced::Alignment::Center)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LocalizedRemoteOption<T>(T);

impl<T: RemoteOptionLabel> fmt::Display for LocalizedRemoteOption<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&tr(self.0.label_key()))
    }
}

trait RemoteOptionLabel: Copy + PartialEq + Eq {
    fn label_key(self) -> &'static str;
}

impl RemoteOptionLabel for RemoteTransportKind {
    fn label_key(self) -> &'static str {
        match self {
            RemoteTransportKind::Auto => "remote-transport-kind-auto",
            RemoteTransportKind::Sftp => "remote-transport-kind-sftp",
            RemoteTransportKind::RsyncSsh => "remote-transport-kind-rsync-ssh",
            RemoteTransportKind::Disabled => "remote-transport-kind-disabled",
        }
    }
}

impl RemoteOptionLabel for SshAuthKind {
    fn label_key(self) -> &'static str {
        match self {
            SshAuthKind::Agent => "remote-transport-auth-agent",
            SshAuthKind::DefaultKey => "remote-transport-auth-default-key",
            SshAuthKind::IdentityFile => "remote-transport-auth-identity-file",
            SshAuthKind::Password => "remote-transport-auth-password",
        }
    }
}

pub(super) fn build_remote_transport_edit_form<'a>(
    buffer: &'a RemoteTransportMapping,
) -> Element<'a, Message> {
    use crate::remote_transport::EditField as Field;

    let header = row![
        text(if buffer.auto_detected {
            tr("settings-remote-transfer-edit-auto-title")
        } else {
            tr("settings-remote-transfer-edit-title")
        })
        .size(12),
        iced::widget::space::horizontal(),
    ]
    .spacing(6);

    let ssh_port_row = row![
        text(tr("settings-remote-transfer-ssh-port"))
            .size(11)
            .width(Length::Fixed(140.0)),
        text_input("22", &buffer.ssh_port.to_string())
            .on_input(|value| RemoteTransportMessage::EditField(Field::SshPort, value).into())
            .size(11)
            .width(Length::Fixed(80.0)),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let validation_hint = match buffer.validate() {
        Ok(()) => text(tr("settings-remote-transfer-input-ok"))
            .size(10)
            .style(theme::muted_text_style),
        Err(error) => text(format!("⚠ {}", remote_transport_validation_message(error)))
            .size(10)
            .color(iced::Color::from_rgb(0.95, 0.55, 0.4)),
    };

    let buttons = row![
        iced::widget::space::horizontal(),
        button(text(tr("common-cancel")).size(11))
            .on_press(RemoteTransportMessage::CancelEdit.into())
            .padding([4, 12]),
        button(text(tr("common-save")).size(11))
            .on_press(RemoteTransportMessage::SaveEdit.into())
            .padding([4, 12]),
    ]
    .spacing(6);

    column![
        header,
        labeled_input(
            tr("settings-remote-transfer-local-path"),
            "/Volumes/share",
            &buffer.local_mount_path,
            Field::LocalPath,
            None
        ),
        labeled_input(
            tr("settings-remote-transfer-host"),
            "nas.local / 192.168.1.10",
            &buffer.host,
            Field::Host,
            None
        ),
        labeled_input(
            tr("settings-remote-transfer-ssh-user"),
            "user",
            &buffer.ssh_username,
            Field::SshUser,
            None
        ),
        ssh_port_row,
        labeled_input(
            tr("settings-remote-transfer-remote-base"),
            "/volume1/share",
            &buffer.remote_base_path,
            Field::RemoteBase,
            None
        ),
        labeled_picker(
            tr("settings-remote-transfer-transport"),
            RemoteTransportKind::ALL.as_slice(),
            buffer.transport,
            |kind| RemoteTransportMessage::EditTransport(kind).into(),
        ),
        labeled_picker(
            tr("settings-remote-transfer-ssh-auth"),
            SshAuthKind::ALL.as_slice(),
            buffer.auth,
            |auth| RemoteTransportMessage::EditAuth(auth).into(),
        ),
        labeled_input(
            tr("settings-remote-transfer-identity-file"),
            "/Users/name/.ssh/id_ed25519",
            &buffer.ssh_identity_file,
            Field::SshIdentityFile,
            None
        ),
        labeled_input(
            tr("settings-remote-transfer-ssh-secret"),
            "Password/IdentityFile only",
            &buffer.ssh_password,
            Field::SshPassword,
            None
        ),
        labeled_input(
            tr("settings-remote-transfer-notes"),
            "Optional",
            &buffer.notes,
            Field::Notes,
            None
        ),
        validation_hint,
        buttons,
    ]
    .spacing(6)
    .into()
}

fn remote_transport_validation_message(error: RemoteTransportValidationError) -> String {
    tr(match error {
        RemoteTransportValidationError::EmptyLocalMountPath => {
            "remote-transfer-validation-empty-local-mount-path"
        }
        RemoteTransportValidationError::LocalMountPathNotAbsolute => {
            "remote-transfer-validation-local-mount-path-not-absolute"
        }
        RemoteTransportValidationError::EmptyHost => "remote-transfer-validation-empty-host",
        RemoteTransportValidationError::InvalidSshPort => {
            "remote-transfer-validation-invalid-ssh-port"
        }
        RemoteTransportValidationError::EmptyRemoteBasePath => {
            "remote-transfer-validation-empty-remote-base-path"
        }
        RemoteTransportValidationError::SshRemoteBasePathNotAbsolute => {
            "remote-transfer-validation-ssh-remote-base-path-not-absolute"
        }
        RemoteTransportValidationError::RemoteBasePathNotAbsolute => {
            "remote-transfer-validation-remote-base-path-not-absolute"
        }
        RemoteTransportValidationError::EmptyIdentityFile => {
            "remote-transfer-validation-empty-identity-file"
        }
        RemoteTransportValidationError::IdentityFileNotAbsolute => {
            "remote-transfer-validation-identity-file-not-absolute"
        }
        RemoteTransportValidationError::EmptySshPassword => {
            "remote-transfer-validation-empty-ssh-password"
        }
    })
}
