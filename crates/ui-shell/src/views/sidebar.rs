//! System section (places, volumes) and user groups. User groups can be renamed, collapsed
//! and reordered by drag and drop; the running jobs are listed at the bottom.

use crate::format::truncate_with_ellipsis;
use crate::i18n::{tr, tr_with};
use crate::messages::{
    ConnectionMessage, JobMessage, Message, PaneMessage, SettingsMessage, SidebarMessage,
};
use crate::state::JobDisplayInfo;
use crate::state::{AppState, SidebarDropTarget};
use crate::views::theme;
use iced::widget::{
    button, container, mouse_area, scrollable, space, text, text_input, Column, Row,
};
use iced::{Color, Element, Length};
use iris_core_domain::types::transfer::JobState;

/// Leaves room for the macOS title bar.
const TITLEBAR_TOP_PADDING: f32 = 32.0;

pub fn view_sidebar(state: &AppState) -> Element<'_, Message> {
    let muted = theme::muted_color(&crate::app::theme(state));

    // On macOS the sidebar toggle sits to the right of the traffic lights.
    let mut content = Column::new().spacing(2);
    if cfg!(target_os = "macos") {
        let toggle_btn = button(
            container(theme::icon("\u{E5D2}").size(16).center())
                .center_x(Length::Fixed(theme::TOOLBAR_ICON_BTN))
                .center_y(Length::Fixed(theme::TOOLBAR_ICON_BTN)),
        )
        .padding(0)
        .style(theme::toolbar_icon_button_style)
        .on_press(SidebarMessage::ToggleVisibility.into());

        content = content.push(
            container(
                Row::new()
                    .push(space::horizontal().width(Length::Fixed(76.0 + theme::ZONE_PADDING)))
                    .push(toggle_btn)
                    .align_y(iced::Alignment::Center),
            )
            .height(Length::Fixed(TITLEBAR_TOP_PADDING))
            .center_y(Length::Fixed(TITLEBAR_TOP_PADDING)),
        );
    }

    for (group_idx, group) in state.sidebar_groups.iter().enumerate() {
        if !group.is_system() {
            continue;
        }
        let collapse_icon = if group.collapsed {
            "\u{E5CC}"
        } else {
            "\u{E5CF}"
        }; // chevron_right / expand_more

        // System groups cannot be renamed or reordered.
        let header = Row::new()
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .push(
                button(theme::icon(collapse_icon).size(10))
                    .on_press(SidebarMessage::ToggleGroupCollapsed(group_idx).into())
                    .padding([2, 4])
                    .style(button::text),
            )
            .push(
                button(
                    text(truncate_with_ellipsis(&group.display_name(), 24))
                        .size(11)
                        .color(muted),
                )
                .on_press(SidebarMessage::ToggleGroupCollapsed(group_idx).into())
                .padding([2, 0])
                .style(button::text),
            );

        content = content.push(container(header).padding([2, 8]).width(Length::Fill));

        if !group.collapsed {
            if group.items.is_empty() {
                content = content.push(
                    container(text(tr("sidebar-empty")).size(11).color(muted)).padding([2, 28]),
                );
            } else {
                for (item_idx, item) in group.items.iter().enumerate() {
                    content = content.push(system_item_row(state, group_idx, item_idx, item));
                }
            }
        }
    }

    content = content.push(iced::widget::Space::new().height(theme::SECTION_SPACING));

    let user_groups: Vec<(usize, &crate::state::SidebarGroup)> = state
        .sidebar_groups
        .iter()
        .enumerate()
        .filter(|(_, g)| !g.is_system())
        .collect();

    let is_dragging_item = state.sidebar_dragging_item.is_some();
    let is_dragging_group = state.sidebar_dragging_group.is_some();
    let is_dragging = is_dragging_item || is_dragging_group;

    for &(group_idx, group) in &user_groups {
        let is_renaming = state.sidebar_renaming_group == Some(group_idx);
        let collapse_icon = if group.collapsed {
            "\u{E5CC}"
        } else {
            "\u{E5CF}"
        }; // chevron_right / expand_more

        let is_group_drop_target = is_dragging_group
            && state.sidebar_drop_target == Some(SidebarDropTarget::BeforeGroup(group_idx));

        let header: Element<'_, Message> = if is_renaming {
            let input = text_input(
                &tr("sidebar-group-name-placeholder"),
                &state.sidebar_rename_input,
            )
            .on_input(|value| SidebarMessage::RenameInput(value).into())
            .on_submit(
                SidebarMessage::ConfirmRenameGroup(state.sidebar_rename_input.clone()).into(),
            )
            .padding([2, 4])
            .size(11)
            .width(Length::Fill);

            Row::new()
                .spacing(2)
                .align_y(iced::Alignment::Center)
                .push(input)
                .push(
                    button(theme::icon("\u{E5CA}").size(10))
                        .on_press(
                            SidebarMessage::ConfirmRenameGroup(state.sidebar_rename_input.clone())
                                .into(),
                        )
                        .padding([2, 4])
                        .style(button::text),
                )
                .push(
                    button(theme::icon("\u{E888}").size(10))
                        .on_press(SidebarMessage::CancelRenameGroup.into())
                        .padding([2, 4])
                        .style(button::text),
                )
                .into()
        } else {
            let drag_handle: Element<'_, Message> = mouse_area(
                container(theme::icon("\u{E945}").size(11).color(muted)).padding([2, 2]),
            )
            .on_press(SidebarMessage::DragGroup(group_idx).into())
            .into();

            let mut row = Row::new()
                .spacing(4)
                .align_y(iced::Alignment::Center)
                .push(drag_handle)
                .push(
                    button(theme::icon(collapse_icon).size(10))
                        .on_press(SidebarMessage::ToggleGroupCollapsed(group_idx).into())
                        .padding([2, 4])
                        .style(button::text),
                )
                .push(
                    button(
                        text(truncate_with_ellipsis(&group.display_name(), 18))
                            .size(11)
                            .color(muted),
                    )
                    .on_press(SidebarMessage::ToggleGroupCollapsed(group_idx).into())
                    .padding([2, 0])
                    .style(button::text),
                )
                .push(iced::widget::space::horizontal());

            row = row.push(
                button(theme::icon("\u{F097}").size(9))
                    .on_press(SidebarMessage::BeginRenameGroup(group_idx).into())
                    .padding([2, 3])
                    .style(button::text),
            );

            row = row.push(
                button(theme::icon("\u{E145}").size(11))
                    .on_press(SidebarMessage::AddCurrentPathToGroup(group_idx).into())
                    .padding([2, 6])
                    .style(button::text),
            );

            row.into()
        };

        let header_container = container(header).padding([2, 8]).width(Length::Fill);
        let header_elem: Element<'_, Message> = if is_group_drop_target {
            container(header_container)
                .style(theme::sidebar_drop_target_style)
                .width(Length::Fill)
                .into()
        } else {
            header_container.into()
        };

        // While dragging, a mouse_area detects the drop target.
        if is_dragging_group {
            content = content.push(
                mouse_area(header_elem)
                    .on_enter(
                        SidebarMessage::DragOver(Some(SidebarDropTarget::BeforeGroup(group_idx)))
                            .into(),
                    )
                    .on_exit(SidebarMessage::DragOver(None).into())
                    .on_release(SidebarMessage::Drop.into()),
            );
        } else {
            content = content.push(header_elem);
        }

        if !group.collapsed {
            if group.items.is_empty() {
                // Empty groups still accept drops.
                let empty_elem: Element<'_, Message> =
                    container(text(tr("sidebar-empty")).size(11).color(muted))
                        .padding([2, 28])
                        .width(Length::Fill)
                        .into();
                if is_dragging_item {
                    let is_target = state.sidebar_drop_target
                        == Some(SidebarDropTarget::ItemPosition {
                            group: group_idx,
                            position: 0,
                        });
                    let elem = if is_target {
                        container(empty_elem)
                            .style(theme::sidebar_drop_target_style)
                            .width(Length::Fill)
                            .into()
                    } else {
                        empty_elem
                    };
                    content = content.push(
                        mouse_area(elem)
                            .on_enter(
                                SidebarMessage::DragOver(Some(SidebarDropTarget::ItemPosition {
                                    group: group_idx,
                                    position: 0,
                                }))
                                .into(),
                            )
                            .on_exit(SidebarMessage::DragOver(None).into())
                            .on_release(SidebarMessage::Drop.into()),
                    );
                } else {
                    content = content.push(empty_elem);
                }
            } else {
                for (item_idx, item) in group.items.iter().enumerate() {
                    content = content.push(user_item_row(
                        state,
                        group_idx,
                        item_idx,
                        item,
                        is_dragging_item,
                    ));
                }
            }
        }
    }

    content = content.push(iced::widget::Space::new().height(theme::SECTION_SPACING));
    content = content.push(
        container(
            button(
                Row::new()
                    .spacing(4)
                    .align_y(iced::Alignment::Center)
                    .push(theme::icon("\u{E145}").size(11).color(muted))
                    .push(text(tr("sidebar-add-group")).size(11).color(muted)),
            )
            .on_press(SidebarMessage::AddGroup.into())
            .padding([4, 8])
            .width(Length::Fill)
            .style(button::text),
        )
        .padding([0, 8]),
    );

    // Catches the release anywhere in the sidebar to cancel a drag.
    let scrollable_content: Element<'_, Message> = if is_dragging {
        mouse_area(scrollable(content).height(Length::Fill))
            .on_release(SidebarMessage::DragCancel.into())
            .into()
    } else {
        scrollable(content).height(Length::Fill).into()
    };

    let mut bottom = Column::new().spacing(0);

    let active_jobs = state
        .jobs
        .iter()
        .filter(|j| matches!(j.state, JobState::Running | JobState::Paused))
        .count();
    let job_label = if active_jobs > 0 {
        tr_with("sidebar-jobs-with-count", &[("count", active_jobs.into())])
    } else {
        tr("jobs-title")
    };
    let job_btn = container(
        button(
            iced::widget::row![
                theme::icon(if state.sidebar_jobs_visible {
                    "\u{E5CF}" // expand_more
                } else {
                    "\u{E5CC}" // chevron_right
                })
                .size(11),
                text(job_label).size(13),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .on_press(SidebarMessage::ToggleJobs.into())
        .width(Length::Fill)
        .padding([6, 10])
        .style(button::text),
    )
    .padding([4, 8]);
    bottom = bottom.push(job_btn);

    if state.sidebar_jobs_visible {
        let jobs_content = view_sidebar_jobs(state);
        bottom = bottom.push(
            container(jobs_content)
                .width(Length::Fill)
                .height(Length::Fixed(theme::SIDEBAR_JOBS_MAX_HEIGHT))
                .padding([0, 4]),
        );
    }

    let settings_btn = container(
        button(
            iced::widget::row![
                theme::icon("\u{E8B8}").size(13),
                text(tr("settings-title")).size(13),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .on_press(SettingsMessage::Open.into())
        .width(Length::Fill)
        .padding([6, 10])
        .style(button::text),
    )
    .padding([4, 8]);
    bottom = bottom.push(settings_btn);

    container(
        Column::new()
            .push(scrollable_content)
            .push(bottom)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_sidebar_jobs(state: &AppState) -> Element<'_, Message> {
    let muted = theme::muted_color(&crate::app::theme(state));

    if state.jobs.is_empty() {
        return container(text(tr("sidebar-running-jobs-empty")).size(10).color(muted))
            .padding([4, 12])
            .width(Length::Fill)
            .into();
    }

    let mut col = Column::new().spacing(4).padding([4, 8]);
    for job in &state.jobs {
        let progress = progress_percent(job);
        let state_icon = match job.state {
            JobState::Running => "\u{E037}",   // play_arrow
            JobState::Paused => "\u{E034}",    // pause
            JobState::Completed => "\u{E5CA}", // check
            JobState::Failed => "\u{F8B6}",    // error
            JobState::Canceled => "\u{E888}",  // cancel
            _ => "\u{E5D3}",                   // more_horiz
        };

        let desc = if job.description.chars().count() > 20 {
            crate::format::truncate_with_ellipsis(&job.description, 18)
        } else {
            job.description.clone()
        };

        let mut row = Row::new()
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .push(theme::icon(state_icon).size(10))
            .push(text(desc).size(10).width(Length::Fill));

        if progress > 0.0 && progress < 100.0 {
            let mut info = format!("{:.0}%", progress);
            if let Some(speed) = job.speed_bps {
                if speed > 0.0 {
                    info = format!("{} {}/s", info, crate::format::format_size(speed as u64));
                }
            }
            row = row.push(text(info).size(9).color(muted));
        }

        match job.state {
            JobState::Running => {
                row = row.push(
                    button(theme::icon("\u{E034}").size(9)) // pause
                        .on_press(JobMessage::Pause(job.id.clone()).into())
                        .padding([1, 3])
                        .style(button::text),
                );
            }
            JobState::Paused => {
                row = row.push(
                    button(theme::icon("\u{E037}").size(9)) // play_arrow
                        .on_press(JobMessage::Resume(job.id.clone()).into())
                        .padding([1, 3])
                        .style(button::text),
                );
            }
            _ => {}
        }
        if !matches!(job.state, JobState::Completed | JobState::Canceled) {
            row = row.push(
                button(theme::icon("\u{E5CD}").size(9)) // close
                    .on_press(JobMessage::Cancel(job.id.clone()).into())
                    .padding([1, 3])
                    .style(button::text),
            );
        }

        col = col.push(row);
    }

    scrollable(col).height(Length::Shrink).into()
}

/// Falls back to the file count when the byte total is unknown.
fn progress_percent(job: &JobDisplayInfo) -> f32 {
    let progress = &job.progress;
    match (progress.bytes_total, progress.files_total) {
        (Some(total), _) if total > 0 => progress.bytes_completed as f32 / total as f32 * 100.0,
        (_, Some(total)) if total > 0 => progress.files_completed as f32 / total as f32 * 100.0,
        _ => 0.0,
    }
}

fn system_item_row<'a>(
    state: &'a AppState,
    group_idx: usize,
    item_idx: usize,
    item: &'a crate::state::SidebarItem,
) -> Element<'a, Message> {
    let active_path = &state.active_pane().current_path;
    let is_current = *active_path == item.path;
    let badge_color = Color::from_rgba(
        item.badge_color[0],
        item.badge_color[1],
        item.badge_color[2],
        item.badge_color[3],
    );
    let badge: Element<'_, Message> = theme::icon_badge_colored(&item.badge, badge_color);

    let mut row_content = Row::new()
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .push(badge)
        .push(
            text(truncate_with_ellipsis(&item.name, 22))
                .size(13)
                .width(Length::Fill),
        )
        .push(iced::widget::space::horizontal());

    if item.ejectable {
        row_content = row_content.push(
            button(theme::icon("\u{E8FB}").size(11)) // eject
                .on_press(ConnectionMessage::UnmountVolume(item.path.clone()).into())
                .padding([2, 4])
                .style(button::text),
        );
    }

    let pi = state.active_pane;
    let nav_msg = Message::Pane(pi, PaneMessage::NavigateToPath(item.path.clone()));

    let item_btn = button(row_content)
        .on_press(nav_msg)
        .width(Length::Fill)
        .height(Length::Fixed(theme::SIDEBAR_ROW_HEIGHT))
        .padding([6, 10])
        .style(button::text);

    let base: Element<'_, Message> = if is_current {
        container(item_btn)
            .style(theme::sidebar_active_style)
            .width(Length::Fill)
            .into()
    } else {
        item_btn.into()
    };

    mouse_area(base)
        .on_right_press(Message::OpenSidebarContextMenu {
            group: group_idx,
            item: item_idx,
        })
        .into()
}

/// Only the drag handle starts a drag; clicking the name navigates.
fn user_item_row<'a>(
    state: &'a AppState,
    group_idx: usize,
    item_idx: usize,
    item: &'a crate::state::SidebarItem,
    is_dragging: bool,
) -> Element<'a, Message> {
    let active_path = &state.active_pane().current_path;
    let is_current = *active_path == item.path;
    let badge_color = Color::from_rgba(
        item.badge_color[0],
        item.badge_color[1],
        item.badge_color[2],
        item.badge_color[3],
    );
    let badge: Element<'_, Message> = theme::icon_badge_colored(&item.badge, badge_color);
    let muted = theme::muted_color(&crate::app::theme(state));

    let pi = state.active_pane;
    let nav_msg = Message::Pane(pi, PaneMessage::NavigateToPath(item.path.clone()));

    let drag_handle: Element<'_, Message> =
        mouse_area(container(theme::icon("\u{E945}").size(10).color(muted)).padding([4, 2]))
            .on_press(SidebarMessage::DragItem(group_idx, item_idx).into())
            .into();

    let item_body = button(
        Row::new()
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .push(badge)
            .push(
                text(truncate_with_ellipsis(&item.name, 18))
                    .size(13)
                    .width(Length::Fill),
            )
            .push(iced::widget::space::horizontal())
            .push(
                button(theme::icon("\u{E5CD}").size(11))
                    .on_press(
                        SidebarMessage::RemoveItem {
                            group: group_idx,
                            item: item_idx,
                        }
                        .into(),
                    )
                    .padding([2, 4])
                    .style(button::text),
            ),
    )
    .on_press(nav_msg)
    .width(Length::Fill)
    .height(Length::Fixed(theme::SIDEBAR_ROW_HEIGHT))
    .padding([6, 4])
    .style(button::text);

    let row = Row::new()
        .spacing(0)
        .align_y(iced::Alignment::Center)
        .push(drag_handle)
        .push(item_body);

    let is_drop_target = state.sidebar_drop_target
        == Some(SidebarDropTarget::ItemPosition {
            group: group_idx,
            position: item_idx,
        });

    let base: Element<'_, Message> = if is_current {
        container(row)
            .style(if is_drop_target {
                theme::sidebar_drop_target_style
            } else {
                theme::sidebar_active_style
            })
            .width(Length::Fill)
            .into()
    } else if is_drop_target {
        container(row)
            .style(theme::sidebar_drop_target_style)
            .width(Length::Fill)
            .into()
    } else {
        container(row).width(Length::Fill).into()
    };

    if is_dragging {
        mouse_area(base)
            .on_enter(
                SidebarMessage::DragOver(Some(SidebarDropTarget::ItemPosition {
                    group: group_idx,
                    position: item_idx,
                }))
                .into(),
            )
            .on_exit(SidebarMessage::DragOver(None).into())
            .on_release(SidebarMessage::Drop.into())
            .into()
    } else {
        mouse_area(base)
            .on_right_press(Message::OpenSidebarContextMenu {
                group: group_idx,
                item: item_idx,
            })
            .into()
    }
}
