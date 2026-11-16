use std::{collections::HashMap, num::NonZeroU32, sync::Arc};

use editor::{
    Editor,
    actions::{ConvertIndentationToSpaces, ConvertIndentationToTabs},
};
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Task, WeakEntity};
use i18n::t;
use language::{Buffer, Point, language_settings::LanguageSettings};
use picker::{Picker, PickerDelegate};
use settings::{LanguageSettingsContent, update_settings_file};
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{ModalView, Workspace};

const TAB_SIZES: [u32; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

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
enum Mode {
    Actions,
    TabSize,
}

pub struct IndentSelector {
    picker: Entity<Picker<IndentSelectorDelegate>>,
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
        let editor = editor.clone();
        workspace.update(cx, |workspace, cx| {
            let weak_workspace = workspace.weak_handle();
            workspace.toggle_modal(window, cx, move |window, cx| {
                IndentSelector::new(editor, buffer, weak_workspace, window, cx)
            });
        })
    }

    fn new(
        editor: WeakEntity<Editor>,
        buffer: Entity<Buffer>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate =
            IndentSelectorDelegate::new(cx.entity().downgrade(), editor, buffer, workspace);
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
    editor: WeakEntity<Editor>,
    buffer: Entity<Buffer>,
    workspace: WeakEntity<Workspace>,
    mode: Mode,
    query: String,
    matches: Vec<Choice>,
    selected_index: usize,
}

impl IndentSelectorDelegate {
    fn new(
        selector: WeakEntity<IndentSelector>,
        editor: WeakEntity<Editor>,
        buffer: Entity<Buffer>,
        workspace: WeakEntity<Workspace>,
    ) -> Self {
        let mut this = Self {
            selector,
            editor,
            buffer,
            workspace,
            mode: Mode::Actions,
            query: String::new(),
            matches: Vec::new(),
            selected_index: 0,
        };
        this.refresh_matches();
        this
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_lowercase();
        let all: Vec<Choice> = match self.mode {
            Mode::Actions => IndentAction::ALL.iter().copied().map(Choice::Action).collect(),
            Mode::TabSize => TAB_SIZES.iter().copied().map(Choice::Size).collect(),
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

    fn update_language_settings(
        &self,
        cx: &mut Context<Picker<Self>>,
        update: impl FnOnce(&mut LanguageSettingsContent) + Send + 'static,
    ) {
        let language = self
            .buffer
            .read(cx)
            .language()
            .map(|language| language.name().as_ref().to_string());
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let fs = workspace.read(cx).app_state().fs.clone();
        update_settings_file(fs, cx, move |settings, _| {
            let all_languages = &mut settings.project.all_languages;
            match language {
                Some(name) => update(all_languages.languages.0.entry(name).or_default()),
                None => update(&mut all_languages.defaults),
            }
        });
    }

    fn trim_trailing_whitespace(&self, cx: &mut Context<Picker<Self>>) {
        self.buffer.update(cx, |buffer, cx| {
            let snapshot = buffer.snapshot();
            let mut edits = Vec::new();
            for row in 0..=snapshot.max_point().row {
                let length = snapshot.line_len(row);
                if length == 0 {
                    continue;
                }
                let line: String = snapshot
                    .text_for_range(Point::new(row, 0)..Point::new(row, length))
                    .collect();
                let trimmed = line.trim_end_matches([' ', '\t']).len() as u32;
                if trimmed < length {
                    edits.push((Point::new(row, trimmed)..Point::new(row, length), ""));
                }
            }
            if !edits.is_empty() {
                buffer.edit(edits, None, cx);
            }
        });
    }

    fn detect_indentation(&self, cx: &mut Context<Picker<Self>>) {
        let text = self.buffer.read(cx).text();
        let mut tab_lines = 0usize;
        let mut space_lines = 0usize;
        let mut differences: HashMap<u32, usize> = HashMap::new();
        let mut previous_spaces = 0u32;
        for line in text.lines().take(2000) {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with('\t') {
                tab_lines += 1;
                continue;
            }
            let spaces = line.chars().take_while(|c| *c == ' ').count() as u32;
            if spaces > 0 {
                space_lines += 1;
            }
            if spaces > previous_spaces {
                *differences.entry(spaces - previous_spaces).or_default() += 1;
            }
            previous_spaces = spaces;
        }
        if tab_lines == 0 && space_lines == 0 {
            return;
        }
        if tab_lines > space_lines {
            self.update_language_settings(cx, |content| content.hard_tabs = Some(true));
            return;
        }
        let size = [2u32, 3, 4, 8]
            .into_iter()
            .max_by_key(|size| differences.get(size).copied().unwrap_or(0))
            .and_then(NonZeroU32::new);
        self.update_language_settings(cx, move |content| {
            content.hard_tabs = Some(false);
            if let Some(size) = size {
                content.tab_size = Some(size);
            }
        });
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
            Mode::TabSize => t!("Select Tab Size for Current File").as_ref().into(),
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
                self.update_language_settings(cx, move |content| {
                    content.tab_size = NonZeroU32::new(size);
                });
            }
            Choice::Action(IndentAction::ChangeTabSize) => {
                self.mode = Mode::TabSize;
                self.query.clear();
                let current = LanguageSettings::for_buffer(self.buffer.read(cx), cx)
                    .tab_size
                    .get() as usize;
                self.selected_index = current.clamp(1, TAB_SIZES.len()) - 1;
                self.refresh_matches();
                cx.notify();
                return;
            }
            Choice::Action(IndentAction::UseSpaces) => {
                self.update_language_settings(cx, |content| content.hard_tabs = Some(false));
            }
            Choice::Action(IndentAction::UseTabs) => {
                self.update_language_settings(cx, |content| content.hard_tabs = Some(true));
            }
            Choice::Action(IndentAction::Detect) => self.detect_indentation(cx),
            Choice::Action(IndentAction::ConvertToSpaces) => {
                self.editor
                    .update(cx, |editor, cx| {
                        editor.convert_indentation_to_spaces(
                            &ConvertIndentationToSpaces,
                            window,
                            cx,
                        )
                    })
                    .ok();
            }
            Choice::Action(IndentAction::ConvertToTabs) => {
                self.editor
                    .update(cx, |editor, cx| {
                        editor.convert_indentation_to_tabs(&ConvertIndentationToTabs, window, cx)
                    })
                    .ok();
            }
            Choice::Action(IndentAction::TrimTrailingWhitespace) => {
                self.trim_trailing_whitespace(cx);
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
        let hint = match choice {
            Choice::Action(action) if self.query.is_empty() => action.hint(),
            _ => None,
        };
        let current_size = match choice {
            Choice::Size(size) => {
                let settings = LanguageSettings::for_buffer(self.buffer.read(cx), cx);
                (settings.tab_size.get() == size).then_some(())
            }
            Choice::Action(_) => None,
        };

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(Label::new(label))
                .when_some(hint, |item, hint| {
                    item.end_slot(Label::new(hint).size(LabelSize::Small).color(Color::Muted))
                })
                .when(current_size.is_some(), |item| {
                    item.end_slot(
                        Label::new(t!("Configured Tab Size"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                }),
        )
    }
}
