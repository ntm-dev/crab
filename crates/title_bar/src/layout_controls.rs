use gpui::{Anchor, Context, IntoElement, Window};
use i18n::t;
use settings::{ActivityBarPosition, BottomDockLayout, Settings as _, update_settings_file};
use ui::{
    ContextMenu, IconButton, IconName, IconPosition, IconSize, PopoverMenu, Tooltip, prelude::*,
};
use workspace::{
    ActivityBarSettings, StatusBarSettings, ToggleCenteredLayout, ToggleLeftDock, Workspace,
    WorkspaceSettings, dock::DockPosition,
};

use crate::TitleBar;

fn is_dock_open(workspace: &Workspace, position: DockPosition, cx: &App) -> bool {
    workspace.dock_at_position(position).read(cx).is_open()
}

fn toggle_dock(
    workspace: &gpui::WeakEntity<Workspace>,
    position: DockPosition,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| workspace.toggle_dock(position, window, cx))
        .ok();
}

fn update_settings(
    workspace: &gpui::WeakEntity<Workspace>,
    cx: &mut App,
    update: impl FnOnce(&mut settings::SettingsContent) + Send + 'static,
) {
    let Some(fs) = workspace
        .upgrade()
        .map(|workspace| workspace.read(cx).app_state().fs.clone())
    else {
        return;
    };
    update_settings_file(fs, cx, move |content, _| update(content));
}

fn move_side_bar(workspace: &gpui::WeakEntity<Workspace>, right: bool, cx: &mut App) {
    update_settings(workspace, cx, move |content| {
        content.activity_bar.get_or_insert_default().position = Some(if right {
            ActivityBarPosition::Right
        } else {
            ActivityBarPosition::Left
        });
        let side = right.then_some(settings::DockSide::Right).or(Some(settings::DockSide::Left));
        let position = right
            .then_some(settings::DockPosition::Right)
            .or(Some(settings::DockPosition::Left));
        content.project_panel.get_or_insert_default().dock = side;
        content.outline_panel.get_or_insert_default().dock = side;
        content.search_panel.get_or_insert_default().dock = side;
        content.git_panel.get_or_insert_default().dock = position;
        content.debugger.get_or_insert_default().dock = position;
    });
}

fn restore_defaults(workspace: &gpui::WeakEntity<Workspace>, cx: &mut App) {
    update_settings(workspace, cx, |content| {
        if let Some(activity_bar) = content.activity_bar.as_mut() {
            activity_bar.show = None;
            activity_bar.position = None;
        }
        if let Some(status_bar) = content.status_bar.as_mut() {
            status_bar.show = None;
        }
        content.workspace.bottom_dock_layout = None;
        if let Some(panel) = content.project_panel.as_mut() {
            panel.dock = None;
        }
        if let Some(panel) = content.outline_panel.as_mut() {
            panel.dock = None;
        }
        if let Some(panel) = content.search_panel.as_mut() {
            panel.dock = None;
        }
        if let Some(panel) = content.git_panel.as_mut() {
            panel.dock = None;
        }
        if let Some(debugger) = content.debugger.as_mut() {
            debugger.dock = None;
        }
    });
}

impl TitleBar {
    pub(crate) fn render_layout_controls(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let workspace = self.workspace.clone();
        let (primary_open, panel_open) = workspace
            .upgrade()
            .map(|workspace| {
                let workspace = workspace.read(cx);
                (
                    is_dock_open(workspace, DockPosition::Left, cx),
                    is_dock_open(workspace, DockPosition::Bottom, cx),
                )
            })
            .unwrap_or((false, false));

        let button = |id: &'static str, icon: IconName| {
            IconButton::new(id, icon)
                .icon_size(IconSize::Custom(rems(16. / 13.)))
                .width(ui::scaled_px(26.))
                .height(ui::scaled_px(22.).into())
        };

        h_flex()
            .gap_0()
            .child(
                PopoverMenu::new("title-bar-customize-layout")
                    .anchor(Anchor::TopRight)
                    .with_handle(self.layout_menu_handle.clone())
                    .trigger_with_tooltip(
                        button("title-bar-customize-layout-button", IconName::Layout)
                            .toggle_state(self.layout_menu_handle.is_deployed()),
                        Tooltip::text(t!("Customize Layout...")),
                    )
                    .menu({
                        let workspace = workspace.clone();
                        move |window, cx| Some(build_layout_menu(workspace.clone(), window, cx))
                    }),
            )
            .child(
                button(
                    "title-bar-toggle-primary-side-bar",
                    if primary_open {
                        IconName::LayoutSidebarLeft
                    } else {
                        IconName::LayoutSidebarLeftOff
                    },
                )
                .tooltip(|_, cx| {
                    Tooltip::for_action(t!("Toggle Primary Side Bar"), &ToggleLeftDock, cx)
                })
                .on_click({
                    let workspace = workspace.clone();
                    move |_, window, cx| toggle_dock(&workspace, DockPosition::Left, window, cx)
                }),
            )
            .child(
                button(
                    "title-bar-toggle-panel",
                    if panel_open {
                        IconName::LayoutPanel
                    } else {
                        IconName::LayoutPanelOff
                    },
                )
                .tooltip(|_, cx| {
                    Tooltip::for_action(
                        t!("Toggle Panel"),
                        &workspace::ToggleBottomDock,
                        cx,
                    )
                })
                .on_click({
                    let workspace = workspace.clone();
                    move |_, window, cx| toggle_dock(&workspace, DockPosition::Bottom, window, cx)
                }),
            )
    }
}

