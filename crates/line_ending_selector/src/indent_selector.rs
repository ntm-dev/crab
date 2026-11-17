use std::{num::NonZeroU32, sync::Arc};

use editor::Editor;
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Task, WeakEntity};
use i18n::t;
use language::{
    Buffer,
    language_settings::{AllLanguageSettings, LanguageSettings},
};
use picker::{Picker, PickerDelegate};
use settings::Settings as _;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{ModalView, Workspace};

use crate::indent_ops::{
    TextEdit, convert_to_spaces, convert_to_tabs, guess_indentation, trim_trailing_whitespace,
};

const TAB_SIZES: [u32; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
const FOCUS_COLOR: u32 = 0x19f9d8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndentAction {
    UseSpaces,
    UseTabs,
    ChangeTabSize,
    Detect,
    ConvertToSpaces,
    ConvertToTabs,
    TrimTrailingWhitespace,
}

impl IndentAction {
    const ALL: [IndentAction; 7] = [
        IndentAction::UseSpaces,
        IndentAction::UseTabs,
        IndentAction::ChangeTabSize,
        IndentAction::Detect,
        IndentAction::ConvertToSpaces,
        IndentAction::ConvertToTabs,
        IndentAction::TrimTrailingWhitespace,
    ];

    fn label(self) -> SharedString {
        match self {
            IndentAction::UseSpaces => t!("Indent Using Spaces"),
            IndentAction::UseTabs => t!("Indent Using Tabs"),
            IndentAction::ChangeTabSize => t!("Change Tab Display Size"),
            IndentAction::Detect => t!("Detect Indentation from Content"),
            IndentAction::ConvertToSpaces => t!("Convert Indentation to Spaces"),
            IndentAction::ConvertToTabs => t!("Convert Indentation to Tabs"),
            IndentAction::TrimTrailingWhitespace => t!("Trim Trailing Whitespace"),
        }
    }

    fn hint(self) -> Option<SharedString> {
        match self {
            IndentAction::UseSpaces => Some(t!("change view")),
            IndentAction::Detect => Some(t!("convert file")),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Action(IndentAction),
    Size(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SizeTarget {
    Spaces,
    Tabs,
    DisplayOnly,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Actions,
    TabSize(SizeTarget),
}

pub struct IndentSelector {
    picker: Entity<Picker<IndentSelectorDelegate>>,
}

pub(crate) fn configured_indent(buffer: &Buffer, cx: &App) -> (u32, bool) {
    let location = buffer.file().map(|file| settings::SettingsLocation {
        worktree_id: file.worktree_id(cx),
        path: file.path().as_ref(),
    });
    let language = buffer.language().map(|language| language.name());
    let settings = AllLanguageSettings::get(location, cx).language(location, language.as_ref(), cx);
    (settings.tab_size.get(), settings.hard_tabs)
}

impl IndentSelector {
    pub fn toggle(editor: &WeakEntity<Editor>, window: &mut Window, cx: &mut App) {
        let Some((workspace, buffer)) = editor
            .update(cx, |editor, cx| {
                Some((editor.workspace()?, editor.active_buffer(cx)?))
            })
            .ok()
            .flatten()
        else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            let weak_workspace = workspace.weak_handle();
            workspace.toggle_modal(window, cx, move |window, cx| {
                IndentSelector::new(buffer, weak_workspace, window, cx)
            });
        })
    }

    fn new(
        buffer: Entity<Buffer>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let configured = configured_indent(buffer.read(cx), cx).0;
        let uses_tabs = LanguageSettings::for_buffer(buffer.read(cx), cx).hard_tabs;
        let delegate = IndentSelectorDelegate::new(
            cx.entity().downgrade(),
            buffer,
            workspace,
            configured,
            uses_tabs,
        );
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for IndentSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().child(self.picker.clone())
    }
}

impl Focusable for IndentSelector {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for IndentSelector {}
impl ModalView for IndentSelector {}

pub struct IndentSelectorDelegate {
    selector: WeakEntity<IndentSelector>,
    buffer: Entity<Buffer>,
    _workspace: WeakEntity<Workspace>,
    configured_tab_size: u32,
    mode: Mode,
    query: String,
    matches: Vec<Choice>,
    selected_index: usize,
}

impl IndentSelectorDelegate {
    fn new(
        selector: WeakEntity<IndentSelector>,
        buffer: Entity<Buffer>,
        workspace: WeakEntity<Workspace>,
        configured_tab_size: u32,
        uses_tabs: bool,
    ) -> Self {
        let mut this = Self {
            selector,
            buffer,
            _workspace: workspace,
            configured_tab_size,
            mode: Mode::Actions,
            query: String::new(),
            matches: Vec::new(),
            selected_index: usize::from(uses_tabs),
        };
        this.refresh_matches();
        this
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_lowercase();
        let all: Vec<Choice> = match self.mode {
            Mode::Actions => IndentAction::ALL.iter().copied().map(Choice::Action).collect(),
            Mode::TabSize(_) => TAB_SIZES.iter().copied().map(Choice::Size).collect(),
        };
        self.matches = all
            .into_iter()
            .filter(|choice| {
                query.is_empty() || Self::choice_label(*choice).to_lowercase().contains(&query)
            })
            .collect();
        self.selected_index = self.selected_index.min(self.matches.len().saturating_sub(1));
    }

    fn choice_label(choice: Choice) -> SharedString {
        match choice {
            Choice::Action(action) => action.label(),
            Choice::Size(size) => size.to_string().into(),
        }
    }

    fn current_tab_size(&self, cx: &App) -> u32 {
        LanguageSettings::for_buffer(self.buffer.read(cx), cx)
            .tab_size
            .get()
    }

    fn set_indent(
        &self,
        hard_tabs: Option<bool>,
        tab_size: Option<u32>,
        cx: &mut Context<Picker<Self>>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            let mut indent = buffer.indent_override();
            if let Some(hard_tabs) = hard_tabs {
                indent.hard_tabs = Some(hard_tabs);
            }
            if let Some(tab_size) = tab_size.and_then(NonZeroU32::new) {
                indent.tab_size = Some(tab_size);
            }
            buffer.set_indent_override(indent, cx);
        });
    }

    fn apply_edits(&self, edits: Vec<TextEdit>, cx: &mut Context<Picker<Self>>) {
        if edits.is_empty() {
            return;
        }
        self.buffer.update(cx, |buffer, cx| {
            buffer.edit(
                edits
                    .into_iter()
                    .map(|edit| (edit.range, Arc::<str>::from(edit.text))),
                None,
                cx,
            );
        });
    }

    fn detect_indentation(&self, cx: &mut Context<Picker<Self>>) {
        let buffer = self.buffer.read(cx);
        let (default_tab_size, default_hard_tabs) = configured_indent(buffer, cx);
        let guessed = guess_indentation(&buffer.text(), default_tab_size, !default_hard_tabs);
        self.set_indent(Some(!guessed.insert_spaces), Some(guessed.tab_size), cx);
    }

    fn enter_size_mode(
        &mut self,
        target: SizeTarget,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        self.mode = Mode::TabSize(target);
        self.query.clear();
        let current = self.current_tab_size(cx) as usize;
        self.selected_index = current.clamp(1, TAB_SIZES.len()) - 1;
        self.refresh_matches();
        cx.defer_in(window, |picker, window, cx| {
            picker.set_query("", window, cx);
            picker.refresh_placeholder(window, cx);
        });
        cx.notify();
    }
}

impl PickerDelegate for IndentSelectorDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "indent selector"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        match self.mode {
            Mode::Actions => t!("Select Action").as_ref().into(),
            Mode::TabSize(_) => t!("Select Tab Size for Current File").as_ref().into(),
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn separators_after_indices(&self) -> Vec<usize> {
        if self.mode == Mode::Actions && self.query.is_empty() {
            vec![2]
        } else {
            Vec::new()
        }
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(choice) = self.matches.get(self.selected_index).copied() else {
            return;
        };
        match choice {
            Choice::Size(size) => {
                let Mode::TabSize(target) = self.mode else {
                    return;
                };
                match target {
                    SizeTarget::Spaces => self.set_indent(Some(false), Some(size), cx),
                    SizeTarget::Tabs => self.set_indent(Some(true), Some(size), cx),
                    SizeTarget::DisplayOnly => self.set_indent(None, Some(size), cx),
                }
            }
            Choice::Action(IndentAction::UseSpaces) => {
                self.enter_size_mode(SizeTarget::Spaces, window, cx);
                return;
            }
            Choice::Action(IndentAction::UseTabs) => {
                self.enter_size_mode(SizeTarget::Tabs, window, cx);
                return;
            }
            Choice::Action(IndentAction::ChangeTabSize) => {
                self.enter_size_mode(SizeTarget::DisplayOnly, window, cx);
                return;
            }
            Choice::Action(IndentAction::Detect) => self.detect_indentation(cx),
            Choice::Action(IndentAction::ConvertToSpaces) => {
                let tab_size = self.current_tab_size(cx);
                let text = self.buffer.read(cx).text();
                self.apply_edits(convert_to_spaces(&text, tab_size), cx);
                self.set_indent(Some(false), None, cx);
            }
            Choice::Action(IndentAction::ConvertToTabs) => {
                let tab_size = self.current_tab_size(cx);
                let text = self.buffer.read(cx).text();
                self.apply_edits(convert_to_tabs(&text, tab_size), cx);
                self.set_indent(Some(true), None, cx);
            }
            Choice::Action(IndentAction::TrimTrailingWhitespace) => {
                let text = self.buffer.read(cx).text();
                self.apply_edits(trim_trailing_whitespace(&text), cx);
            }
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.selector.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.query = query;
        self.refresh_matches();
        Task::ready(())
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let choice = *self.matches.get(ix)?;
        let label = Self::choice_label(choice);
        let focus_color = Color::Custom(gpui::rgb(FOCUS_COLOR).into());
        let (text_color, description_color) = if selected {
            (focus_color, focus_color)
        } else {
            (Color::Default, Color::Muted)
        };
        let hint = match choice {
            Choice::Action(action) if self.query.is_empty() => action.hint(),
            _ => None,
        };
        let description = match choice {
            Choice::Size(size) => {
                let current = self.current_tab_size(cx);
                if size == self.configured_tab_size && size == current {
                    Some(t!("Configured Tab Size"))
                } else if size == self.configured_tab_size {
                    Some(t!("Default Tab Size"))
                } else if size == current {
                    Some(t!("Current Tab Size"))
                } else {
                    None
                }
            }
            Choice::Action(_) => None,
        };

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    h_flex()
                        .gap_2()
                        .child(Label::new(label).color(text_color))
                        .when_some(description, |this, description| {
                            this.child(
                                Label::new(description)
                                    .size(LabelSize::Small)
                                    .color(description_color),
                            )
                        }),
                )
                .when_some(hint, |item, hint| {
                    item.end_slot(Label::new(hint).size(LabelSize::Small).color(Color::Muted))
                }),
        )
    }
}
