use crate::messages::Message;
use crate::state::ResizeTarget;
use crate::views::theme;
use iced::widget::{container, mouse_area, Space};
use iced::{Element, Length, Theme};

/// A draggable bar. The preview panel is resized vertically; every other target horizontally.
pub fn resize_handle(target: ResizeTarget, is_active: bool) -> Element<'static, Message> {
    let horizontal_bar = matches!(target, ResizeTarget::PreviewPanel);
    let thin = Length::Fixed(theme::RESIZE_HANDLE_VISUAL_WIDTH);
    let grab = Length::Fixed(theme::RESIZE_HANDLE_WIDTH);
    let (bar_width, bar_height) = if horizontal_bar {
        (Length::Fill, thin)
    } else {
        (thin, Length::Fill)
    };
    let visual_bar =
        container(Space::new().width(bar_width).height(bar_height)).style(move |theme: &Theme| {
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
    let handle_area = if horizontal_bar {
        container(visual_bar)
            .width(Length::Fill)
            .height(grab)
            .center_y(grab)
    } else {
        container(visual_bar)
            .width(grab)
            .height(Length::Fill)
            .center_x(grab)
    };
    mouse_area(handle_area)
        .on_press(Message::ResizeStart(target))
        .into()
}
