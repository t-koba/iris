use crate::i18n::tr;
use crate::messages::{ConnectionMessage, DialogMessage, Message};
use crate::state::{
    CloudConnectFormState, CloudProvider, NetworkConnectFormState, NetworkProtocol,
};
use crate::views::theme;
use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Element, Length};

pub(super) fn build_connections_settings_section<'a>(
    network_form: &'a NetworkConnectFormState,
    cloud_form: &'a CloudConnectFormState,
) -> Element<'a, Message> {
    let network_can_connect = network_form.can_connect();
    let cloud_can_connect = match cloud_form.provider {
        CloudProvider::S3 => {
            !cloud_form.display_name.is_empty()
                && !cloud_form.s3_bucket.is_empty()
                && !cloud_form.s3_region.is_empty()
        }
        CloudProvider::PCloud => {
            !cloud_form.display_name.is_empty()
                && !cloud_form.pcloud_client_id.is_empty()
                && !cloud_form.pcloud_redirect_url.is_empty()
                && !cloud_form.pcloud_callback_url.is_empty()
        }
    };

    column![
        text(tr("connections-description"))
            .size(10)
            .style(theme::muted_text_style),
        container(
            column![
                text(tr("connections-network-title")).size(14),
                text(tr("connections-network-description"))
                    .size(10)
                    .style(theme::muted_text_style),
                iced::widget::rule::horizontal(1),
                build_network_connect_fields(network_form),
                row![
                    iced::widget::space::horizontal(),
                    button(text(tr("connections-connect")).size(12))
                        .on_press_maybe(
                            network_can_connect
                                .then_some(ConnectionMessage::PerformConnectNetwork.into())
                        )
                        .padding(8),
                ]
                .spacing(8),
            ]
            .spacing(12),
        )
        .padding(16)
        .style(theme::dialog_card_style),
        container(
            column![
                text(tr("connections-cloud-title")).size(14),
                text(tr("connections-cloud-description"))
                    .size(10)
                    .style(theme::muted_text_style),
                iced::widget::rule::horizontal(1),
                build_cloud_connect_fields(cloud_form),
                row![
                    iced::widget::space::horizontal(),
                    button(
                        text(match cloud_form.provider {
                            CloudProvider::S3 => tr("connections-connect"),
                            CloudProvider::PCloud => tr("connections-connect-after-auth"),
                        })
                        .size(12)
                    )
                    .on_press_maybe(
                        cloud_can_connect.then_some(ConnectionMessage::PerformConnectCloud.into())
                    )
                    .padding(8),
                ]
                .spacing(8),
            ]
            .spacing(12),
        )
        .padding(16)
        .style(theme::dialog_card_style),
    ]
    .spacing(16)
    .into()
}

