use crate::i18n::tr;
use crate::messages::{JobMessage, Message};
use crate::state::{Notification, NotificationAction, NotificationSeverity};
use crate::views::theme;
use iced::widget::{button, column, container, row, text, Column};
use iced::{Element, Length};

pub fn view_notifications(notifications: &[Notification]) -> Element<'_, Message> {
    let mut toast_column = Column::new().spacing(8).padding(16);

    // Newest first, at most five.
    for notification in notifications.iter().rev().take(5) {
        toast_column = toast_column.push(view_notification_toast(notification));
    }

    container(toast_column)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .into()
}

fn view_notification_toast(notification: &Notification) -> Element<'_, Message> {
    let mut content = column![].spacing(4);

    let badge: Element<'_, Message> = theme::severity_badge(&notification.severity);
    let title_row = row![
        badge,
        text(&notification.title).size(12),
        iced::widget::space::horizontal(),
        button(text(tr("common-close")).size(10))
            .on_press(Message::DismissNotification(notification.id))
            .padding(2)
            .style(button::text),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);

    content = content.push(title_row);

    if !notification.message.is_empty() {
        content = content.push(text(&notification.message).size(11));
    }

    if !notification.actions.is_empty() {
        let mut action_row = iced::widget::Row::new().spacing(4);
        let mut has_actions = false;
        for action in &notification.actions {
            match action {
                NotificationAction::Dismiss => {
                    // The close button is already shown.
                }
                NotificationAction::Retry(job_id) => {
                    action_row = action_row.push(
                        button(text(tr("action-retry")).size(10))
                            .on_press(JobMessage::Retry(job_id.clone()).into())
                            .padding(4),
                    );
                    has_actions = true;
                }
                NotificationAction::SkipAndContinue(job_id) => {
                    action_row = action_row.push(
                        button(text(tr("action-skip")).size(10))
                            .on_press(Message::SkipError(job_id.clone()))
                            .padding(4),
                    );
                    has_actions = true;
                }
                NotificationAction::AbortAll(job_id) => {
                    action_row = action_row.push(
                        button(text(tr("action-abort")).size(10))
                            .on_press(Message::AbortJob(job_id.clone()))
                            .padding(4)
                            .style(button::danger),
                    );
                    has_actions = true;
                }
            }
        }
        if has_actions {
            content = content.push(action_row);
        }
    }

    let style = severity_style(&notification.severity);

    container(content)
        .width(Length::Fixed(340.0))
        .padding(12)
        .style(style)
        .into()
}

fn severity_style(severity: &NotificationSeverity) -> impl Fn(&iced::Theme) -> container::Style {
    let severity = severity.clone();
    move |theme: &iced::Theme| {
        let palette = theme.extended_palette();
        let (accent, background) = match severity {
            NotificationSeverity::Info => (palette.primary.base.color, palette.primary.weak.color),
            NotificationSeverity::Warning => {
                (palette.warning.base.color, palette.warning.weak.color)
            }
            NotificationSeverity::Error => (palette.danger.base.color, palette.danger.weak.color),
        };
        container::Style {
            background: Some(iced::Background::Color(background)),
            border: iced::Border {
                color: iced::Color { a: 0.55, ..accent },
                width: 1.0,
                radius: 10.0.into(),
            },
            shadow: iced::Shadow {
                color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.22),
                offset: iced::Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
            ..Default::default()
        }
    }
}
