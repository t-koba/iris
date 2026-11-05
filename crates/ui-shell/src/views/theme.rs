//! Shared theme constants and style functions.

use iced::widget::{button, container, text, Text};
use iced::{Border, Color, Element, Font, Length, Theme};
use std::sync::LazyLock;

/// The product palette deliberately follows the cool blue of the Iris icon.
/// Keeping this here (instead of relying on iced's generic Light/Dark themes)
/// makes every built-in control share the same visual language.
static IRIS_LIGHT: LazyLock<Theme> = LazyLock::new(|| {
    Theme::custom(
        "Iris Light",
        iced::theme::Palette {
            background: Color::from_rgb8(247, 249, 252),
            text: Color::from_rgb8(24, 34, 51),
            primary: Color::from_rgb8(39, 119, 217),
            success: Color::from_rgb8(30, 137, 91),
            warning: Color::from_rgb8(181, 112, 20),
            danger: Color::from_rgb8(196, 55, 65),
        },
    )
});

static IRIS_DARK: LazyLock<Theme> = LazyLock::new(|| {
    Theme::custom(
        "Iris Dark",
        iced::theme::Palette {
            background: Color::from_rgb8(20, 26, 35),
            text: Color::from_rgb8(232, 238, 247),
            primary: Color::from_rgb8(95, 168, 255),
            success: Color::from_rgb8(76, 195, 138),
            warning: Color::from_rgb8(244, 184, 96),
            danger: Color::from_rgb8(255, 107, 114),
        },
    )
});

pub fn iris_light() -> Theme {
    IRIS_LIGHT.clone()
}

pub fn iris_dark() -> Theme {
    IRIS_DARK.clone()
}

/// Material Symbols Outlined; icons are addressed by private-use code points (U+E000-).
pub const ICON_FONT: Font = Font::with_name("Material Symbols Outlined");

pub fn icon(label: &str) -> Text<'_> {
    text(label).font(ICON_FONT).line_height(1.0)
}

pub const ZONE_PADDING: f32 = 10.0;
pub const GROUP_SPACING: f32 = 12.0;
pub const SECTION_SPACING: f32 = 16.0;
pub const DIALOG_PADDING: f32 = 24.0;
pub const DIALOG_BORDER_RADIUS: f32 = 12.0;
pub const LIST_ROW_PADDING_V: u16 = 3;
pub const LIST_ROW_PADDING_H: u16 = 8;

/// Clickable width.
pub const RESIZE_HANDLE_WIDTH: f32 = 6.0;
/// Visible width (center line).
pub const RESIZE_HANDLE_VISUAL_WIDTH: f32 = 1.0;
pub const SIDEBAR_MIN_WIDTH: f32 = 120.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 400.0;
pub const RIGHT_PANEL_MIN_WIDTH: f32 = 200.0;
pub const RIGHT_PANEL_MAX_WIDTH: f32 = 600.0;
pub const PREVIEW_PANEL_MIN_HEIGHT: f32 = 150.0;
/// Square toolbar icon button.
pub const TOOLBAR_ICON_BTN: f32 = 30.0;
pub const TOOLBAR_HEIGHT: f32 = 38.0;
pub const BREADCRUMB_BAR_HEIGHT: f32 = 34.0;
pub const STATUS_BAR_HEIGHT: f32 = 28.0;
pub const SIDEBAR_ROW_HEIGHT: f32 = 34.0;
/// Maximum height of the sidebar job list.
pub const SIDEBAR_JOBS_MAX_HEIGHT: f32 = 176.0;
pub const PANEL_HEADER_HEIGHT: f32 = 32.0;
/// Minimum width for showing preview and properties side by side.
pub const RIGHT_PANEL_HORIZONTAL_SPLIT_MIN_WIDTH: f32 = 420.0;
pub const GRID_CELL_WIDTH: f32 = 120.0;
pub const GRID_CELL_HEIGHT: f32 = 138.0;
pub const GRID_THUMB_SIZE: f32 = 96.0;
pub const LIST_ICON_COL: f32 = 20.0;
pub const LIST_COL_RESIZE_HANDLE: f32 = 6.0;

fn mix(a: Color, b: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    Color {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: a.a + (b.a - a.a) * amount,
    }
}

