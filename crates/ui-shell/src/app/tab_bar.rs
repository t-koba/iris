use super::toolbar::toolbar_icon_with_tip;
use crate::format::truncate_with_ellipsis;
use crate::i18n::{tr, tr_with};
use crate::messages::{Message, TabMessage};
use crate::runtime::app_runtime;
use crate::state::AppState;
use iced::widget::{button, container, mouse_area, row, scrollable, text, tooltip};
use iced::{Element, Length};
use std::path::Path;

const TAB_LABEL_MAX_CHARS: usize = 24;

pub(super) fn view_tab_bar(state: &AppState, pane_idx: usize) -> Element<'_, Message> {
    let tabs = state.pane_tabs(pane_idx);
    let active_index = tabs.active_index();
    let tab_count = tabs.tabs().len();
    let mut tab_row = row![].spacing(2).align_y(iced::Alignment::Center);

    for (tab_index, pane) in tabs.tabs().iter().enumerate() {
        let is_active = tab_index == active_index;
        let label = tab_label(&pane.current_path);
        let is_tab_drop_target = state
            .tab_drag_state
            .is_some_and(|drag| drag.pane == pane_idx && drag.target == tab_index);
        let full_path = if pane.current_path.is_empty() {
            tr("tab-untitled")
        } else {
            app_runtime().format_display_path(&pane.current_path)
        };
        let switch_message = TabMessage::Switch {
            pane: pane_idx,
            tab: tab_index,
        };
        let tab_button = button(text(label.clone()).size(11))
            .on_press(switch_message.into())
            .padding([3, 8])
            .width(Length::Fixed(132.0))
            .style(move |theme, status| {
                crate::views::theme::tab_button_style(theme, status, is_active)
            });
        let switch_tip = tooltip(
            tab_button,
            container(text(tr_with("tab-switch", &[("name", (&full_path).into())])).size(11))
                .padding([4, 8])
                .style(crate::views::theme::tooltip_style),
            tooltip::Position::Bottom,
        )
        .gap(2)
        .snap_within_viewport(true);

        let close_message = (tab_count > 1).then_some(
            TabMessage::Close {
                pane: pane_idx,
                tab: tab_index,
            }
            .into(),
        );
        let middle_message = close_message.clone().unwrap_or(
            TabMessage::Switch {
                pane: pane_idx,
                tab: tab_index,
            }
            .into(),
        );
        let switch_tip = mouse_area(switch_tip)
            .on_press(
                TabMessage::BeginDrag {
                    pane: pane_idx,
                    tab: tab_index,
                }
                .into(),
            )
            .on_middle_press(middle_message)
            .on_right_press(Message::OpenTabContextMenu {
                pane: pane_idx,
                tab: tab_index,
            })
            .on_enter(
                TabMessage::DragOver {
                    pane: pane_idx,
                    tab: tab_index,
                }
                .into(),
            )
            .on_release(TabMessage::Drop.into());
        let move_left = toolbar_icon_with_tip(
            "\u{E5CB}",
            tr("tab-move-left"),
            (tab_index > 0)
                .then_some(TabMessage::MoveLeft {
                    pane: pane_idx,
                    tab: tab_index,
                })
                .map(Into::into),
        );
        let move_right = toolbar_icon_with_tip(
            "\u{E5CC}",
            tr("tab-move-right"),
            (tab_index + 1 < tab_count)
                .then_some(TabMessage::MoveRight {
                    pane: pane_idx,
                    tab: tab_index,
                })
                .map(Into::into),
        );
        let close_button = toolbar_icon_with_tip("\u{E5CD}", tr("tab-close"), close_message);
        let duplicate_button = toolbar_icon_with_tip(
            "\u{E14D}",
            tr("tab-duplicate"),
            Some(
                TabMessage::Duplicate {
                    pane: pane_idx,
                    tab: tab_index,
                }
                .into(),
            ),
        );

        let tab_controls: Element<'_, Message> = if is_active {
            row![
                move_left,
                switch_tip,
                move_right,
                duplicate_button,
                close_button
            ]
            .spacing(1)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![switch_tip, close_button]
                .spacing(1)
                .align_y(iced::Alignment::Center)
                .into()
        };

        let tab_controls = container(tab_controls).style(move |theme| {
            if is_tab_drop_target {
                crate::views::theme::sidebar_drop_target_style(theme)
            } else {
                container::Style::default()
            }
        });

        tab_row = tab_row.push(tab_controls);
    }

    let new_tab = toolbar_icon_with_tip(
        "\u{E145}",
        tr("tab-new"),
        Some(TabMessage::New { pane: pane_idx }.into()),
    );
    tab_row = tab_row.push(new_tab);

    container(
        scrollable(tab_row).direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default().width(0).scroller_width(0),
        )),
    )
    .width(Length::Fill)
    .height(Length::Fixed(30.0))
    .padding([2, crate::views::theme::ZONE_PADDING as u16])
    .style(crate::views::theme::toolbar_style)
    .into()
}

fn tab_label(path: &str) -> String {
    if path.is_empty() {
        return tr("tab-untitled");
    }

    let display_path = app_runtime().format_display_path(path);
    let label = Path::new(&display_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(display_path.as_str());
    truncate_with_ellipsis(label, TAB_LABEL_MAX_CHARS)
}
