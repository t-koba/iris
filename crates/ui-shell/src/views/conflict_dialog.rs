use crate::format::{format_size, format_time};
use crate::i18n::{tr, tr_with};
use crate::messages::{ConflictMessage, DialogMessage, Message};
use crate::views::theme;
use iced::widget::{button, checkbox, column, container, row, text};
use iced::{Element, Length};
use std::time::SystemTime;

pub fn view_conflict_dialog<'a>(
    source_path: &'a str,
    destination_path: &'a str,
    source_size: u64,
    destination_size: u64,
    source_mtime: Option<SystemTime>,
    destination_mtime: Option<SystemTime>,
    remember_choice: bool,
) -> Element<'a, Message> {
    let mut content = column![].spacing(12).padding(theme::DIALOG_PADDING);

    content = content.push(text(tr("conflict-title")).size(16));
    content = content.push(iced::widget::rule::horizontal(1));

    content = content.push(text(tr("conflict-source")).size(12));
    content = content.push(
        container(
            column![
                text(truncate_path(source_path, 40)).size(11),
                text(tr_with(
                    "conflict-file-info",
                    &[
                        ("size", format_size(source_size).into()),
                        (
                            "modified",
                            (&source_mtime
                                .map(format_time)
                                .unwrap_or_else(|| tr("conflict-metadata-unavailable")))
                                .into()
                        )
                    ]
                ))
                .size(10),
            ]
            .spacing(2),
        )
        .padding(8)
        .style(theme::dialog_card_style),
    );

    content = content.push(text(tr("conflict-existing-file")).size(12));
    content = content.push(
        container(
            column![
                text(truncate_path(destination_path, 40)).size(11),
                text(tr_with(
                    "conflict-file-info",
                    &[
                        ("size", format_size(destination_size).into()),
                        (
                            "modified",
                            (&destination_mtime
                                .map(format_time)
                                .unwrap_or_else(|| tr("conflict-metadata-unavailable")))
                                .into()
                        )
                    ]
                ))
                .size(10),
            ]
            .spacing(2),
        )
        .padding(8)
        .style(theme::dialog_card_style),
    );

    let size_comparison = if source_size > destination_size {
        tr("conflict-source-larger")
    } else if source_size < destination_size {
        tr("conflict-existing-larger")
    } else {
        tr("conflict-size-same")
    };

    let time_comparison = match (source_mtime, destination_mtime) {
        (Some(s), Some(d)) if s > d => Some(tr("conflict-source-newer")),
        (Some(s), Some(d)) if s < d => Some(tr("conflict-existing-newer")),
        (Some(_), Some(_)) => Some(tr("conflict-modified-same")),
        _ => None,
    };

    if let Some(time_comparison) = time_comparison {
        content = content.push(
            text(tr_with(
                "conflict-comparison",
                &[
                    ("size", (&size_comparison).into()),
                    ("modified", (&time_comparison).into()),
                ],
            ))
            .size(10),
        );
    }

    let actions = row![
        iced::widget::space::horizontal(),
        button(text(tr("action-skip")).size(12))
            .on_press(ConflictMessage::Skip.into())
            .padding(8),
        button(text(tr("common-rename")).size(12))
            .on_press(ConflictMessage::Rename.into())
            .padding(8),
        button(text(tr("conflict-overwrite")).size(12))
            .on_press(ConflictMessage::Overwrite.into())
            .padding(8)
            .style(button::danger),
    ]
    .spacing(8);

    content = content.push(actions);

    content = content.push(
        checkbox(remember_choice)
            .label(tr("conflict-apply-to-all"))
            .on_toggle(|_| ConflictMessage::ToggleRememberChoice.into())
            .text_size(11),
    );

    if remember_choice {
        let batch_actions = row![
            iced::widget::space::horizontal(),
            button(text(tr("conflict-skip-all")).size(11))
                .on_press(ConflictMessage::SkipAll.into())
                .padding(6),
            button(text(tr("conflict-overwrite-all")).size(11))
                .on_press(ConflictMessage::OverwriteAll.into())
                .padding(6)
                .style(button::danger),
        ]
        .spacing(8);

        content = content.push(batch_actions);
    }

    content = content.push(
        row![
            iced::widget::space::horizontal(),
            button(text(tr("common-cancel")).size(11))
                .on_press(DialogMessage::Close.into())
                .padding(6)
                .style(button::secondary),
        ]
        .spacing(8),
    );

    container(content)
        .width(Length::Fixed(400.0))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        let start = &path[..max_len / 3];
        let end = &path[path.len() - max_len * 2 / 3..];
        format!("{}...{}", start, end)
    }
}