fn surface_sunken(theme: &Theme) -> Color {
    let palette = theme.extended_palette();
    mix(
        palette.background.base.color,
        if palette.is_dark {
            Color::BLACK
        } else {
            palette.background.base.text
        },
        if palette.is_dark { 0.12 } else { 0.025 },
    )
}

fn surface_raised(theme: &Theme) -> Color {
    let palette = theme.extended_palette();
    mix(
        palette.background.base.color,
        Color::WHITE,
        if palette.is_dark { 0.055 } else { 0.82 },
    )
}

fn hairline(theme: &Theme) -> Color {
    let palette = theme.extended_palette();
    Color {
        a: if palette.is_dark { 0.16 } else { 0.12 },
        ..palette.background.base.text
    }
}

pub fn toolbar_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(surface_sunken(theme))),
        border: Border {
            color: hairline(theme),
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn breadcrumb_style(theme: &Theme) -> container::Style {
    container::Style {
        border: Border {
            color: hairline(theme),
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn status_bar_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(surface_sunken(theme))),
        border: Border {
            color: hairline(theme),
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn sidebar_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(surface_sunken(theme))),
        border: Border {
            color: hairline(theme),
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Faint background with a left accent.
pub fn sidebar_active_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(iced::Background::Color(Color {
            a: if palette.is_dark { 0.18 } else { 0.11 },
            ..palette.primary.base.color
        })),
        border: Border {
            color: palette.primary.base.color,
            width: 1.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    }
}

pub fn sidebar_drop_target_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(iced::Background::Color(Color {
            a: 0.20,
            ..palette.secondary.base.color
        })),
        border: Border {
            color: palette.secondary.base.color,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..Default::default()
    }
}

pub fn panel_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(surface_raised(theme))),
        border: Border {
            color: hairline(theme),
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Subtle primary-color border.
pub fn active_pane_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    let c = palette.primary.base.color;
    container::Style {
        border: Border {
            color: Color { a: c.a * 0.5, ..c },
            width: 1.5,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

pub fn dialog_backdrop_style(theme: &Theme) -> container::Style {
    let is_dark = theme.extended_palette().is_dark;
    container::Style {
        background: Some(iced::Background::Color(Color::from_rgba(
            0.0,
            0.0,
            0.0,
            if is_dark { 0.62 } else { 0.36 },
        ))),
        ..Default::default()
    }
}

pub fn dialog_card_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(iced::Background::Color(surface_raised(theme))),
        border: Border {
            color: hairline(theme),
            width: 1.0,
            radius: DIALOG_BORDER_RADIUS.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if palette.is_dark { 0.5 } else { 0.18 }),
            offset: iced::Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        },
        ..Default::default()
    }
}

/// Opaque so it stays readable over other elements.
pub fn tooltip_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    let bg = if palette.is_dark {
        Color::from_rgb8(35, 44, 58)
    } else {
        Color::from_rgb8(28, 38, 54)
    };
    container::Style {
        text_color: Some(Color::from_rgb8(245, 248, 252)),
        background: Some(iced::Background::Color(bg)),
        border: Border {
            color: palette.background.weak.color,
            width: 1.0,
            radius: 7.0.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 6.0,
        },
        ..Default::default()
    }
}

/// Transparent until hovered or pressed.
pub fn toolbar_icon_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let base = button::Style {
        background: None,
        text_color: palette.background.base.text,
        border: Border {
            radius: 7.0.into(),
            ..Default::default()
        },
        shadow: iced::Shadow::default(),
        ..Default::default()
    };

    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(iced::Background::Color(Color {
                a: if palette.is_dark { 0.13 } else { 0.08 },
                ..palette.primary.base.color
            })),
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(iced::Background::Color(Color {
                a: if palette.is_dark { 0.22 } else { 0.14 },
                ..palette.primary.base.color
            })),
            ..base
        },
        button::Status::Disabled => button::Style {
            text_color: muted_color(theme),
            ..base
        },
    }
}

