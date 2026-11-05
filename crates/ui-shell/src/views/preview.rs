use crate::format::truncate_with_ellipsis;
use crate::i18n::{tr, tr_with};
use crate::messages::Message;
use crate::state::{AppState, PreviewData};
use crate::views::theme;
use iced::widget::{column, container, image as iced_image, scrollable, text};
use iced::{Element, Length};

pub fn view_preview_panel(state: &AppState) -> Element<'_, Message> {
    let pane = state.active_pane();

    if pane.selected_indices.is_empty() {
        let muted = theme::muted_color(&crate::app::theme(state));
        return container(
            column![
                theme::icon("\u{F1C5}").size(36).color(muted).center(), // preview
                text(tr("preview-title")).size(11).color(muted).center(),
            ]
            .spacing(8)
            .align_x(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel_style)
        .padding(12)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    if pane.selected_indices.len() > 1 {
        let count = pane.selected_indices.len();
        return container(
            text(tr_with(
                "preview-selected-count",
                &[("count", count.into())],
            ))
            .size(11),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel_style)
        .padding(12)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    let index = match pane.selected_indices.iter().next().copied() {
        Some(i) => i,
        None => {
            return container(text("").size(11))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel_style)
                .padding(12)
                .into();
        }
    };

    let entry = match pane.entry(index) {
        Some(e) => e,
        None => {
            return container(text("").size(11))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel_style)
                .padding(12)
                .into();
        }
    };

    let muted = theme::muted_color(&crate::app::theme(state));
    let mut content = column![].spacing(4);

    let file_icon_cp = if entry.is_dir { "\u{E2C7}" } else { "\u{E873}" };
    let mut header = iced::widget::Row::new()
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .push(theme::icon(file_icon_cp).size(14))
        .push(
            text(truncate_with_ellipsis(&entry.name, 34))
                .size(11)
                .width(Length::Fill),
        );
    if let Some(size) = entry.size {
        header = header.push(text(crate::format::format_size(size)).size(10).color(muted));
    }
    content = content.push(
        container(header)
            .height(Length::Fixed(theme::PANEL_HEADER_HEIGHT))
            .width(Length::Fill)
            .clip(true),
    );

    let full_path = entry.object_ref.display_path.as_str();
    let is_active_target = state.preview_target_path.as_deref() == Some(full_path);
    let thumbnail_key = crate::state::ThumbnailCacheKey::from_entry(entry);
    let cached_thumbnail = || -> Option<Element<'_, Message>> {
        state.thumbnail_cache.get(&thumbnail_key).map(|thumbnail| {
            let thumb: Element<'_, Message> = container(
                iced_image(thumbnail.handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(iced::ContentFit::Contain),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();

            if is_active_target && state.preview_loading {
                column![thumb, text(tr("preview-loading")).size(9)]
                    .spacing(4)
                    .height(Length::Fill)
                    .into()
            } else {
                thumb
            }
        })
    };
    let loading_placeholder = || -> Element<'_, Message> {
        container(
            column![
                theme::icon("\u{E88B}").size(36).color(muted).center(), // hourglass_empty
                text(tr("preview-loading")).size(10).color(muted).center(),
            ]
            .spacing(8)
            .align_x(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    };
    let icon_fallback = || -> Element<'_, Message> {
        let icon = if entry.is_dir {
            "\u{E2C7}" // folder (Material Symbols)
        } else {
            "\u{E873}" // description (Material Symbols)
        };
        container(theme::icon(icon).size(48))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    };

    let preview_element: Element<'_, Message> = if let Some(ref data) = state.preview_data {
        match data {
            PreviewData::Image {
                handle: Some(handle),
                ..
            } => container(
                iced_image(handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(iced::ContentFit::Contain),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),

            PreviewData::Video {
                thumbnail: Some(thumbnail),
                ..
            } => container(
                iced_image(thumbnail.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(iced::ContentFit::Contain),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),

            PreviewData::Pdf {
                thumbnail: Some(thumbnail),
                page_count,
            } => {
                let img: Element<'_, Message> = container(
                    iced_image(thumbnail.clone())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .content_fit(iced::ContentFit::Contain),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into();
                if let Some(pages) = page_count {
                    column![
                        img,
                        text(tr_with("preview-pages", &[("count", (*pages).into())]))
                            .size(10)
                            .color(muted),
                    ]
                    .spacing(2)
                    .height(Length::Fill)
                    .into()
                } else {
                    img
                }
            }

            PreviewData::Image { handle: None, .. }
            | PreviewData::Video {
                thumbnail: None, ..
            }
            | PreviewData::Pdf {
                thumbnail: None, ..
            } => cached_thumbnail().unwrap_or_else(|| {
                if is_active_target && state.preview_loading {
                    loading_placeholder()
                } else {
                    icon_fallback()
                }
            }),

            PreviewData::Text {
                content: text_content,
            } => {
                let is_truncated = text_content.len() > 2000;
                let truncated = truncate_with_ellipsis(text_content, 2000);
                let mut text_col =
                    column![
                        container(text(truncated).size(10).font(iced::Font::MONOSPACE),)
                            .width(Length::Fill)
                            .padding(4)
                    ]
                    .spacing(4);
                if is_truncated {
                    text_col =
                        text_col.push(text(tr("preview-text-truncated")).size(9).color(muted));
                }
                scrollable(text_col).height(Length::Fill).into()
            }

            PreviewData::Unknown { file_type, .. } => container(
                text(tr_with(
                    "preview-unsupported",
                    &[("type", file_type.into())],
                ))
                .size(11),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),
        }
    } else if let Some(thumbnail) = cached_thumbnail() {
        thumbnail
    } else if is_active_target && state.preview_loading {
        loading_placeholder()
    } else {
        icon_fallback()
    };

    content = content.push(preview_element);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel_style)
        .padding(8)
        .into()
}
