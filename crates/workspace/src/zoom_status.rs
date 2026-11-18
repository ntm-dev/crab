use gpui::{Anchor, App, IntoElement, Window};
use i18n::t;
use settings::Settings;
use theme_settings::ThemeSettings;
use ui::{Button, ButtonStyle, ContextMenu, Divider, IconButton, IconName, IconSize, PopoverMenu, Tooltip, prelude::*};

use crate::{StatusItemView, item::ItemHandle};

#[derive(Default)]
pub struct ZoomStatus;

fn zoom_state(cx: &App) -> (i32, i32) {
    let settings = ThemeSettings::get_global(cx);
    let current = f32::from(settings.ui_font_size(cx));
    let configured = f32::from(settings.ui_font_size_settings());
    let level = (current - configured).round() as i32;
    let percent = (current / configured * 100.0).round() as i32;
    (level, percent)
}

fn dispatch_zoom_action(name: &'static str, window: &mut Window, cx: &mut App) {
    if let Ok(action) = cx.build_action(name, Some(serde_json::json!({ "persist": false }))) {
        window.dispatch_action(action, cx);
    }
}

impl Render for ZoomStatus {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (level, percent) = zoom_state(cx);
        if level == 0 {
            return div();
        }
        let icon = if level < 0 {
            IconName::ZoomOut
        } else {
            IconName::ZoomIn
        };
        let tooltip_text = t!(
            "Zoom Level: {level} ({percent}%)",
            level = level,
            percent = percent
        );

        div().child(
            PopoverMenu::new("zoom-status-menu")
                .anchor(Anchor::BottomRight)
                .trigger_with_tooltip(
                    IconButton::new("zoom-status", icon)
                        .icon_size(IconSize::Medium)
                        .style(ButtonStyle::Tinted(ui::TintColor::Accent)),
                    Tooltip::text(tooltip_text),
                )
                .menu(move |window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _, _| {
                        menu.custom_row(|_, cx| {
                            let (level, percent) = zoom_state(cx);
                            h_flex()
                                .px_2()
                                .py_1()
                                .gap_1()
                                .items_center()
                                .child(
                                    IconButton::new("zoom-out", IconName::Dash)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text(t!("Zoom Out")))
                                        .on_click(|_, window, cx| {
                                            dispatch_zoom_action(
                                                "crab::DecreaseBufferFontSize",
                                                window,
                                                cx,
                                            )
                                        }),
                                )
                                .child(
                                    Label::new(level.to_string())
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    IconButton::new("zoom-in", IconName::Plus)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text(t!("Zoom In")))
                                        .on_click(|_, window, cx| {
                                            dispatch_zoom_action(
                                                "crab::IncreaseBufferFontSize",
                                                window,
                                                cx,
                                            )
                                        }),
                                )
                                .child(
                                    Label::new(format!("{percent}%"))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(Divider::vertical())
                                .child(
                                    Button::new("zoom-reset", t!("Reset"))
                                        .label_size(LabelSize::Default)
                                        .on_click(|_, window, cx| {
                                            dispatch_zoom_action(
                                                "crab::ResetBufferFontSize",
                                                window,
                                                cx,
                                            )
                                        }),
                                )
                                .child(
                                    IconButton::new("zoom-settings", IconName::Settings)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text(t!("Settings")))
                                        .on_click(|_, window, cx| {
                                            if let Ok(action) =
                                                cx.build_action("crab::OpenSettingsFile", None)
                                            {
                                                window.dispatch_action(action, cx);
                                            }
                                        }),
                                )
                                .into_any_element()
                        })
                    }))
                }),
        )
    }
}

impl StatusItemView for ZoomStatus {
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