fn build_network_connect_fields<'a>(
    form: &'a NetworkConnectFormState,
) -> iced::widget::Column<'a, Message> {
    let mut content = column![].spacing(12);
    let proto_picker = pick_list(
        NetworkProtocol::ALL.as_slice(),
        Some(form.protocol),
        |protocol| ConnectionMessage::SetNetworkProtocol(protocol).into(),
    )
    .text_size(11)
    .padding(4);

    content = content.push(
        row![
            text(tr("connections-protocol-label"))
                .size(12)
                .width(Length::Fixed(90.0)),
            proto_picker
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    if form.protocol == NetworkProtocol::Smb {
        content = content.push(
            row![
                text(tr("connections-smb-transport-label"))
                    .size(12)
                    .width(Length::Fixed(90.0)),
                text(tr("connections-smb-native-mode")).size(12),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        content = content.push(
            row![
                text("").width(Length::Fixed(90.0)),
                checkbox(form.smb_allow_guest)
                    .label(tr("connections-smb-allow-guest"))
                    .on_toggle(|value| {
                        ConnectionMessage::NetworkSmbAllowGuestChanged(value).into()
                    })
                    .size(14),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }

    content = content.push(
        row![
            text(tr("connections-server-label"))
                .size(12)
                .width(Length::Fixed(90.0)),
            text_input(&tr("connections-server-placeholder"), &form.server)
                .on_input(|value| ConnectionMessage::NetworkServerChanged(value).into())
                .padding(6)
                .size(12),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    if form.protocol == NetworkProtocol::Smb {
        content = content.push(
            row![
                text(tr("connections-smb-port-label"))
                    .size(12)
                    .width(Length::Fixed(90.0)),
                text_input(
                    &tr("connections-smb-port-placeholder"),
                    &form.smb_port_input
                )
                .on_input(|value| ConnectionMessage::NetworkSmbPortChanged(value).into())
                .padding(6)
                .size(12)
                .width(Length::Fixed(96.0)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }

    content = content.push(
        row![
            text(tr("connections-username-label"))
                .size(12)
                .width(Length::Fixed(90.0)),
            text_input(&tr("connections-optional-placeholder"), &form.username)
                .on_input(|value| ConnectionMessage::NetworkUsernameChanged(value).into())
                .padding(6)
                .size(12),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    if form.protocol == NetworkProtocol::Smb {
        content = content.push(
            row![
                text(tr("connections-smb-domain-label"))
                    .size(12)
                    .width(Length::Fixed(90.0)),
                text_input(&tr("connections-optional-placeholder"), &form.smb_domain)
                    .on_input(|value| ConnectionMessage::NetworkSmbDomainChanged(value).into())
                    .padding(6)
                    .size(12),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        content = content.push(
            row![
                text(tr("connections-smb-password-label"))
                    .size(12)
                    .width(Length::Fixed(90.0)),
                text_input(&tr("connections-optional-placeholder"), &form.smb_password)
                    .on_input(|value| ConnectionMessage::NetworkSmbPasswordChanged(value).into())
                    .padding(6)
                    .size(12)
                    .secure(true),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }

    let browse_btn_label = if form.browsing_shares {
        tr("connections-browsing-shares")
    } else {
        tr("connections-browse-shares")
    };
    content = content.push(
        row![
            text("").width(Length::Fixed(90.0)),
            button(text(browse_btn_label).size(11))
                .on_press_maybe(if !form.browsing_shares && !form.server.is_empty() {
                    Some(ConnectionMessage::BrowseNetworkShares.into())
                } else {
                    None
                })
                .padding([4, 12])
                .style(button::secondary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    let share_label = match form.protocol {
        NetworkProtocol::Smb => tr("connections-share-label"),
        NetworkProtocol::Nfs => tr("connections-export-label"),
    };
    let share_hint = match form.protocol {
        NetworkProtocol::Smb => tr("connections-share-placeholder"),
        NetworkProtocol::Nfs => tr("connections-export-placeholder"),
    };

    if let Some(shares) = form.available_shares.as_deref() {
        let share_options: Vec<String> = shares.to_vec();
        let selected = if form.share_path.is_empty() {
            None
        } else {
            Some(form.share_path.clone())
        };
        let share_picker = pick_list(share_options, selected, |share| {
            ConnectionMessage::NetworkShareSelected(share).into()
        })
        .text_size(11)
        .padding(4)
        .width(Length::Fill);
        content = content.push(
            row![
                text(share_label).size(12).width(Length::Fixed(90.0)),
                share_picker,
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    } else {
        content = content.push(
            row![
                text(share_label).size(12).width(Length::Fixed(90.0)),
                text_input(&share_hint, &form.share_path)
                    .on_input(|value| ConnectionMessage::NetworkShareChanged(value).into())
                    .padding(6)
                    .size(12),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }

    if let Some(error) = form.error_message.as_deref() {
        content = content.push(
            container(
                text(error)
                    .size(11)
                    .color(iced::Color::from_rgb(0.9, 0.3, 0.3)),
            )
            .padding(8)
            .style(theme::dialog_card_style),
        );
    }

    content
}

pub fn view_connect_network_dialog<'a>(form: &'a NetworkConnectFormState) -> Element<'a, Message> {
    let can_connect = form.can_connect();
    let content = column![
        text(tr("connections-network-dialog-title")).size(16),
        iced::widget::rule::horizontal(1),
        build_network_connect_fields(form),
        row![
            iced::widget::space::horizontal(),
            button(text(tr("common-cancel")).size(12))
                .on_press(DialogMessage::Close.into())
                .padding(8)
                .style(button::secondary),
            button(text(tr("connections-connect")).size(12))
                .on_press_maybe(
                    can_connect.then_some(ConnectionMessage::PerformConnectNetwork.into())
                )
                .padding(8),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .padding(theme::DIALOG_PADDING);

    container(content)
        .width(Length::Fixed(420.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

fn build_cloud_connect_fields<'a>(
    form: &'a CloudConnectFormState,
) -> iced::widget::Column<'a, Message> {
    let mut content = column![].spacing(12);
    let provider_picker = pick_list(
        CloudProvider::ALL.as_slice(),
        Some(form.provider),
        |provider| ConnectionMessage::SetCloudProvider(provider).into(),
    )
    .text_size(11)
    .padding(4);

    content = content.push(
        row![
            text(tr("connections-provider-label"))
                .size(12)
                .width(Length::Fixed(110.0)),
            provider_picker
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    content = content.push(
        row![
            text(tr("connections-display-name-label"))
                .size(12)
                .width(Length::Fixed(110.0)),
            text_input(
                &tr("connections-display-name-placeholder"),
                &form.display_name
            )
            .on_input(|value| ConnectionMessage::CloudDisplayNameChanged(value).into())
            .padding(6)
            .size(12),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    match form.provider {
        CloudProvider::S3 => {
            content = content.push(
                row![
                    text(tr("connections-s3-bucket-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(&tr("connections-s3-bucket-placeholder"), &form.s3_bucket)
                        .on_input(|value| ConnectionMessage::CloudS3BucketChanged(value).into())
                        .padding(6)
                        .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            content = content.push(
                row![
                    text(tr("connections-s3-region-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(&tr("connections-s3-region-placeholder"), &form.s3_region)
                        .on_input(|value| ConnectionMessage::CloudS3RegionChanged(value).into())
                        .padding(6)
                        .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            content = content.push(
                row![
                    text(tr("connections-s3-endpoint-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(
                        &tr("connections-optional-placeholder"),
                        &form.s3_endpoint_url
                    )
                    .on_input(|value| ConnectionMessage::CloudS3EndpointChanged(value).into())
                    .padding(6)
                    .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            content = content.push(
                row![
                    text(tr("connections-s3-prefix-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(&tr("connections-optional-placeholder"), &form.s3_prefix)
                        .on_input(|value| ConnectionMessage::CloudS3PrefixChanged(value).into())
                        .padding(6)
                        .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        CloudProvider::PCloud => {
            content = content.push(
                row![
                    text(tr("connections-pcloud-client-id-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(
                        &tr("connections-pcloud-client-id-placeholder"),
                        &form.pcloud_client_id
                    )
                    .on_input(|value| ConnectionMessage::CloudPCloudClientIdChanged(value).into())
                    .padding(6)
                    .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            content = content.push(
                row![
                    text(tr("connections-pcloud-redirect-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(
                        &tr("connections-pcloud-redirect-placeholder"),
                        &form.pcloud_redirect_url
                    )
                    .on_input(
                        |value| ConnectionMessage::CloudPCloudRedirectUrlChanged(value).into()
                    )
                    .padding(6)
                    .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            if let Some(auth_url) = form.pcloud_auth_url.as_deref() {
                content = content.push(
                    container(text(auth_url).size(10))
                        .padding(8)
                        .style(theme::dialog_card_style),
                );
            }
            content = content.push(
                row![
                    text(tr("connections-pcloud-callback-label"))
                        .size(12)
                        .width(Length::Fixed(110.0)),
                    text_input(
                        &tr("connections-pcloud-callback-placeholder"),
                        &form.pcloud_callback_url
                    )
                    .on_input(|value| ConnectionMessage::CloudPCloudCallbackChanged(value).into())
                    .padding(6)
                    .size(12),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            content = content.push(
                button(text(tr("connections-open-auth-page")).size(12))
                    .on_press(ConnectionMessage::StartCloudAuthorization.into())
                    .padding(8)
                    .style(button::secondary),
            );
        }
    }

    if let Some(error) = form.error_message.as_deref() {
        content = content.push(
            container(
                text(error)
                    .size(11)
                    .color(iced::Color::from_rgb(0.9, 0.3, 0.3)),
            )
            .padding(8)
            .style(theme::dialog_card_style),
        );
    }

    content
}

pub fn view_connect_cloud_dialog<'a>(form: &'a CloudConnectFormState) -> Element<'a, Message> {
    let can_connect = match form.provider {
        CloudProvider::S3 => {
            !form.display_name.is_empty()
                && !form.s3_bucket.is_empty()
                && !form.s3_region.is_empty()
        }
        CloudProvider::PCloud => {
            !form.display_name.is_empty()
                && !form.pcloud_client_id.is_empty()
                && !form.pcloud_redirect_url.is_empty()
                && !form.pcloud_callback_url.is_empty()
        }
    };
    let connect_label = match form.provider {
        CloudProvider::S3 => tr("connections-connect"),
        CloudProvider::PCloud => tr("connections-connect-after-auth"),
    };

    let content = column![
        text(tr("connections-cloud-dialog-title")).size(16),
        iced::widget::rule::horizontal(1),
        build_cloud_connect_fields(form),
        row![
            iced::widget::space::horizontal(),
            button(text(tr("common-cancel")).size(12))
                .on_press(DialogMessage::Close.into())
                .padding(8)
                .style(button::secondary),
            button(text(connect_label).size(12))
                .on_press_maybe(
                    can_connect.then_some(ConnectionMessage::PerformConnectCloud.into())
                )
                .padding(8),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .padding(theme::DIALOG_PADDING);

    container(content)
        .width(Length::Fixed(520.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}
