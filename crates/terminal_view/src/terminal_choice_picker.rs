use std::sync::Arc;

use gpui::{
    App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Hsla, SharedString, Task,
    WeakEntity,
};
use i18n::t;
use picker::{Picker, PickerDelegate};
use theme::ActiveTheme;
use ui::{Icon, IconName, IconSize, ListItem, ListItemSpacing, prelude::*};
use workspace::{ModalView, Workspace};

use crate::TerminalView;

pub fn ansi_theme_color(index: usize, cx: &App) -> Hsla {
    let colors = cx.theme().colors();
    match index {
        0 => colors.terminal_ansi_black,
        1 => colors.terminal_ansi_red,
        2 => colors.terminal_ansi_green,
        3 => colors.terminal_ansi_yellow,
        4 => colors.terminal_ansi_blue,
        5 => colors.terminal_ansi_magenta,
        6 => colors.terminal_ansi_cyan,
        7 => colors.terminal_ansi_white,
        8 => colors.terminal_ansi_bright_black,
        9 => colors.terminal_ansi_bright_red,
        10 => colors.terminal_ansi_bright_green,
        11 => colors.terminal_ansi_bright_yellow,
        12 => colors.terminal_ansi_bright_blue,
        13 => colors.terminal_ansi_bright_magenta,
        14 => colors.terminal_ansi_bright_cyan,
        _ => colors.terminal_ansi_bright_white,
    }
}

fn color_label(index: usize) -> SharedString {
    match index {
        0 => t!("Black"),
        1 => t!("Red"),
        2 => t!("Green"),
        3 => t!("Yellow"),
        4 => t!("Blue"),
        5 => t!("Magenta"),
        6 => t!("Cyan"),
        7 => t!("White"),
        8 => t!("Bright Black"),
        9 => t!("Bright Red"),
        10 => t!("Bright Green"),
        11 => t!("Bright Yellow"),
        12 => t!("Bright Blue"),
        13 => t!("Bright Magenta"),
        14 => t!("Bright Cyan"),
        _ => t!("Bright White"),
    }
}

const ICON_CHOICES: [IconName; 15] = [
    IconName::Terminal,
    IconName::TerminalAlt,
    IconName::Code,
    IconName::Server,
    IconName::GitBranch,
    IconName::Folder,
    IconName::Settings,
    IconName::Box,
    IconName::Lock,
    IconName::Bell,
    IconName::Eye,
    IconName::Flame,
    IconName::Sparkle,
    IconName::Star,
    IconName::Hash,
];

fn icon_label(icon: IconName) -> SharedString {
    match icon {
        IconName::Terminal => t!("Terminal"),
        IconName::TerminalAlt => t!("Terminal (Alternate)"),
        IconName::Code => t!("Code"),
        IconName::Server => t!("Server"),
        IconName::GitBranch => t!("Git Branch"),
        IconName::Folder => t!("Folder"),
        IconName::Settings => t!("Settings"),
        IconName::Box => t!("Box"),
        IconName::Lock => t!("Lock"),
        IconName::Bell => t!("Bell"),
        IconName::Eye => t!("Eye"),
        IconName::Flame => t!("Flame"),
        IconName::Sparkle => t!("Sparkle"),
        IconName::Star => t!("Star"),
        _ => t!("Hash"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TerminalChoiceKind {
    Color,
    Icon,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    DefaultColor,
    Color(usize),
    DefaultIcon,
    Icon(IconName),
}

pub struct TerminalChoiceModal {
    picker: Entity<Picker<TerminalChoiceDelegate>>,
}

impl TerminalChoiceModal {
    pub fn toggle(
        workspace: &mut Workspace,
        view: Entity<TerminalView>,
        kind: TerminalChoiceKind,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, move |window, cx| {
            TerminalChoiceModal::new(view, kind, window, cx)
        });
    }

    fn new(
        view: Entity<TerminalView>,
        kind: TerminalChoiceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = TerminalChoiceDelegate::new(cx.entity().downgrade(), view, kind, cx);
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for TerminalChoiceModal {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().child(self.picker.clone())
    }
}

impl Focusable for TerminalChoiceModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for TerminalChoiceModal {}
impl ModalView for TerminalChoiceModal {}

pub struct TerminalChoiceDelegate {
    modal: WeakEntity<TerminalChoiceModal>,
    view: Entity<TerminalView>,
    kind: TerminalChoiceKind,
    all: Vec<Choice>,
    matches: Vec<Choice>,
    selected_index: usize,
}

impl TerminalChoiceDelegate {
    fn new(
        modal: WeakEntity<TerminalChoiceModal>,
        view: Entity<TerminalView>,
        kind: TerminalChoiceKind,
        cx: &App,
    ) -> Self {
        let (all, current): (Vec<Choice>, Choice) = match kind {
            TerminalChoiceKind::Color => (
                std::iter::once(Choice::DefaultColor)
                    .chain((0..16).map(Choice::Color))
                    .collect(),
                view.read(cx)
                    .custom_color()
                    .map(Choice::Color)
                    .unwrap_or(Choice::DefaultColor),
            ),
            TerminalChoiceKind::Icon => (
                std::iter::once(Choice::DefaultIcon)
                    .chain(ICON_CHOICES.into_iter().map(Choice::Icon))
                    .collect(),
                view.read(cx)
                    .custom_icon()
                    .map(Choice::Icon)
                    .unwrap_or(Choice::DefaultIcon),
            ),
        };
        let selected_index = all.iter().position(|choice| *choice == current).unwrap_or(0);
        Self {
            modal,
            view,
            kind,
            matches: all.clone(),
            all,
            selected_index,
        }
    }

    fn label(choice: Choice) -> SharedString {
        match choice {
            Choice::DefaultColor | Choice::DefaultIcon => t!("Default"),
            Choice::Color(index) => color_label(index),
            Choice::Icon(icon) => icon_label(icon),
        }
    }
}

impl PickerDelegate for TerminalChoiceDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "terminal choice picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        match self.kind {
            TerminalChoiceKind::Color => t!("Select a color for the terminal").as_ref().into(),
            TerminalChoiceKind::Icon => t!("Select an icon for the terminal").as_ref().into(),
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.matches = self
            .all
            .iter()
            .copied()
            .filter(|choice| {
                query.is_empty() || Self::label(*choice).to_lowercase().contains(&query)
            })
            .collect();
        self.selected_index = self.selected_index.min(self.matches.len().saturating_sub(1));
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(choice) = self.matches.get(self.selected_index).copied() {
            self.view.update(cx, |view, cx| match choice {
                Choice::DefaultColor => view.set_custom_color(None, cx),
                Choice::Color(index) => view.set_custom_color(Some(index), cx),
                Choice::DefaultIcon => view.set_custom_icon(None, cx),
                Choice::Icon(icon) => view.set_custom_icon(Some(icon), cx),
            });
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.modal.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let choice = *self.matches.get(ix)?;
        let leading = match choice {
            Choice::Color(index) => div()
                .size(px(10.))
                .rounded_full()
                .bg(ansi_theme_color(index, cx))
                .into_any_element(),
            Choice::Icon(icon) => Icon::new(icon).size(IconSize::Small).into_any_element(),
            Choice::DefaultColor => Icon::new(IconName::Circle)
                .size(IconSize::XSmall)
                .color(Color::Muted)
                .into_any_element(),
            Choice::DefaultIcon => Icon::new(IconName::Terminal)
                .size(IconSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        };
        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    h_flex()
                        .gap_2()
                        .child(leading)
                        .child(Label::new(Self::label(choice))),
                ),
        )
    }
}
