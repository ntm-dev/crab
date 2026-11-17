use std::{collections::HashSet, num::NonZeroU32};

use editor::Editor;
use gpui::{App, Entity, EntityId, SharedString, Subscription, WeakEntity};
use i18n::t;
use language::{IndentOverride, language_settings::LanguageSettings};
use ui::{Tooltip, prelude::*};
use workspace::{StatusItemView, item::ItemHandle};

use crate::{IndentSelector, indent_ops::guess_indentation, indent_selector::configured_indent};

#[derive(Default)]
pub struct IndentIndicator {
    label: Option<SharedString>,
    active_editor: Option<WeakEntity<Editor>>,
    detected_buffers: HashSet<EntityId>,
    _observe_active_editor: Option<Subscription>,
}

impl IndentIndicator {
    fn detect_once(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let Some(buffer) = editor.read(cx).active_buffer(cx) else {
            return;
        };
        if !self.detected_buffers.insert(buffer.entity_id()) {
            return;
        }
        if buffer.read(cx).indent_override() != IndentOverride::default() {
            return;
        }
        let (default_tab_size, default_hard_tabs) = configured_indent(buffer.read(cx), cx);
        let guessed = guess_indentation(
            &buffer.read(cx).text(),
            default_tab_size,
            !default_hard_tabs,
        );
        buffer.update(cx, |buffer, cx| {
            buffer.set_indent_override(
                IndentOverride {
                    hard_tabs: Some(!guessed.insert_spaces),
                    tab_size: NonZeroU32::new(guessed.tab_size),
                },
                cx,
            );
        });
    }

    fn update(&mut self, editor: Entity<Editor>, _: &mut Window, cx: &mut Context<Self>) {
        self.detect_once(&editor, cx);
        self.active_editor = Some(editor.downgrade());
        self.label = editor.read(cx).active_buffer(cx).map(|buffer| {
            let settings = LanguageSettings::for_buffer(buffer.read(cx), cx);
            let size = settings.tab_size.get();
            if settings.hard_tabs {
                t!("Tab Size: {size}", size = size)
            } else {
                t!("Spaces: {size}", size = size)
            }
        });
        cx.notify();
    }
}

impl Render for IndentIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().when_some(self.label.clone(), |el, label| {
            el.child(
                Button::new("indent-indicator", label)
                    .label_size(LabelSize::Small)
                    .tab_index(0isize)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(editor) = this.active_editor.as_ref() {
                            IndentSelector::toggle(editor, window, cx);
                        }
                    }))
                    .tooltip(Tooltip::text(t!("Select Indentation"))),
            )
        })
    }
}

impl StatusItemView for IndentIndicator {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = active_pane_item.and_then(|item| item.downcast::<Editor>()) {
            self._observe_active_editor = Some(cx.observe_in(&editor, window, Self::update));
            self.update(editor, window, cx);
        } else {
            self.label = None;
            self.active_editor = None;
            self._observe_active_editor = None;
        }
        cx.notify();
    }

    fn hide_setting(&self, _: &App) -> Option<workspace::HideStatusItem> {
        None
    }
}
