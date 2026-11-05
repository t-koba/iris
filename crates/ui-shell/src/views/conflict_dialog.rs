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
    content = content.push(file_card(source_path, source_size, source_mtime));

    content = content.push(text(tr("conflict-existing-file")).size(12));
    content = content.push(file_card(
        destination_path,
        destination_size,
        destination_mtime,
    ));

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

/// Keeps the first third and the last two thirds of `max_len` characters. Counts characters,
/// not bytes: slicing a Japanese path at a byte offset would panic.
fn truncate_path(path: &str, max_len: usize) -> String {
    let char_count = path.chars().count();
    if char_count <= max_len {
        return path.to_string();
    }
    let head = max_len / 3;
    let tail = max_len * 2 / 3;
    let start = path.chars().take(head).collect::<String>();
    let end = path.chars().skip(char_count - tail).collect::<String>();
    format!("{start}...{end}")
}

/// Path, size and modification time of one side of the conflict.
fn file_card<'a>(
    path: &str,
    size: u64,
    mtime: Option<std::time::SystemTime>,
) -> Element<'a, Message> {
    let modified = mtime
        .map(format_time)
        .unwrap_or_else(|| tr("conflict-metadata-unavailable"));
    container(
        column![
            text(truncate_path(path, 40)).size(11),
            text(tr_with(
                "conflict-file-info",
                &[
                    ("size", format_size(size).into()),
                    ("modified", modified.into()),
                ],
            ))
            .size(10),
        ]
        .spacing(2),
    )
    .padding(8)
    .style(theme::dialog_card_style)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_path_cuts_on_character_boundaries() {
        let path = format!("/Users/me/{}/photo.jpg", "\u{5199}\u{771f}".repeat(40));
        let truncated = truncate_path(&path, 30);
        assert_eq!(truncated.chars().count(), 33);
        assert!(truncated.starts_with("/Users/me/"));
        assert!(truncated.ends_with("/photo.jpg"));
        assert_eq!(truncate_path("/short", 30), "/short");
    }
}