/// Tabs use a quiet selected surface instead of a saturated filled button.
/// This keeps the file content visually dominant while retaining a clear
/// active state in both themes.
pub fn tab_button_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: selected.then(|| iced::Background::Color(surface_raised(theme))),
        text_color: if selected {
            palette.primary.base.color
        } else {
            palette.background.base.text
        },
        border: Border {
            color: if selected {
                Color {
                    a: 0.42,
                    ..palette.primary.base.color
                }
            } else {
                Color::TRANSPARENT
            },
            width: if selected { 1.0 } else { 0.0 },
            radius: 7.0.into(),
        },
        ..button::Style::default()
    };

    match status {
        button::Status::Hovered => {
            style.background = Some(iced::Background::Color(Color {
                a: if palette.is_dark { 0.14 } else { 0.08 },
                ..palette.primary.base.color
            }));
        }
        button::Status::Pressed => {
            style.background = Some(iced::Background::Color(Color {
                a: if palette.is_dark { 0.22 } else { 0.14 },
                ..palette.primary.base.color
            }));
        }
        button::Status::Disabled => style.text_color = muted_color(theme),
        button::Status::Active => {}
    }
    style
}

pub fn list_header_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(surface_sunken(theme))),
        border: Border {
            color: hairline(theme),
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn list_row_alt_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    let base = palette.background.base.color;
    container::Style {
        background: Some(iced::Background::Color(mix(
            base,
            palette.background.base.text,
            if palette.is_dark { 0.025 } else { 0.018 },
        ))),
        ..Default::default()
    }
}

/// Small rounded colored badge with centered text.
pub fn icon_badge<'a, M: 'a>(label: &str, bg: Color) -> Element<'a, M> {
    let label_text = iced::widget::text(label.to_string())
        .size(9)
        .color(Color::WHITE)
        .center();

    iced::widget::container(label_text)
        .width(Length::Fixed(20.0))
        .height(Length::Fixed(20.0))
        .center_x(Length::Fixed(20.0))
        .center_y(Length::Fixed(20.0))
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(bg)),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

pub fn icon_badge_colored<'a, M: 'a>(label: &str, color: Color) -> Element<'a, M> {
    icon_badge(label, color)
}

pub fn file_icon_ex<'a, M: 'a>(is_dir: bool, is_symlink: bool, theme: &Theme) -> Element<'a, M> {
    let palette = theme.extended_palette();
    let base_icon = if is_dir {
        icon("\u{E2C7}") // folder
            .size(16)
            .color(palette.primary.base.color)
    } else {
        icon("\u{E873}") // description
            .size(16)
            .color(Color {
                r: 0.55,
                g: 0.55,
                b: 0.55,
                a: 1.0,
            })
    };

    if is_symlink {
        // Symlink badge: small arrow in the bottom-right corner.
        iced::widget::stack![
            base_icon,
            iced::widget::container(icon("\u{E157}").size(9).color(Color {
                r: 0.7,
                g: 0.7,
                b: 0.7,
                a: 1.0,
            }))
            .align_right(Length::Fill)
            .align_bottom(Length::Fill),
        ]
        .into()
    } else {
        base_icon.into()
    }
}

pub fn severity_badge<'a, M: 'a>(severity: &crate::state::NotificationSeverity) -> Element<'a, M> {
    use crate::state::NotificationSeverity;
    let severity = severity.clone();
    let label = match severity {
        NotificationSeverity::Info => "i",
        NotificationSeverity::Warning => "!",
        NotificationSeverity::Error => "×",
    };
    container(text(label).size(10).color(Color::WHITE).center())
        .width(Length::Fixed(22.0))
        .height(Length::Fixed(22.0))
        .center(Length::Fixed(22.0))
        .style(move |theme: &Theme| {
            let palette = theme.extended_palette();
            let background = match severity {
                NotificationSeverity::Info => palette.primary.base.color,
                NotificationSeverity::Warning => palette.warning.base.color,
                NotificationSeverity::Error => palette.danger.base.color,
            };
            container::Style {
                background: Some(iced::Background::Color(background)),
                border: Border {
                    radius: 7.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            }
        })
        .into()
}

pub fn muted_color(theme: &Theme) -> Color {
    let palette = theme.extended_palette();
    let base = palette.background.base.text;
    Color {
        a: if palette.is_dark { 0.66 } else { 0.62 },
        ..base
    }
}

/// Theme-aware secondary text. Prefer this for components that do not have
/// access to `AppState`; it avoids baking dark-theme colors into light mode.
pub fn muted_text_style(theme: &Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(muted_color(theme)),
    }
}
