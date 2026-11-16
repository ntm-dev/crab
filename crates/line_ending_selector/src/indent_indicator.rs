use editor::Editor;
use gpui::{App, Entity, SharedString, Subscription, WeakEntity};
use i18n::t;
use language::language_settings::LanguageSettings;
use ui::{Tooltip, prelude::*};
use workspace::{StatusItemView, item::ItemHandle};

use crate::IndentSelector;

#[derive(Default)]
pub struct IndentIndicator {
    label: Option<SharedString>,
    active_editor: Option<WeakEntity<Editor>>,
    _observe_active_editor: Option<Subscription>,
}

impl IndentIndicator {
    fn update(&mut self, editor: Entity<Editor>, _: &mut Window, cx: &mut Context<Self>) {
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
