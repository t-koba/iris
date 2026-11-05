//! Inspector panel; shows every EXIF field for images.

use crate::format::{format_size, format_time, truncate_with_ellipsis};
use crate::i18n::{tr, tr_with};
use crate::messages::Message;
use crate::state::AppState;
use crate::views::theme;
use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Length};
use iris_core_domain::types::metadata::MediaMetadata;

const PROPERTY_LABEL_WIDTH: f32 = 76.0;

pub fn view_properties_panel(state: &AppState) -> Element<'_, Message> {
    let muted = theme::muted_color(&crate::app::theme(state));
    let mut content = column![].spacing(6);

    let pane = state.active_pane();

    if pane.selected_indices.is_empty() {
        content = content.push(
            container(text(tr("properties-select-file")).size(11).color(muted))
                .width(Length::Fill)
                .center_x(Length::Fill)
                .padding(16),
        );
    } else if pane.selected_indices.len() == 1 {
        if let Some(index) = pane.selected_indices.iter().next().copied() {
            if let Some(entry) = pane.entry(index) {
                content = content.push(
                    container(text(truncate_with_ellipsis(&entry.name, 36)).size(13))
                        .height(Length::Fixed(theme::PANEL_HEADER_HEIGHT))
                        .width(Length::Fill)
                        .clip(true),
                );
                content = content.push(iced::widget::rule::horizontal(1));

                content = content.push(section_header(tr("properties-section-basic")));

                if entry.is_dir {
                    content = content.push(info_row(
                        tr("properties-kind"),
                        tr("properties-kind-folder"),
                    ));
                } else {
                    let ext = std::path::Path::new(&entry.name)
                        .extension()
                        .map(|e| e.to_string_lossy().to_uppercase())
                        .unwrap_or_default();
                    content = content.push(info_row(
                        tr("properties-kind"),
                        if ext.is_empty() {
                            tr("properties-kind-file")
                        } else {
                            tr_with(
                                "properties-kind-extension-file",
                                &[("extension", (&ext).into())],
                            )
                        },
                    ));
                }

                content = content.push(info_row_owned(
                    tr("properties-location"),
                    entry.object_ref.display_path.clone(),
                ));

                if let Some(size) = entry.size {
                    content = content.push(info_row_owned(
                        tr("properties-size"),
                        tr_with(
                            "properties-size-with-bytes",
                            &[("size", format_size(size).into()), ("bytes", size.into())],
                        ),
                    ));
                }

                if let Some(mtime) = entry.mtime {
                    content = content.push(info_row_owned(
                        tr("properties-modified-at"),
                        format_time(mtime),
                    ));
                }

                if let Some(ctime) = entry.ctime {
                    content = content.push(info_row_owned(
                        tr("properties-created-at"),
                        format_time(ctime),
                    ));
                }

                if let Some(ref data) = state.preview_data {
                    match data {
                        crate::state::PreviewData::Image {
                            width,
                            height,
                            media,
                            exif_data,
                            ..
                        } => {
                            content = content.push(iced::widget::Space::new().height(4.0));
                            content = content.push(iced::widget::rule::horizontal(1));
                            content = content.push(section_header(tr("properties-section-image")));

                            if let (Some(width), Some(height)) = (*width, *height) {
                                content = content.push(info_row_owned(
                                    tr("properties-dimensions"),
                                    tr_with(
                                        "properties-dimensions-pixels",
                                        &[("width", width.into()), ("height", height.into())],
                                    ),
                                ));
                                let gcd = gcd(width, height);
                                if let (Some(ratio_width), Some(ratio_height)) =
                                    (width.checked_div(gcd), height.checked_div(gcd))
                                {
                                    content = content.push(info_row_owned(
                                        tr("properties-aspect-ratio"),
                                        format!("{ratio_width}:{ratio_height}"),
                                    ));
                                }
                                let megapixels = (width as f64 * height as f64) / 1_000_000.0;
                                if megapixels >= 0.1 {
                                    content = content.push(info_row_owned(
                                        tr("properties-megapixels"),
                                        format!("{:.1} MP", megapixels),
                                    ));
                                }
                            }

                            for (label, value) in typed_media_rows(media) {
                                content = content.push(info_row_owned(label, value));
                            }

                            if !exif_data.is_empty() {
                                content = content.push(iced::widget::Space::new().height(4.0));
                                content = content.push(iced::widget::rule::horizontal(1));
                                content = content.push(section_header("EXIF".to_string()));

                                for (tag, value) in exif_data {
                                    content =
                                        content.push(info_row_owned(tag.clone(), value.clone()));
                                }
                            }
                        }
                        crate::state::PreviewData::Video {
                            duration_secs: Some(secs),
                            ..
                        } => {
                            content = content.push(iced::widget::Space::new().height(4.0));
                            content = content.push(iced::widget::rule::horizontal(1));
                            content = content.push(section_header(tr("properties-section-video")));
                            content = content.push(info_row_owned(
                                tr("properties-duration"),
                                format_duration(*secs),
                            ));
                        }
                        crate::state::PreviewData::Pdf {
                            page_count: Some(pages),
                            ..
                        } => {
                            content = content.push(iced::widget::Space::new().height(4.0));
                            content = content.push(iced::widget::rule::horizontal(1));
                            content = content.push(section_header(tr("properties-section-pdf")));
                            content = content
                                .push(info_row_owned(tr("properties-pages"), format!("{}", pages)));
                        }
                        crate::state::PreviewData::Text {
                            content: text_content,
                        } => {
                            content = content.push(iced::widget::Space::new().height(4.0));
                            content = content.push(iced::widget::rule::horizontal(1));
                            content = content.push(section_header(tr("properties-section-text")));
                            let line_count = text_content.lines().count();
                            let char_count = text_content.chars().count();
                            content = content.push(info_row_owned(
                                tr("properties-line-count"),
                                format!("{}", line_count),
                            ));
                            content = content.push(info_row_owned(
                                tr("properties-character-count"),
                                format!("{}", char_count),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    } else {
        let count = pane.selected_indices.len();
        content = content.push(
            text(tr_with(
                "properties-selected-count",
                &[("count", count.into())],
            ))
            .size(13),
        );
        content = content.push(iced::widget::rule::horizontal(1));

        let total_size: u64 = pane
            .selected_indices
            .iter()
            .filter_map(|&i| pane.entry(i))
            .filter_map(|e| e.size)
            .sum();

        let dir_count = pane
            .selected_indices
            .iter()
            .filter_map(|&i| pane.entry(i))
            .filter(|e| e.is_dir)
            .count();

        let file_count = count - dir_count;

        if file_count > 0 {
            content = content.push(info_row_owned(
                tr("properties-file-count"),
                format!("{}", file_count),
            ));
        }
        if dir_count > 0 {
            content = content.push(info_row_owned(
                tr("properties-folder-count"),
                format!("{}", dir_count),
            ));
        }
        if total_size > 0 {
            content = content.push(info_row_owned(
                tr("properties-total-size"),
                tr_with(
                    "properties-size-with-bytes",
                    &[
                        ("size", format_size(total_size).into()),
                        ("bytes", total_size.into()),
                    ],
                ),
            ));
        }
    }

    container(scrollable(content).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel_style)
        .padding(12)
        .into()
}

fn section_header(label: String) -> Element<'static, Message> {
    text(label).size(11).style(theme::muted_text_style).into()
}

fn info_row(label: String, value: String) -> Element<'static, Message> {
    let label_text = format!("{}:", label);

    row![
        selectable_property_text(label_text, Length::Fixed(PROPERTY_LABEL_WIDTH), true),
        selectable_property_text(value, Length::Fill, false),
    ]
    .spacing(4)
    .into()
}

fn info_row_owned(label: String, value: String) -> Element<'static, Message> {
    info_row(label, value)
}

fn selectable_property_text(
    value: String,
    width: Length,
    muted: bool,
) -> Element<'static, Message> {
    let content = text(value).size(10).width(width);

    if muted {
        content.style(theme::muted_text_style).into()
    } else {
        content.into()
    }
}

fn typed_media_rows(media: &MediaMetadata) -> Vec<(String, String)> {
    let mut rows = Vec::new();

    if let Some(value) = format_capture_time(media.capture_naive_secs, media.capture_offset_min) {
        rows.push((tr("properties-captured-at"), value));
    }

    if let Some(value) = media.camera_label() {
        rows.push((tr("properties-camera"), value));
    }

    if let Some(value) = media.lens_model.as_ref().filter(|value| !value.is_empty()) {
        rows.push((tr("properties-lens"), value.clone()));
    }

    if let Some(value) = format_gps_coordinates(media.gps_lat, media.gps_lon) {
        rows.push((tr("properties-gps"), value));
    }

    rows
}

fn format_capture_time(capture_naive_secs: Option<i64>, offset_min: Option<i16>) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp(capture_naive_secs?, 0)?;
    let mut value = dt.format("%Y-%m-%d %H:%M:%S").to_string();
    if let Some(offset_min) = offset_min {
        value.push(' ');
        value.push_str(&format_utc_offset(offset_min));
    }
    Some(value)
}

fn format_utc_offset(offset_min: i16) -> String {
    let sign = if offset_min < 0 { '-' } else { '+' };
    let abs = offset_min.unsigned_abs();
    format!("UTC{}{:02}:{:02}", sign, abs / 60, abs % 60)
}

fn format_gps_coordinates(lat: Option<f64>, lon: Option<f64>) -> Option<String> {
    Some(format!("{:.6}, {:.6}", lat?, lon?))
}

fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_time_formats_with_offset_suffix() {
        assert_eq!(
            format_capture_time(Some(1_704_110_400), Some(540)).as_deref(),
            Some("2024-01-01 12:00:00 UTC+09:00")
        );
    }

    #[test]
    fn camera_label_avoids_duplicate_make_prefix() {
        let media = MediaMetadata {
            camera_make: Some("Canon".to_string()),
            camera_model: Some("Canon EOS R5".to_string()),
            ..MediaMetadata::default()
        };
        assert_eq!(media.camera_label().as_deref(), Some("Canon EOS R5"));
    }

    #[test]
    fn gps_coordinates_require_both_axes() {
        assert_eq!(format_gps_coordinates(Some(35.658581), None), None);
        assert_eq!(
            format_gps_coordinates(Some(35.658581), Some(139.745433)).as_deref(),
            Some("35.658581, 139.745433")
        );
    }
}