fn build_layout_menu(
    workspace: gpui::WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Entity<ContextMenu> {
    let (primary_open, secondary_open, panel_open, centered, layout) = workspace
        .upgrade()
        .map(|workspace| {
            let workspace = workspace.read(cx);
            (
                is_dock_open(workspace, DockPosition::Left, cx),
                is_dock_open(workspace, DockPosition::Right, cx),
                is_dock_open(workspace, DockPosition::Bottom, cx),
                workspace.centered_layout,
                WorkspaceSettings::get_global(cx).bottom_dock_layout,
            )
        })
        .unwrap_or((false, false, false, false, BottomDockLayout::Contained));
    let activity_bar = ActivityBarSettings::get_global(cx);
    let activity_bar_shown = activity_bar.show;
    let side_bar_right = activity_bar.position == ActivityBarPosition::Right;
    let status_bar_shown = StatusBarSettings::get_global(cx).show;
    let fullscreen = window.is_fullscreen();

    ContextMenu::build(window, cx, move |menu, _, _| {
        let mut menu = menu
            .header(t!("Visibility"))
            .toggleable_entry(
                t!("Activity Bar"),
                activity_bar_shown,
                IconPosition::End,
                None,
                {
                    let workspace = workspace.clone();
                    move |_, cx| {
                        update_settings(&workspace, cx, move |content| {
                            content.activity_bar.get_or_insert_default().show =
                                Some(!activity_bar_shown);
                        });
                    }
                },
            )
            .toggleable_entry(
                t!("Primary Side Bar"),
                primary_open,
                IconPosition::End,
                Some(Box::new(ToggleLeftDock)),
                {
                    let workspace = workspace.clone();
                    move |window, cx| toggle_dock(&workspace, DockPosition::Left, window, cx)
                },
            )
            .toggleable_entry(
                t!("Secondary Side Bar"),
                secondary_open,
                IconPosition::End,
                Some(Box::new(workspace::ToggleRightDock)),
                {
                    let workspace = workspace.clone();
                    move |window, cx| toggle_dock(&workspace, DockPosition::Right, window, cx)
                },
            )
            .toggleable_entry(
                t!("Panel"),
                panel_open,
                IconPosition::End,
                Some(Box::new(workspace::ToggleBottomDock)),
                {
                    let workspace = workspace.clone();
                    move |window, cx| toggle_dock(&workspace, DockPosition::Bottom, window, cx)
                },
            )
            .toggleable_entry(
                t!("Status Bar"),
                status_bar_shown,
                IconPosition::End,
                None,
                {
                    let workspace = workspace.clone();
                    move |_, cx| {
                        update_settings(&workspace, cx, move |content| {
                            content.status_bar.get_or_insert_default().show =
                                Some(!status_bar_shown);
                        });
                    }
                },
            )
            .separator()
            .header(t!("Primary Side Bar Position"))
            .toggleable_entry(t!("Left"), !side_bar_right, IconPosition::End, None, {
                let workspace = workspace.clone();
                move |_, cx| move_side_bar(&workspace, false, cx)
            })
            .toggleable_entry(t!("Right"), side_bar_right, IconPosition::End, None, {
                let workspace = workspace.clone();
                move |_, cx| move_side_bar(&workspace, true, cx)
            })
            .separator()
            .header(t!("Panel Alignment"));

        for (label, value) in [
            (t!("Left"), BottomDockLayout::LeftAligned),
            (t!("Right"), BottomDockLayout::RightAligned),
            (t!("Center"), BottomDockLayout::Contained),
            (t!("Justify"), BottomDockLayout::Full),
        ] {
            let workspace = workspace.clone();
            menu = menu.toggleable_entry(label, layout == value, IconPosition::End, None, {
                move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.set_bottom_dock_layout(value, window, cx)
                        })
                        .ok();
                }
            });
        }

        menu.separator()
            .header(t!("Modes"))
            .toggleable_entry(
                t!("Full Screen"),
                fullscreen,
                IconPosition::End,
                None,
                |window, cx| {
                    if let Ok(action) = cx.build_action("crab::ToggleFullScreen", None) {
                        window.dispatch_action(action, cx);
                    }
                },
            )
            .toggleable_entry(
                t!("Centered Layout"),
                centered,
                IconPosition::End,
                Some(Box::new(ToggleCenteredLayout)),
                {
                    let workspace = workspace.clone();
                    move |window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.toggle_centered_layout(&ToggleCenteredLayout, window, cx)
                            })
                            .ok();
                    }
                },
            )
            .separator()
            .entry(t!("Restore Defaults"), None, {
                let workspace = workspace.clone();
                move |_, cx| restore_defaults(&workspace, cx)
            })
    })
}
