use crate::i18n::{tr, tr_with};
use crate::messages::{DialogMessage, Message};
use crate::state::{BatchRenameDialogState, BatchRenameMode};
use crate::views::theme;
use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};

pub fn view_delete_dialog(
    target_names: &[String],
    use_trash: bool,
    can_use_trash: bool,
) -> Element<'_, Message> {
    let mut content = column![].spacing(12).padding(theme::DIALOG_PADDING);

    let title = if use_trash {
        tr("delete-title-trash")
    } else {
        tr("delete-title-permanent")
    };
    content = content.push(text(title).size(16));
    content = content.push(iced::widget::rule::horizontal(1));

    let message = if target_names.len() == 1 {
        tr_with(
            "delete-confirm-one",
            &[("target", (&target_names[0]).into())],
        )
    } else {
        tr_with(
            "delete-confirm-many",
            &[("count", target_names.len().into())],
        )
    };
    content = content.push(text(message).size(13));

    if target_names.len() > 1 && target_names.len() <= 5 {
        let list_text = target_names.join("\n");
        content = content.push(
            container(text(list_text).size(11))
                .padding(8)
                .style(theme::dialog_card_style),
        );
    } else if target_names.len() > 5 {
        let preview: Vec<_> = target_names.iter().take(3).cloned().collect();
        let list_text = tr_with(
            "delete-preview-more",
            &[
                ("items", preview.join("\n").into()),
                ("count", (target_names.len() - 3).into()),
            ],
        );
        content = content.push(
            container(text(list_text).size(11))
                .padding(8)
                .style(theme::dialog_card_style),
        );
    }

    if !use_trash {
        content = content.push(text(tr("delete-permanent-warning")).size(11));
    }

    if can_use_trash {
        content = content.push(
            checkbox(use_trash)
                .label(tr("delete-trash-checkbox"))
                .on_toggle(Message::SetDeleteUseTrash)
                .text_size(11),
        );
    } else {
        content = content.push(text(tr("delete-trash-unavailable")).size(11));
    }

    let buttons = row![
        iced::widget::space::horizontal(),
        button(text(tr("common-cancel")).size(12))
            .on_press(DialogMessage::Close.into())
            .padding(8)
            .style(button::secondary),
        button(text(tr("common-delete")).size(12))
            .on_press(Message::PerformDelete)
            .padding(8)
            .style(button::danger),
    ]
    .spacing(8);

    content = content.push(buttons);

    container(content)
        .width(Length::Fixed(350.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

pub fn view_create_folder_dialog(input_value: &str) -> Element<'_, Message> {
    let mut content = column![].spacing(12).padding(theme::DIALOG_PADDING);

    content = content.push(text(tr("create-folder-title")).size(16));
    content = content.push(iced::widget::rule::horizontal(1));
    content = content.push(text(tr("create-folder-name-label")).size(12));
    content = content.push(
        text_input(&tr("create-folder-placeholder"), input_value)
            .on_input(|value| DialogMessage::InputChanged(value).into())
            .on_submit(Message::PerformCreateFolder(input_value.to_string()))
            .padding(8)
            .size(13),
    );

    let create_enabled = !input_value.is_empty();
    let buttons = row![
        iced::widget::space::horizontal(),
        button(text(tr("common-cancel")).size(12))
            .on_press(DialogMessage::Close.into())
            .padding(8)
            .style(button::secondary),
        button(text(tr("common-create")).size(12))
            .on_press_maybe(
                create_enabled.then_some(Message::PerformCreateFolder(input_value.to_string()))
            )
            .padding(8),
    ]
    .spacing(8);

    content = content.push(buttons);

    container(content)
        .width(Length::Fixed(300.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

pub fn view_batch_rename_dialog(dialog: &BatchRenameDialogState) -> Element<'_, Message> {
    let mut content = column![].spacing(12).padding(theme::DIALOG_PADDING);

    content = content.push(text(tr("batch-rename-title")).size(16));
    content = content.push(iced::widget::rule::horizontal(1));
    content = content.push(
        text(tr_with(
            "batch-rename-item-count",
            &[("count", dialog.items.len().into())],
        ))
        .size(12),
    );

    let mut mode_row = row![].spacing(6);
    for mode in BatchRenameMode::ALL {
        let style = if mode == dialog.mode {
            button::primary
        } else {
            button::secondary
        };
        mode_row = mode_row.push(
            button(text(tr(mode.i18n_key())).size(11))
                .on_press(Message::BatchRenameModeChanged(mode))
                .padding([6, 8])
                .style(style),
        );
    }
    content = content.push(mode_row);

    content = content.push(view_batch_rename_inputs(dialog));

    if let Some(error) = &dialog.error_message {
        content = content.push(text(error).size(11).color(iced::Color {
            r: 0.85,
            g: 0.22,
            b: 0.22,
            a: 1.0,
        }));
    }

    let mut preview_rows = column![].spacing(2);
    preview_rows = preview_rows.push(
        row![
            text(tr("batch-rename-preview-original"))
                .size(10)
                .width(Length::FillPortion(3)),
            text(tr("batch-rename-preview-new"))
                .size(10)
                .width(Length::FillPortion(3)),
            text(tr("batch-rename-preview-status"))
                .size(10)
                .width(Length::FillPortion(2)),
        ]
        .spacing(8),
    );
    for item in &dialog.preview {
        let status = item
            .status
            .clone()
            .unwrap_or_else(|| tr("batch-rename-status-ready"));
        let status_color = if item.blocking {
            iced::Color {
                r: 0.85,
                g: 0.22,
                b: 0.22,
                a: 1.0,
            }
        } else {
            iced::Color {
                r: 0.55,
                g: 0.55,
                b: 0.55,
                a: 1.0,
            }
        };
        preview_rows = preview_rows.push(
            row![
                text(&item.original_name)
                    .size(11)
                    .width(Length::FillPortion(3)),
                text(&item.new_name).size(11).width(Length::FillPortion(3)),
                text(status)
                    .size(11)
                    .color(status_color)
                    .width(Length::FillPortion(2)),
            ]
            .spacing(8),
        );
    }

    content = content.push(
        container(scrollable(preview_rows).height(Length::Fixed(180.0)))
            .padding(8)
            .style(theme::dialog_card_style),
    );

    let buttons = row![
        iced::widget::space::horizontal(),
        button(text(tr("common-cancel")).size(12))
            .on_press(DialogMessage::Close.into())
            .padding(8)
            .style(button::secondary),
        button(text(tr("batch-rename-apply")).size(12))
            .on_press_maybe(dialog.can_apply().then_some(Message::PerformBatchRename))
            .padding(8),
    ]
    .spacing(8);

    content = content.push(buttons);

    container(content)
        .width(Length::Fixed(560.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

fn view_batch_rename_inputs(dialog: &BatchRenameDialogState) -> Element<'_, Message> {
    match dialog.mode {
        BatchRenameMode::NumberSequence => column![
            text(tr("batch-rename-base-label")).size(12),
            text_input(
                &tr("batch-rename-base-placeholder"),
                &dialog.replacement_input
            )
            .on_input(Message::BatchRenameReplacementChanged)
            .padding(8)
            .size(12),
            row![
                column![
                    text(tr("batch-rename-start-label")).size(12),
                    text_input("", &dialog.start_input)
                        .on_input(Message::BatchRenameStartChanged)
                        .padding(8)
                        .size(12),
                ]
                .spacing(4)
                .width(Length::Fill),
                column![
                    text(tr("batch-rename-padding-label")).size(12),
                    text_input("", &dialog.padding_input)
                        .on_input(Message::BatchRenamePaddingChanged)
                        .padding(8)
                        .size(12),
                ]
                .spacing(4)
                .width(Length::Fill),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into(),
        BatchRenameMode::Replace | BatchRenameMode::Regex => {
            let pattern_label = if dialog.mode == BatchRenameMode::Regex {
                tr("batch-rename-regex-label")
            } else {
                tr("batch-rename-find-label")
            };
            column![
                text(pattern_label).size(12),
                text_input("", &dialog.pattern_input)
                    .on_input(Message::BatchRenamePatternChanged)
                    .padding(8)
                    .size(12),
                text(tr("batch-rename-replacement-label")).size(12),
                text_input("", &dialog.replacement_input)
                    .on_input(Message::BatchRenameReplacementChanged)
                    .padding(8)
                    .size(12),
            ]
            .spacing(8)
            .into()
        }
        BatchRenameMode::Date => column![
            text(tr("batch-rename-base-label")).size(12),
            text_input(
                &tr("batch-rename-base-placeholder"),
                &dialog.replacement_input
            )
            .on_input(Message::BatchRenameReplacementChanged)
            .padding(8)
            .size(12),
            text(tr("batch-rename-date-format-label")).size(12),
            text_input("", &dialog.date_format_input)
                .on_input(Message::BatchRenameDateFormatChanged)
                .padding(8)
                .size(12),
        ]
        .spacing(8)
        .into(),
    }
}

pub fn view_error_dialog<'a>(title: &'a str, message: &'a str) -> Element<'a, Message> {
    let content = column![
        text(title).size(16),
        iced::widget::rule::horizontal(1),
        container(text(message).size(12))
            .padding(8)
            .style(theme::dialog_card_style),
        row![
            iced::widget::space::horizontal(),
            button(text(tr("common-ok")).size(12))
                .on_press(DialogMessage::Close.into())
                .padding(8),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .padding(theme::DIALOG_PADDING);

    container(content)
        .width(Length::Fixed(350.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

pub fn view_remote_undo_confirmation_dialog(item_count: usize) -> Element<'static, Message> {
    let message = if item_count == 1 {
        tr("undo-remote-confirm-one")
    } else {
        tr_with("undo-remote-confirm-many", &[("count", item_count.into())])
    };
    let content = column![
        text(tr("undo-remote-confirm-title")).size(16),
        iced::widget::rule::horizontal(1),
        text(message).size(12),
        text(tr("undo-remote-confirm-warning")).size(11),
        row![
            iced::widget::space::horizontal(),
            button(text(tr("common-cancel")).size(12))
                .on_press(DialogMessage::Close.into())
                .padding(8)
                .style(button::secondary),
            button(text(tr("undo-remote-confirm-action")).size(12))
                .on_press(Message::ConfirmRemoteUndo)
                .padding(8)
                .style(button::danger),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .padding(theme::DIALOG_PADDING);

    container(content)
        .width(Length::Fixed(380.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}
