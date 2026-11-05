use crate::messages::Message;
use crate::state::ResizeTarget;
use crate::views::theme;
use iced::widget::{container, mouse_area, Space};
use iced::{Element, Length, Theme};

/// Dragged horizontally; resizes the sidebar and the right panel.
pub fn vertical_resize_handle(target: ResizeTarget, is_active: bool) -> Element<'static, Message> {
    let visual_bar = container(
        Space::new()
            .width(Length::Fixed(theme::RESIZE_HANDLE_VISUAL_WIDTH))
            .height(Length::Fill),
    )
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        let color = if is_active {
            iced::Color {
                a: 0.82,
                ..palette.primary.base.color
            }
        } else {
            iced::Color {
                a: if palette.is_dark { 0.18 } else { 0.12 },
                ..palette.background.base.text
            }
        };
        container::Style {
            background: Some(iced::Background::Color(color)),
            ..Default::default()
        }
    });

    let handle_area = container(visual_bar)
        .width(Length::Fixed(theme::RESIZE_HANDLE_WIDTH))
        .height(Length::Fill)
        .center_x(Length::Fixed(theme::RESIZE_HANDLE_WIDTH));

    mouse_area(handle_area)
        .on_press(Message::ResizeStart(target))
        .into()
}

/// Dragged vertically; resizes the preview panel when stacked.
pub fn horizontal_resize_handle(
    target: ResizeTarget,
    is_active: bool,
) -> Element<'static, Message> {
    let visual_bar = container(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(theme::RESIZE_HANDLE_VISUAL_WIDTH)),
    )
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        let color = if is_active {
            iced::Color {
                a: 0.82,
                ..palette.primary.base.color
            }
        } else {
            iced::Color {
                a: if palette.is_dark { 0.18 } else { 0.12 },
                ..palette.background.base.text
            }
        };
        container::Style {
            background: Some(iced::Background::Color(color)),
            ..Default::default()
        }
    });

    let handle_area = container(visual_bar)
        .width(Length::Fill)
        .height(Length::Fixed(theme::RESIZE_HANDLE_WIDTH))
        .center_y(Length::Fixed(theme::RESIZE_HANDLE_WIDTH));

    mouse_area(handle_area)
        .on_press(Message::ResizeStart(target))
        .into()
}

/// Divider between the two panes in dual-pane mode.
pub fn pane_divider_handle(is_active: bool) -> Element<'static, Message> {
    vertical_resize_handle(ResizeTarget::PaneDivider, is_active)
}
