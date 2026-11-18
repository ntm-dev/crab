use gpui::{
    Anchor, App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Subscription, WeakEntity, Window, px,
};
use i18n::t;
use ui::{IconButton, IconButtonShape, IconName, IconSize, PopoverMenu, Tooltip, prelude::*};

use crate::{StatusItemView, Workspace, item::ItemHandle};

pub struct NotificationsStatus {
    workspace: Entity<Workspace>,
    _subscription: Subscription,
}

impl NotificationsStatus {
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&workspace, |_, _, cx| cx.notify());
        Self {
            workspace,
            _subscription: subscription,
        }
    }
}

impl Render for NotificationsStatus {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace_entity = self.workspace.clone();
        let (total, unseen, do_not_disturb) = {
            let workspace = self.workspace.read(cx);
            (
                workspace.notification_count(),
                workspace.unseen_notification_count(),
                workspace.is_do_not_disturb(),
            )
        };
        let workspace = self.workspace.downgrade();
        let tooltip = if do_not_disturb {
            t!("Do Not Disturb Mode is Enabled")
        } else if total == 0 {
            t!("No Notifications")
        } else if unseen == 0 {
            t!("No New Notifications")
        } else if unseen == 1 {
            t!("1 New Notification")
        } else {
            t!("{count} New Notifications", count = unseen)
        };
        let icon = match (do_not_disturb, unseen > 0) {
            (false, false) => IconName::Bell,
            (false, true) => IconName::BellDot,
            (true, false) => IconName::BellOff,
            (true, true) => IconName::BellOffDot,
        };

        PopoverMenu::new("notifications-status-menu")
            .anchor(Anchor::BottomRight)
            .trigger_with_tooltip(
                IconButton::new("notifications-status", icon)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small),
                Tooltip::text(tooltip),
            )
            .menu(move |_window, cx| {
                let workspace = workspace.clone();
                let _ = &workspace_entity;
                Some(cx.new(|cx| NotificationsCenter::new(workspace, cx)))
            })
    }
}

struct NotificationsCenter {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    _subscription: Option<Subscription>,
}

impl NotificationsCenter {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let subscription = workspace
            .upgrade()
            .map(|workspace| cx.observe(&workspace, |_, _, cx| cx.notify()));
        workspace
            .update(cx, |workspace, cx| {
                workspace.set_notification_center_visible(true, cx)
            })
            .ok();
        cx.on_release(|this, cx| {
            this.workspace
                .update(cx, |workspace, cx| {
                    workspace.set_notification_center_visible(false, cx)
                })
                .ok();
        })
        .detach();
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    fn clear_all(&mut self, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.clear_all_notifications(cx))
            .ok();
        cx.emit(DismissEvent);
    }

    fn toggle_do_not_disturb(&mut self, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                let enabled = !workspace.is_do_not_disturb();
                workspace.set_do_not_disturb(enabled, cx)
            })
            .ok();
        cx.notify();
    }
}

impl EventEmitter<DismissEvent> for NotificationsCenter {}

impl Focusable for NotificationsCenter {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NotificationsCenter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (notifications, do_not_disturb): (Vec<_>, bool) = self
            .workspace
            .upgrade()
            .map(|workspace| {
                let workspace = workspace.read(cx);
                (
                    workspace
                        .notifications
                        .iter()
                        .rev()
                        .map(|(id, view)| (id.clone(), view.clone()))
                        .collect(),
                    workspace.is_do_not_disturb(),
                )
            })
            .unwrap_or_default();
        let is_empty = notifications.is_empty();
        let title = if is_empty {
            t!("No New Notifications")
        } else {
            t!("Notifications")
        };

        let colors = cx.theme().colors();
        let body_background = colors.elevated_surface_background;
        let mut header_background = body_background;
        header_background.l = (header_background.l * 1.3).min(1.0);
        let border_color = gpui::rgb(0x303031);

        v_flex()
            .id("notifications-center")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(450.))
            .max_h(px(435.))
            .overflow_hidden()
            .bg(body_background)
            .border_1()
            .border_color(border_color)
            .rounded(px(4.))
            .shadow_lg()
            .child(
                h_flex()
                    .w_full()
                    .h(px(35.))
                    .flex_none()
                    .pl(px(8.))
                    .pr(px(5.))
                    .gap_1()
                    .justify_between()
                    .bg(header_background)
                    .child(
                        Label::new(title)
                            .size(LabelSize::Default)
                            .weight(gpui::FontWeight::SEMIBOLD),
                    )
                    .child(
                        h_flex()
                            .gap(px(2.))
                            .child(
                                IconButton::new("notifications-clear-all", IconName::ClearAll)
                                    .shape(IconButtonShape::Square)
                                    .icon_size(IconSize::Small)
                                    .disabled(is_empty)
                                    .tooltip(Tooltip::text(t!("Clear All Notifications")))
                                    .on_click(cx.listener(|this, _, _, cx| this.clear_all(cx))),
                            )
                            .child(
                                IconButton::new("notifications-dnd", IconName::BellOff)
                                    .shape(IconButtonShape::Square)
                                    .icon_size(IconSize::Small)
                                    .toggle_state(do_not_disturb)
                                    .tooltip(Tooltip::text(if do_not_disturb {
                                        t!("Disable Do Not Disturb Mode")
                                    } else {
                                        t!("Enable Do Not Disturb Mode")
                                    }))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_do_not_disturb(cx)),
                                    ),
                            )
                            .child(
                                IconButton::new("notifications-hide", IconName::ChevronDown)
                                    .shape(IconButtonShape::Square)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text(t!("Hide Notifications")))
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                            ),
                    ),
            )
            .when(!is_empty, |this| {
                this.child(
                    v_flex()
                        .id("notifications-center-list")
                        .max_h(px(400.))
                        .overflow_y_scroll()
                        .children({
                            let last = notifications.len().saturating_sub(1);
                            notifications
                                .into_iter()
                                .enumerate()
                                .map(move |(ix, (_, view))| {
                                    div()
                                        .w_full()
                                        .flex_none()
                                        .overflow_hidden()
                                        .when(ix != last, |this| {
                                            this.border_b_1().border_color(header_background)
                                        })
                                        .child(div().mx(px(-1.)).my(px(-1.)).child(view))
                                })
                        }),
                )
            })
    }
}

impl StatusItemView for NotificationsStatus {
    fn set_active_pane_item(
        &mut self,
        _: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<crate::HideStatusItem> {
        None
    }
}
