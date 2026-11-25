use std::{cmp, path::PathBuf, process::ExitStatus, sync::Arc, time::Duration};

use crate::{
    TerminalChoiceKind, TerminalChoiceModal, TerminalView, default_working_directory,
    persistence::{
        SerializedItems, SerializedTerminalPanel, deserialize_terminal_panel, serialize_pane_group,
    },
};
use breadcrumbs::Breadcrumbs;
use collections::{HashMap, HashSet};
use db::kvp::KeyValueStore;
use futures::{channel::oneshot, future::join_all};
use gpui::{
    Action, Anchor, AnyView, App, AsyncApp, AsyncWindowContext, ClickEvent, Context, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, IntoElement, Modifiers, ParentElement, Pixels, Render,
    Styled, Task, TaskExt, WeakEntity, Window, actions,
};
use i18n::{n, t, tc};
use itertools::Itertools;
use project::{Fs, Project};

use settings::{Settings, TerminalDockPosition};
use task::{RevealStrategy, RevealTarget, Shell, ShellBuilder, SpawnInTerminal, TaskId};
use terminal::{Terminal, terminal_settings::TerminalSettings};
use ui::{
    ButtonLike, Clickable, CommonAnimationExt, ContextMenu, FluentBuilder, PopoverMenu,
    SplitButton, Toggleable, Tooltip, prelude::*, right_click_menu,
};
use util::{ResultExt, TryFutureExt, defer};
use workspace::{
    ActivateNextPane, ActivatePane, ActivatePaneDown, ActivatePaneLeft, ActivatePaneRight,
    ActivatePaneUp, ActivatePreviousPane, DraggedTab, ItemId, MoveItemToPane,
    MoveItemToPaneInDirection, MovePaneDown, MovePaneLeft, MovePaneRight, MovePaneUp, Pane,
    PaneGroup, SplitDirection, SplitDown, SplitLeft, SplitMode, SplitRight, SplitUp, SwapPaneDown,
    SwapPaneLeft, SwapPaneRight, SwapPaneUp, ToggleZoom, Workspace,
    dock::{DockPosition, Panel, PanelEvent, PanelHandle},
    item::SerializableItem,
    move_active_item, pane,
};

use anyhow::{Result, anyhow};

const TERMINAL_PANEL_KEY: &str = "TerminalPanel";

actions!(
    terminal_panel,
    [
        /// Toggles the terminal panel.
        Toggle,
        /// Toggles focus on the terminal panel.
        ToggleFocus
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace.register_action(TerminalPanel::new_terminal);
            workspace.register_action(TerminalPanel::open_terminal);
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                if is_enabled_in_workspace(workspace, cx) {
                    workspace.toggle_panel_focus::<TerminalPanel>(window, cx);
                }
            });
            workspace.register_action(|workspace, _: &Toggle, window, cx| {
                if is_enabled_in_workspace(workspace, cx) {
                    if !workspace.toggle_panel_focus::<TerminalPanel>(window, cx) {
                        workspace.close_panel::<TerminalPanel>(window, cx);
                    }
                }
            });
        },
    )
    .detach();
}

struct PortsTabToast;

pub struct TerminalPanel {
    pub(crate) active_pane: Entity<Pane>,
    pub(crate) center: PaneGroup,
    focus_handle: FocusHandle,
    fs: Arc<dyn Fs>,
    workspace: WeakEntity<Workspace>,
    pending_serialization: Task<Option<()>>,
    pending_terminals_to_add: usize,
    restoring: bool,
    _restoration: Task<()>,
    deferred_tasks: HashMap<TaskId, Task<()>>,
    active: bool,
    selected_terminals: Vec<EntityId>,
    selection_anchor: Option<EntityId>,
    groups: Vec<Vec<EntityId>>,
    displayed: Vec<EntityId>,
    pending_split_source: Option<EntityId>,
    terminal_list_width: Option<Pixels>,
    list_width_before_fit: Option<Option<Pixels>>,
    maximized_from: Option<Pixels>,
    show_problems: bool,
    problems_view: Option<Entity<diagnostics::ProblemsTable>>,
    debug_console_view: Option<AnyView>,
    debug_log_view: Option<AnyView>,
    show_debug_console: bool,
    show_debug_logs: bool,
    hidden_tabs: HashSet<PanelTab>,
    show_tab_icons: bool,
}

fn format_problem_count(count: usize) -> String {
    if count >= 1000 {
        format!("{}K+", count / 1000)
    } else {
        count.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PanelTab {
    Terminal,
    Problems,
    Output,
    Ports,
    DebugConsole,
}

impl PanelTab {
    fn all() -> [PanelTab; 5] {
        [
            PanelTab::Terminal,
            PanelTab::Problems,
            PanelTab::Output,
            PanelTab::Ports,
            PanelTab::DebugConsole,
        ]
    }

    fn icon(self) -> IconName {
        match self {
            PanelTab::Terminal => IconName::Terminal,
            PanelTab::Problems => IconName::Warning,
            PanelTab::Output => IconName::File,
            PanelTab::Ports => IconName::Server,
            PanelTab::DebugConsole => IconName::Debug,
        }
    }

    fn label(self) -> SharedString {
        match self {
            PanelTab::Terminal => t!("Terminal"),
            PanelTab::Problems => t!("Problems"),
            PanelTab::Output => t!("Output"),
            PanelTab::Ports => t!("Ports"),
            PanelTab::DebugConsole => t!("Debug Console"),
        }
    }
}

impl TerminalPanel {
    fn show_problems_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.problems_view.is_none() {
            let Some(workspace) = self.workspace.upgrade() else {
                return;
            };
            let project = workspace.read(cx).project().clone();
            let weak_workspace = self.workspace.clone();
            self.problems_view = Some(diagnostics::ProblemsTable::build(
                project,
                weak_workspace,
                window,
                cx,
            ));
        }
        self.show_problems = true;
        self.show_debug_console = false;
        cx.notify();
    }

    pub fn set_debug_views(
        &mut self,
        console: Option<AnyView>,
        logs: Option<AnyView>,
        cx: &mut Context<Self>,
    ) {
        if console.is_some() {
            self.hidden_tabs.remove(&PanelTab::DebugConsole);
        } else {
            self.show_debug_console = false;
            self.show_debug_logs = false;
        }
        self.debug_console_view = console;
        self.debug_log_view = logs;
        cx.notify();
    }

    pub fn show_debug_console(&mut self, logs: bool, cx: &mut Context<Self>) {
        if self.debug_console_view.is_none() {
            return;
        }
        self.show_problems = false;
        self.show_debug_console = true;
        self.show_debug_logs = logs && self.debug_log_view.is_some();
        cx.notify();
    }

    fn toggle_tab_icons(&mut self, cx: &mut Context<Self>) {
        self.show_tab_icons = !self.show_tab_icons;
        cx.notify();
    }

    fn toggle_tab_visibility(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if !self.hidden_tabs.remove(&tab) {
            self.hidden_tabs.insert(tab);
        }
        cx.notify();
    }

    pub fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = workspace.project();
        let pane = new_terminal_pane(workspace.weak_handle(), project.clone(), false, window, cx);
        let center = PaneGroup::new(pane.clone());
        let terminal_panel = Self {
            center,
            active_pane: pane,
            focus_handle: cx.focus_handle(),
            fs: workspace.app_state().fs.clone(),
            workspace: workspace.weak_handle(),
            pending_serialization: Task::ready(None),
            pending_terminals_to_add: 0,
            restoring: false,
            _restoration: Task::ready(()),
            deferred_tasks: HashMap::default(),
            active: false,
            selected_terminals: Vec::new(),
            selection_anchor: None,
            groups: Vec::new(),
            displayed: Vec::new(),
            pending_split_source: None,
            terminal_list_width: None,
            list_width_before_fit: None,
            maximized_from: None,
            show_problems: false,
            problems_view: None,
            debug_console_view: None,
            debug_log_view: None,
            show_debug_console: false,
            show_debug_logs: false,
            hidden_tabs: HashSet::from_iter([
                PanelTab::Output,
                PanelTab::Ports,
                PanelTab::DebugConsole,
            ]),
            show_tab_icons: false,
        };
        cx.subscribe(project, |_, _, event, cx| {
            if matches!(
                event,
                project::Event::DiagnosticsUpdated { .. }
                    | project::Event::DiskBasedDiagnosticsFinished { .. }
            ) {
                cx.notify();
            }
        })
        .detach();
        terminal_panel.apply_tab_bar_buttons(&terminal_panel.active_pane, cx);
        terminal_panel
    }

    pub(crate) fn apply_tab_bar_buttons(
        &self,
        terminal_pane: &Entity<Pane>,
        cx: &mut Context<Self>,
    ) {
        terminal_pane.update(cx, |pane, cx| {
            pane.set_render_trailing_tab_button(cx, move |pane, _window, cx| {
                let focus_handle = pane.focus_handle(cx);
                Some(
                    IconButton::new("new-terminal", IconName::Plus)
                        .icon_size(IconSize::Small)
                        .tooltip(move |_window, cx| {
                            Tooltip::for_action_in(
                                t!("New Terminal"),
                                &workspace::NewTerminal::default(),
                                &focus_handle,
                                cx,
                            )
                        })
                        .on_click(cx.listener(|_pane, _, window, cx| {
                            window.dispatch_action(
                                workspace::NewTerminal::default().boxed_clone(),
                                cx,
                            );
                        }))
                        .into_any_element(),
                )
            });
            pane.set_render_tab_bar_buttons(cx, move |pane, window, cx| {
                let split_context = pane
                    .active_item()
                    .and_then(|item| item.downcast::<TerminalView>())
                    .map(|terminal_view| terminal_view.read(cx).focus_handle.clone());
                let has_focused_rename_editor = pane
                    .active_item()
                    .and_then(|item| item.downcast::<TerminalView>())
                    .is_some_and(|view| view.read(cx).rename_editor_is_focused(window, cx));
                if !pane.has_focus(window, cx)
                    && !pane.context_menu_focused(window, cx)
                    && !has_focused_rename_editor
                {
                    return (None, None);
                }
                let right_children = h_flex()
                    .gap(DynamicSpacing::Base02.rems(cx))
                    .child(
                        PopoverMenu::new("terminal-pane-tab-bar-split")
                            .trigger_with_tooltip(
                                IconButton::new("terminal-pane-split", IconName::Split)
                                    .icon_size(IconSize::Small),
                                Tooltip::text(t!("Split Pane")),
                            )
                            .anchor(Anchor::TopRight)
                            .with_handle(pane.split_item_context_menu_handle.clone())
                            .menu({
                                move |window, cx| {
                                    ContextMenu::build(window, cx, |menu, _, _| {
                                        menu.when_some(
                                            split_context.clone(),
                                            |menu, split_context| menu.context(split_context),
                                        )
                                        .action(
                                            t!("Split Right"),
                                            SplitRight::default().boxed_clone(),
                                        )
                                        .action(
                                            t!("Split Left"),
                                            SplitLeft::default().boxed_clone(),
                                        )
                                        .action(t!("Split Up"), SplitUp::default().boxed_clone())
                                        .action(
                                            t!("Split Down"),
                                            SplitDown::default().boxed_clone(),
                                        )
                                    })
                                    .into()
                                }
                            }),
                    )
                    .child({
                        let zoomed = pane.is_zoomed();
                        IconButton::new("toggle_zoom", IconName::Maximize)
                            .icon_size(IconSize::Small)
                            .toggle_state(zoomed)
                            .selected_icon(IconName::Minimize)
                            .on_click(cx.listener(|pane, _, window, cx| {
                                pane.toggle_zoom(&workspace::ToggleZoom, window, cx);
                            }))
                            .tooltip(move |_window, cx| {
                                Tooltip::for_action(
                                    if zoomed {
                                        tc!("pane", "Zoom Out")
                                    } else {
                                        tc!("pane", "Zoom In")
                                    },
                                    &ToggleZoom,
                                    cx,
                                )
                            })
                    })
                    .into_any_element()
                    .into();
                (None, right_children)
            });
        });
    }

    fn serialization_key(workspace: &Workspace) -> Option<String> {
        workspace
            .database_id()
            .map(|id| i64::from(id).to_string())
            .or(workspace.session_id())
            .map(|id| format!("{:?}-{:?}", TERMINAL_PANEL_KEY, id))
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        let terminal_panel = workspace.update_in(&mut cx, |workspace, window, cx| {
            cx.new(|cx| TerminalPanel::new(workspace, window, cx))
        })?;

        workspace
            .update(&mut cx, |workspace, _| {
                workspace.set_terminal_provider(TerminalProvider(terminal_panel.clone()))
            })
            .ok();

        terminal_panel.update_in(&mut cx, |panel, window, cx| {
            panel.restoring = true;
            panel._restoration = cx.spawn_in(window, {
                let workspace = workspace.clone();
                async move |terminal_panel, cx| {
                    let restored =
                        Self::restore_serialized_state(workspace, terminal_panel.clone(), cx)
                            .await
                            .log_err()
                            .unwrap_or(false);
                    let default_shell_task = terminal_panel
                        .update_in(cx, |terminal_panel, window, cx| {
                            terminal_panel.finish_restoration(restored, window, cx)
                        })
                        .ok()
                        .flatten();
                    if let Some(task) = default_shell_task {
                        task.await.log_err();
                    }
                }
            });
        })?;

        Ok(terminal_panel)
    }

    fn finish_restoration(
        &mut self,
        restored: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<WeakEntity<Terminal>>>> {
        self.restoring = false;
        self.groups.clear();
        self.displayed.clear();
        self.sync_groups(cx);
        self.displayed = self.pane_active_ids(cx);
        let has_terminals = self
            .center
            .panes()
            .into_iter()
            .any(|pane| pane.read(cx).items_len() > 0);
        if restored || has_terminals {
            self.serialize(cx);
        }
        cx.notify();
        if self.active && self.has_no_terminals(cx) {
            let working_directory = self
                .workspace
                .update(cx, |workspace, cx| default_working_directory(workspace, cx))
                .ok()
                .flatten();
            Some(self.add_terminal_shell(
                false,
                working_directory,
                RevealStrategy::Always,
                window,
                cx,
            ))
        } else {
            None
        }
    }

    async fn restore_serialized_state(
        workspace: WeakEntity<Workspace>,
        terminal_panel: WeakEntity<Self>,
        cx: &mut AsyncWindowContext,
    ) -> Result<bool> {
        let mut restored = false;
        if let Some((database_id, serialization_key, kvp)) = workspace
            .read_with(cx, |workspace, cx| {
                workspace
                    .database_id()
                    .zip(TerminalPanel::serialization_key(workspace))
                    .map(|(id, key)| (id, key, KeyValueStore::global(cx)))
            })
            .ok()
            .flatten()
            && let Some(serialized_panel) = cx
                .background_spawn(async move { kvp.read_kvp(&serialization_key) })
                .await
                .log_err()
                .flatten()
                .map(|panel| serde_json::from_str::<SerializedTerminalPanel>(&panel))
                .transpose()
                .log_err()
                .flatten()
        {
            let started_at = std::time::Instant::now();
            let deserialized = workspace
                .update_in(cx, |workspace, window, cx| {
                    deserialize_terminal_panel(
                        workspace.weak_handle(),
                        workspace.project().clone(),
                        database_id,
                        serialized_panel,
                        terminal_panel.clone(),
                        window,
                        cx,
                    )
                })?
                .await;
            if let Some(restored_terminals) = deserialized.log_err() {
                restored = restored_terminals > 0;
                log::debug!(
                    "terminal panel: restored {restored_terminals} serialized terminal(s) in {:?}",
                    started_at.elapsed()
                );
            }
        }

        // Since panels/docks are loaded outside from the workspace, we cleanup here, instead of through the workspace.
        let cleanup = workspace.update_in(cx, |workspace, window, cx| {
            let alive_item_ids = terminal_panel.upgrade().map(|terminal_panel| {
                terminal_panel
                    .read(cx)
                    .center
                    .panes()
                    .into_iter()
                    .flat_map(|pane| pane.read(cx).items())
                    .map(|item| item.item_id().as_u64() as ItemId)
                    .collect::<Vec<_>>()
            });
            alive_item_ids
                .zip(workspace.database_id())
                .map(|(alive_item_ids, workspace_id)| {
                    let cleanup_task =
                        TerminalView::cleanup(workspace_id, alive_item_ids.clone(), window, cx);
                    (cleanup_task, alive_item_ids)
                })
        })?;
        if let Some((cleanup_task, alive_item_ids)) = cleanup {
            cleanup_task.await.log_err();
            terminal_panel
                .update(cx, |terminal_panel, cx| {
                    let terminals_to_reserialize = terminal_panel
                        .center
                        .panes()
                        .into_iter()
                        .flat_map(|pane| {
                            pane.read(cx)
                                .items()
                                .filter(|item| {
                                    !alive_item_ids.contains(&(item.item_id().as_u64() as ItemId))
                                })
                                .filter_map(|item| item.act_as::<TerminalView>(cx))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    for terminal_view in terminals_to_reserialize {
                        terminal_view.update(cx, |terminal_view, cx| {
                            terminal_view.mark_needs_serialize(cx)
                        });
                    }
                })
                .ok();
        }

        let should_focus = workspace
            .update_in(cx, |workspace, window, cx| {
                !workspace.has_active_modal(window, cx)
                    && terminal_panel.upgrade().is_some_and(|terminal_panel| {
                        workspace.active_item(cx).is_none()
                            && workspace
                                .is_dock_at_position_open(terminal_panel.position(window, cx), cx)
                    })
            })
            .unwrap_or(false);
        if should_focus {
            terminal_panel
                .update_in(cx, |panel, window, cx| {
                    panel.active_pane.update(cx, |pane, cx| {
                        pane.focus_active_item(window, cx);
                    });
                })
                .ok();
        }
        Ok(restored)
    }

    fn handle_pane_event(
        &mut self,
        pane: &Entity<Pane>,
        event: &pane::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            pane::Event::ActivateItem { .. } => self.serialize(cx),
            pane::Event::RemovedItem { .. } => {
                self.serialize(cx);
                self.schedule_normalize(None, window, cx);
            }
            pane::Event::Remove { focus_on_pane } => {
                let pane_count_before_removal = self.center.panes().len();
                let _removal_result = self.center.remove(pane, cx);
                if pane_count_before_removal == 1 {
                    self.center.first_pane().update(cx, |pane, cx| {
                        pane.set_zoomed(false, cx);
                    });
                    cx.emit(PanelEvent::Close);
                } else if let Some(focus_on_pane) =
                    focus_on_pane.as_ref().or_else(|| self.center.panes().pop())
                {
                    focus_on_pane.focus_handle(cx).focus(window, cx);
                }
                self.schedule_normalize(None, window, cx);
            }
            pane::Event::ZoomIn => {
                for pane in self.center.panes() {
                    pane.update(cx, |pane, cx| {
                        pane.set_zoomed(true, cx);
                    })
                }
                cx.emit(PanelEvent::ZoomIn);
                cx.notify();
            }
            pane::Event::ZoomOut => {
                for pane in self.center.panes() {
                    pane.update(cx, |pane, cx| {
                        pane.set_zoomed(false, cx);
                    })
                }
                cx.emit(PanelEvent::ZoomOut);
                cx.notify();
            }
            pane::Event::AddItem { item } => {
                if let Some(workspace) = self.workspace.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        item.added_to_pane(workspace, pane.clone(), window, cx)
                    })
                }
                self.serialize(cx);
                if !self.restoring {
                    let id = item.item_id();
                    let known = self.groups.iter().any(|group| group.contains(&id));
                    let intent = (!known
                        && self.pending_split_source.is_none()
                        && !self.groups.is_empty())
                    .then_some(id);
                    self.schedule_normalize(intent, window, cx);
                }
            }
            &pane::Event::Split { direction, mode } => {
                match mode {
                    SplitMode::ClonePane | SplitMode::EmptyPane => {
                        let clone = matches!(mode, SplitMode::ClonePane);
                        self.sync_groups(cx);
                        self.pending_split_source = pane
                            .read(cx)
                            .active_item()
                            .map(|item| item.item_id());
                        let new_pane = self.new_pane_with_active_terminal(clone, window, cx);
                        let pane = pane.clone();
                        cx.spawn_in(window, async move |panel, cx| {
                            let Some(new_pane) = new_pane.await else {
                                panel
                                    .update(cx, |panel, _| panel.pending_split_source = None)
                                    .ok();
                                return;
                            };
                            panel
                                .update_in(cx, |panel, window, cx| {
                                    panel.center.split(&pane, &new_pane, direction, cx);
                                    window.focus(&new_pane.focus_handle(cx), cx);
                                    panel.schedule_normalize(None, window, cx);
                                })
                                .ok();
                        })
                        .detach();
                    }
                    SplitMode::MovePane => {
                        let Some(item) =
                            pane.update(cx, |pane, cx| pane.take_active_item(window, cx))
                        else {
                            return;
                        };
                        let Ok(project) = self
                            .workspace
                            .update(cx, |workspace, _| workspace.project().clone())
                        else {
                            return;
                        };
                        let new_pane =
                            new_terminal_pane(self.workspace.clone(), project, false, window, cx);
                        self.apply_tab_bar_buttons(&new_pane, cx);
                        new_pane.update(cx, |pane, cx| {
                            pane.add_item(item, true, true, None, window, cx);
                        });
                        self.center.split(&pane, &new_pane, direction, cx);
                        window.focus(&new_pane.focus_handle(cx), cx);
                    }
                };
            }
            pane::Event::Focus => {
                self.active_pane = pane.clone();
            }
            pane::Event::ItemPinned | pane::Event::ItemUnpinned => {
                self.serialize(cx);
            }

            _ => {}
        }
    }

    fn new_pane_with_active_terminal(
        &mut self,
        clone: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Pane>>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(None);
        };
        let workspace = workspace.read(cx);
        let database_id = workspace.database_id();
        let weak_workspace = self.workspace.clone();
        let project = workspace.project().clone();
        let active_pane = &self.active_pane;
        let terminal_view = if clone {
            active_pane
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<TerminalView>())
        } else {
            None
        };
        let working_directory = if clone {
            terminal_view
                .as_ref()
                .and_then(|terminal_view| {
                    terminal_view
                        .read(cx)
                        .terminal()
                        .read(cx)
                        .working_directory()
                })
                .or_else(|| default_working_directory(workspace, cx))
        } else {
            default_working_directory(workspace, cx)
        };

        let is_zoomed = if clone {
            active_pane.read(cx).is_zoomed()
        } else {
            false
        };
        cx.spawn_in(window, async move |panel, cx| {
            let terminal = project
                .update(cx, |project, cx| match terminal_view {
                    Some(view) => project.clone_terminal(
                        &view.read(cx).terminal.clone(),
                        cx,
                        working_directory,
                    ),
                    None => project.create_terminal_shell(working_directory, cx),
                })
                .await
                .log_err()?;

            panel
                .update_in(cx, move |terminal_panel, window, cx| {
                    let terminal_view = Box::new(cx.new(|cx| {
                        TerminalView::new(
                            terminal.clone(),
                            weak_workspace.clone(),
                            database_id,
                            project.downgrade(),
                            window,
                            cx,
                        )
                    }));
                    let pane = new_terminal_pane(weak_workspace, project, is_zoomed, window, cx);
                    terminal_panel.apply_tab_bar_buttons(&pane, cx);
                    pane.update(cx, |pane, cx| {
                        pane.add_item(terminal_view, true, true, None, window, cx);
                    });
                    Some(pane)
                })
                .ok()
                .flatten()
        })
    }

    pub fn open_terminal(
        workspace: &mut Workspace,
        action: &workspace::OpenTerminal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let Some(terminal_panel) = workspace.panel::<Self>(cx) else {
            return;
        };

        terminal_panel
            .update(cx, |panel, cx| {
                panel.add_terminal_shell(
                    action.local,
                    Some(action.working_directory.clone()),
                    RevealStrategy::Always,
                    window,
                    cx,
                )
            })
            .detach_and_log_err(cx);
    }

    pub fn spawn_task(
        &mut self,
        task: &SpawnInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow!("failed to read workspace")));
        };

        let project = workspace.read(cx).project().read(cx);

        let remote_client = project.remote_client();
        let is_windows = project.path_style(cx).is_windows();
        let remote_shell = remote_client
            .as_ref()
            .and_then(|remote_client| remote_client.read(cx).shell());

        let shell = if let Some(remote_shell) = remote_shell
            && task.shell == Shell::System
        {
            Shell::Program(remote_shell)
        } else {
            task.shell.clone()
        };

        let task = prepare_task_for_spawn(task, &shell, is_windows);

        if task.allow_concurrent_runs && task.use_new_terminal {
            return self.spawn_in_new_terminal(task, window, cx);
        }

        let mut terminals_for_task = self.terminals_for_task(&task.full_label, cx);
        let Some(existing) = terminals_for_task.pop() else {
            return self.spawn_in_new_terminal(task, window, cx);
        };

        let (existing_item_index, task_pane, existing_terminal) = existing;
        if task.allow_concurrent_runs {
            return self.replace_terminal(
                task,
                task_pane,
                existing_item_index,
                existing_terminal,
                window,
                cx,
            );
        }

        let (tx, rx) = oneshot::channel();

        self.deferred_tasks.insert(
            task.id.clone(),
            cx.spawn_in(window, async move |terminal_panel, cx| {
                wait_for_terminals_tasks(terminals_for_task, cx).await;
                let task = terminal_panel.update_in(cx, |terminal_panel, window, cx| {
                    if task.use_new_terminal {
                        terminal_panel.spawn_in_new_terminal(task, window, cx)
                    } else {
                        terminal_panel.replace_terminal(
                            task,
                            task_pane,
                            existing_item_index,
                            existing_terminal,
                            window,
                            cx,
                        )
                    }
                });
                if let Ok(task) = task {
                    tx.send(task.await).ok();
                }
            }),
        );

        cx.spawn(async move |_, _| rx.await?)
    }

    fn spawn_in_new_terminal(
        &mut self,
        spawn_task: SpawnInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let reveal = spawn_task.reveal;
        let reveal_target = spawn_task.reveal_target;
        match reveal_target {
            RevealTarget::Center => self
                .workspace
                .update(cx, |workspace, cx| {
                    Self::add_center_terminal(workspace, window, cx, |project, cx| {
                        project.create_terminal_task(spawn_task, cx)
                    })
                })
                .unwrap_or_else(|e| Task::ready(Err(e))),
            RevealTarget::Dock => self.add_terminal_task(spawn_task, reveal, window, cx),
        }
    }

    /// Create a new Terminal in the current working directory or the user's home directory
    fn new_terminal(
        workspace: &mut Workspace,
        action: &workspace::NewTerminal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let center_pane = workspace.active_pane();
        let center_pane_has_focus = center_pane.focus_handle(cx).contains_focused(window, cx);
        let active_center_item_is_terminal = center_pane
            .read(cx)
            .active_item()
            .is_some_and(|item| item.downcast::<TerminalView>().is_some());

        if center_pane_has_focus && active_center_item_is_terminal {
            let working_directory = default_working_directory(workspace, cx);
            let local = action.local;
            Self::add_center_terminal(workspace, window, cx, move |project, cx| {
                if local {
                    project.create_local_terminal(cx)
                } else {
                    project.create_terminal_shell(working_directory, cx)
                }
            })
            .detach_and_log_err(cx);
            return;
        }

        let Some(terminal_panel) = workspace.panel::<Self>(cx) else {
            return;
        };

        terminal_panel
            .update(cx, |this, cx| {
                this.add_terminal_shell(
                    action.local,
                    default_working_directory(workspace, cx),
                    RevealStrategy::Always,
                    window,
                    cx,
                )
            })
            .detach_and_log_err(cx);
    }

    fn terminals_for_task(
        &self,
        label: &str,
        cx: &mut App,
    ) -> Vec<(usize, Entity<Pane>, Entity<TerminalView>)> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };

        let pane_terminal_views = |pane: Entity<Pane>| {
            pane.read(cx)
                .items()
                .enumerate()
                .filter_map(|(index, item)| Some((index, item.act_as::<TerminalView>(cx)?)))
                .filter_map(|(index, terminal_view)| {
                    let task_state = terminal_view.read(cx).terminal().read(cx).task()?;
                    if &task_state.spawned_task.full_label == label {
                        Some((index, terminal_view))
                    } else {
                        None
                    }
                })
                .map(move |(index, terminal_view)| (index, pane.clone(), terminal_view))
        };

        self.center
            .panes()
            .into_iter()
            .cloned()
            .flat_map(pane_terminal_views)
            .chain(
                workspace
                    .read(cx)
                    .panes()
                    .iter()
                    .cloned()
                    .flat_map(pane_terminal_views),
            )
            .sorted_by_key(|(_, _, terminal_view)| terminal_view.entity_id())
            .collect()
    }

    fn activate_terminal_view(
        &self,
        pane: &Entity<Pane>,
        item_index: usize,
        focus: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        pane.update(cx, |pane, cx| {
            pane.activate_item(item_index, true, focus, window, cx)
        })
    }

    pub fn add_center_terminal(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        create_terminal: impl FnOnce(
            &mut Project,
            &mut Context<Project>,
        ) -> Task<Result<Entity<Terminal>>>
        + 'static,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        if !is_enabled_in_workspace(workspace, cx) {
            return Task::ready(Err(anyhow!(
                "terminal not yet supported for remote projects"
            )));
        }
        let project = workspace.project().downgrade();
        cx.spawn_in(window, async move |workspace, cx| {
            let terminal = project.update(cx, create_terminal)?.await?;

            workspace.update_in(cx, |workspace, window, cx| {
                let terminal_view = cx.new(|cx| {
                    TerminalView::new(
                        terminal.clone(),
                        workspace.weak_handle(),
                        workspace.database_id(),
                        workspace.project().downgrade(),
                        window,
                        cx,
                    )
                });
                // Don't steal focus from an open modal (e.g. the command palette):
                // a background terminal can finish starting up after the user has
                // moved on, and focusing it would dismiss whatever they opened.
                let focus_item = !workspace.has_active_modal(window, cx);
                workspace.add_item_to_active_pane(
                    Box::new(terminal_view),
                    None,
                    focus_item,
                    window,
                    cx,
                );
            })?;
            Ok(terminal.downgrade())
        })
    }

    pub fn add_terminal_task(
        &mut self,
        task: SpawnInTerminal,
        reveal_strategy: RevealStrategy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let workspace = self.workspace.clone();
        self.spawn_pending_terminal(window, cx, async move |terminal_panel, cx| {
            if workspace.update(cx, |workspace, cx| !is_enabled_in_workspace(workspace, cx))? {
                anyhow::bail!("terminal not yet supported for remote projects");
            }
            let project = workspace.read_with(cx, |workspace, _| workspace.project().clone())?;
            let terminal = project
                .update(cx, |project, cx| project.create_terminal_task(task, cx))
                .await?;
            let pane = terminal_panel
                .read_with(cx, |terminal_panel, _| terminal_panel.active_pane.clone())?;
            workspace.update_in(cx, |workspace, window, cx| {
                let terminal_view = Box::new(cx.new(|cx| {
                    TerminalView::new(
                        terminal.clone(),
                        workspace.weak_handle(),
                        workspace.database_id(),
                        workspace.project().downgrade(),
                        window,
                        cx,
                    )
                }));

                let take_focus = reveal_strategy == RevealStrategy::Always
                    && !workspace.has_active_modal(window, cx);
                match reveal_strategy {
                    RevealStrategy::Always if take_focus => {
                        workspace.focus_panel::<Self>(window, cx);
                    }
                    RevealStrategy::Always | RevealStrategy::NoFocus => {
                        workspace.open_panel::<Self>(window, cx);
                    }
                    RevealStrategy::Never => {}
                }

                pane.update(cx, |pane, cx| {
                    pane.add_item(terminal_view, true, take_focus, None, window, cx);
                });

                terminal.downgrade()
            })
        })
    }

    fn add_terminal_shell(
        &mut self,
        force_local: bool,
        cwd: Option<PathBuf>,
        reveal_strategy: RevealStrategy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let workspace = self.workspace.clone();
        self.spawn_pending_terminal(window, cx, async move |terminal_panel, cx| {
            if workspace.update(cx, |workspace, cx| !is_enabled_in_workspace(workspace, cx))? {
                anyhow::bail!("terminal not yet supported for collaborative projects");
            }
            let project = workspace.read_with(cx, |workspace, _| workspace.project().clone())?;
            let terminal = if force_local {
                project
                    .update(cx, |project, cx| project.create_local_terminal(cx))
                    .await
            } else {
                project
                    .update(cx, |project, cx| project.create_terminal_shell(cwd, cx))
                    .await
            };

            let pane = terminal_panel
                .read_with(cx, |terminal_panel, _| terminal_panel.active_pane.clone())?;
            match terminal {
                Ok(terminal) => workspace.update_in(cx, |workspace, window, cx| {
                    let terminal_view = Box::new(cx.new(|cx| {
                        TerminalView::new(
                            terminal.clone(),
                            workspace.weak_handle(),
                            workspace.database_id(),
                            workspace.project().downgrade(),
                            window,
                            cx,
                        )
                    }));

                    let take_focus = reveal_strategy == RevealStrategy::Always
                        && !workspace.has_active_modal(window, cx);
                    match reveal_strategy {
                        RevealStrategy::Always if take_focus => {
                            workspace.focus_panel::<Self>(window, cx);
                        }
                        RevealStrategy::Always | RevealStrategy::NoFocus => {
                            workspace.open_panel::<Self>(window, cx);
                        }
                        RevealStrategy::Never => {}
                    }

                    pane.update(cx, |pane, cx| {
                        pane.add_item(terminal_view, true, take_focus, None, window, cx);
                    });

                    terminal.downgrade()
                }),
                Err(error) => {
                    pane.update_in(cx, |pane, window, cx| {
                        let focus = pane.has_focus(window, cx);
                        let failed_to_spawn = cx.new(|cx| FailedToSpawnTerminal {
                            error: error.to_string(),
                            focus_handle: cx.focus_handle(),
                        });
                        pane.add_item(Box::new(failed_to_spawn), true, focus, None, window, cx);
                    })?;
                    Err(error)
                }
            }
        })
    }

    fn spawn_pending_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        create_terminal: impl AsyncFnOnce(
            WeakEntity<Self>,
            &mut AsyncWindowContext,
        ) -> Result<WeakEntity<Terminal>>
        + 'static,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        self.pending_terminals_to_add += 1;
        cx.notify();
        let decrement_when_cancelled = defer({
            let terminal_panel = cx.weak_entity();
            let cx = cx.to_async();
            move || {
                cx.spawn(async move |cx| {
                    terminal_panel
                        .update(cx, |terminal_panel, cx| {
                            terminal_panel.finish_pending_terminal(cx)
                        })
                        .ok();
                })
                .detach();
            }
        });
        cx.spawn_in(window, async move |terminal_panel, cx| {
            let result = create_terminal(terminal_panel.clone(), cx).await;
            decrement_when_cancelled.abort();
            terminal_panel
                .update(cx, |terminal_panel, cx| {
                    terminal_panel.finish_pending_terminal(cx);
                    if result.is_ok() {
                        terminal_panel.serialize(cx);
                    }
                })
                .ok();
            result
        })
    }

    fn finish_pending_terminal(&mut self, cx: &mut Context<Self>) {
        self.pending_terminals_to_add = self.pending_terminals_to_add.saturating_sub(1);
        cx.notify();
    }

    fn serialize(&mut self, cx: &mut Context<Self>) {
        if self.restoring {
            return;
        }
        let Some(serialization_key) = self
            .workspace
            .read_with(cx, |workspace, _| {
                TerminalPanel::serialization_key(workspace)
            })
            .ok()
            .flatten()
        else {
            return;
        };
        let kvp = KeyValueStore::global(cx);
        self.pending_serialization = cx.spawn(async move |terminal_panel, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            let terminal_panel = terminal_panel.upgrade()?;
            let items = terminal_panel.update(cx, |terminal_panel, cx| {
                SerializedItems::WithSplits(serialize_pane_group(
                    &terminal_panel.center,
                    &terminal_panel.active_pane,
                    cx,
                ))
            });
            cx.background_spawn(
                async move {
                    kvp.write_kvp(
                        serialization_key,
                        serde_json::to_string(&SerializedTerminalPanel {
                            items,
                            active_item_id: None,
                        })?,
                    )
                    .await?;
                    anyhow::Ok(())
                }
                .log_err(),
            )
            .await;
            Some(())
        });
    }

    fn replace_terminal(
        &self,
        spawn_task: SpawnInTerminal,
        task_pane: Entity<Pane>,
        terminal_item_index: usize,
        terminal_to_replace: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let reveal = spawn_task.reveal;
        let task_workspace = self.workspace.clone();
        cx.spawn_in(window, async move |terminal_panel, cx| {
            let project = terminal_panel.update(cx, |this, cx| {
                this.workspace
                    .update(cx, |workspace, _| workspace.project().clone())
            })??;
            let new_terminal = project
                .update(cx, |project, cx| {
                    project.create_terminal_task(spawn_task, cx)
                })
                .await?;
            terminal_to_replace.update_in(cx, |terminal_to_replace, window, cx| {
                terminal_to_replace.set_terminal(new_terminal.clone(), window, cx);
            })?;

            let reveal_target = terminal_panel.update(cx, |panel, _| {
                if panel.center.panes().iter().any(|p| **p == task_pane) {
                    RevealTarget::Dock
                } else {
                    RevealTarget::Center
                }
            })?;

            match reveal {
                RevealStrategy::Always => match reveal_target {
                    RevealTarget::Center => {
                        task_workspace.update_in(cx, |workspace, window, cx| {
                            let did_activate = workspace.activate_item(
                                &terminal_to_replace,
                                true,
                                true,
                                window,
                                cx,
                            );

                            anyhow::ensure!(did_activate, "Failed to retrieve terminal pane");

                            anyhow::Ok(())
                        })??;
                    }
                    RevealTarget::Dock => {
                        terminal_panel.update_in(cx, |terminal_panel, window, cx| {
                            terminal_panel.activate_terminal_view(
                                &task_pane,
                                terminal_item_index,
                                true,
                                window,
                                cx,
                            )
                        })?;

                        cx.spawn(async move |cx| {
                            task_workspace
                                .update_in(cx, |workspace, window, cx| {
                                    workspace.focus_panel::<Self>(window, cx)
                                })
                                .ok()
                        })
                        .detach();
                    }
                },
                RevealStrategy::NoFocus => match reveal_target {
                    RevealTarget::Center => {
                        task_workspace.update_in(cx, |workspace, window, cx| {
                            workspace.active_pane().focus_handle(cx).focus(window, cx);
                        })?;
                    }
                    RevealTarget::Dock => {
                        terminal_panel.update_in(cx, |terminal_panel, window, cx| {
                            terminal_panel.activate_terminal_view(
                                &task_pane,
                                terminal_item_index,
                                false,
                                window,
                                cx,
                            )
                        })?;

                        cx.spawn(async move |cx| {
                            task_workspace
                                .update_in(cx, |workspace, window, cx| {
                                    workspace.open_panel::<Self>(window, cx)
                                })
                                .ok()
                        })
                        .detach();
                    }
                },
                RevealStrategy::Never => {}
            }

            Ok(new_terminal.downgrade())
        })
    }

    fn has_no_terminals(&self, cx: &App) -> bool {
        self.active_pane.read(cx).items_len() == 0 && self.pending_terminals_to_add == 0
    }

    /// Returns all panes in the terminal panel.
    pub fn panes(&self) -> Vec<&Entity<Pane>> {
        self.center.panes()
    }

    /// Returns all non-empty terminal selections from all terminal views in all panes.
    pub fn terminal_selections(&self, cx: &App) -> Vec<String> {
        self.center
            .panes()
            .iter()
            .flat_map(|pane| {
                pane.read(cx).items().filter_map(|item| {
                    let terminal_view = item.downcast::<crate::TerminalView>()?;
                    terminal_view
                        .read(cx)
                        .terminal()
                        .read(cx)
                        .last_content
                        .selection_text
                        .clone()
                        .filter(|text| !text.is_empty())
                })
            })
            .collect()
    }

    fn is_enabled(&self, cx: &App) -> bool {
        self.workspace
            .upgrade()
            .is_some_and(|workspace| is_enabled_in_workspace(workspace.read(cx), cx))
    }

    fn activate_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self
            .center
            .find_pane_in_direction(&self.active_pane, direction, cx)
        {
            window.focus(&pane.focus_handle(cx), cx);
        } else {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.activate_pane_in_direction(direction, window, cx)
                })
                .ok();
        }
    }

    fn swap_pane_in_direction(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if let Some(to) = self
            .center
            .find_pane_in_direction(&self.active_pane, direction, cx)
            .cloned()
        {
            self.center.swap(&self.active_pane, &to, cx);
            cx.notify();
        }
    }

    fn move_pane_to_border(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if self
            .center
            .move_to_border(&self.active_pane, direction, cx)
            .unwrap()
        {
            cx.notify();
        }
    }
}

/// Prepares a `SpawnInTerminal` by computing the command, args, and command_label
/// based on the shell configuration. This is a pure function that can be tested
/// without spawning actual terminals.
pub fn prepare_task_for_spawn(
    task: &SpawnInTerminal,
    shell: &Shell,
    is_windows: bool,
) -> SpawnInTerminal {
    let builder = ShellBuilder::new(shell, is_windows);
    let command_label = builder.command_label(task.command.as_deref().unwrap_or(""));
    let (command, args) = builder.build_no_quote(task.command.clone(), &task.args);

    SpawnInTerminal {
        command_label,
        command: Some(command),
        args,
        ..task.clone()
    }
}

fn is_enabled_in_workspace(workspace: &Workspace, cx: &App) -> bool {
    workspace.project().read(cx).supports_terminal(cx)
}

pub fn new_terminal_pane(
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    zoomed: bool,
    window: &mut Window,
    cx: &mut Context<TerminalPanel>,
) -> Entity<Pane> {
    let terminal_panel = cx.entity();
    let pane = cx.new(|cx| {
        let mut pane = Pane::new(
            workspace.clone(),
            project.clone(),
            Default::default(),
            None,
            workspace::NewTerminal::default().boxed_clone(),
            false,
            window,
            cx,
        );
        pane.set_zoomed(zoomed, cx);
        pane.set_can_navigate(false, cx);
        pane.display_nav_history_buttons(None);
        // Terminals are listed in the panel's right-hand list instead (VS Code style).
        pane.set_should_display_tab_bar(|_, _| false);
        pane.set_zoom_out_on_close(false);

        let split_closure_terminal_panel = terminal_panel.downgrade();
        pane.set_can_split(Some(Arc::new(move |pane, dragged_item, _window, cx| {
            if let Some(tab) = dragged_item.downcast_ref::<DraggedTab>() {
                let is_current_pane = tab.pane == cx.entity();
                let Some(can_drag_away) = split_closure_terminal_panel
                    .read_with(cx, |terminal_panel, _| {
                        let current_panes = terminal_panel.center.panes();
                        !current_panes.contains(&&tab.pane)
                            || current_panes.len() > 1
                            || (!is_current_pane || pane.items_len() > 1)
                    })
                    .ok()
                else {
                    return false;
                };
                if can_drag_away {
                    let item = if is_current_pane {
                        pane.item_for_index(tab.ix)
                    } else {
                        tab.pane.read(cx).item_for_index(tab.ix)
                    };
                    if let Some(item) = item {
                        return item.downcast::<TerminalView>().is_some();
                    }
                }
            }
            false
        })));

        let toolbar = pane.toolbar().clone();
        if let Some(callbacks) = cx.try_global::<workspace::PaneSearchBarCallbacks>() {
            let languages = Some(project.read(cx).languages().clone());
            (callbacks.setup_search_bar)(languages, &toolbar, window, cx);
        }
        let breadcrumbs = cx.new(|_| Breadcrumbs::new());
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_item(breadcrumbs, window, cx);
        });

        pane
    });

    cx.subscribe_in(&pane, window, TerminalPanel::handle_pane_event)
        .detach();
    cx.observe(&pane, |_, _, cx| cx.notify()).detach();

    pane
}

async fn wait_for_terminals_tasks(
    terminals_for_task: Vec<(usize, Entity<Pane>, Entity<TerminalView>)>,
    cx: &mut AsyncApp,
) {
    let pending_tasks = terminals_for_task.iter().map(|(_, _, terminal)| {
        terminal.update(cx, |terminal_view, cx| {
            terminal_view
                .terminal()
                .update(cx, |terminal, cx| terminal.wait_for_completed_task(cx))
        })
    });
    join_all(pending_tasks).await;
}

struct FailedToSpawnTerminal {
    error: String,
    focus_handle: FocusHandle,
}

impl Focusable for FailedToSpawnTerminal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FailedToSpawnTerminal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popover_menu = PopoverMenu::new("settings-popover")
            .trigger(
                IconButton::new("icon-button-popover", IconName::ChevronDown)
                    .icon_size(IconSize::XSmall),
            )
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |context_menu, _, _| {
                    context_menu
                        .action(t!("Open Settings"), crab_actions::OpenSettings.boxed_clone())
                        .action(
                            t!("Edit settings.json"),
                            crab_actions::OpenSettingsFile.boxed_clone(),
                        )
                }))
            })
            .anchor(Anchor::TopRight)
            .offset(gpui::Point {
                x: px(0.0),
                y: px(2.0),
            });

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_4()
            .items_center()
            .justify_center()
            .bg(cx.theme().colors().editor_background)
            .child(
                v_flex()
                    .max_w_112()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .child(Label::new(t!("Failed to spawn terminal")))
                    .child(
                        Label::new(self.error.to_string())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .mb_4(),
                    )
                    .child(SplitButton::new(
                        ButtonLike::new("open-settings-ui")
                            .child(Label::new(t!("Edit Settings")).size(LabelSize::Small))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(crab_actions::OpenSettings.boxed_clone(), cx);
                            }),
                        popover_menu.into_any_element(),
                    )),
            )
    }
}

impl EventEmitter<()> for FailedToSpawnTerminal {}

impl workspace::Item for FailedToSpawnTerminal {
    type Event = ();

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        t!("Failed to spawn terminal")
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl TerminalPanel {
    fn apply_layout(
        &mut self,
        selected: Vec<EntityId>,
        active: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(project) = self
            .workspace
            .update(cx, |workspace, _| workspace.project().clone())
        else {
            return;
        };
        self.displayed = selected.clone();

        let mut located = Vec::new();
        for (pane_ix, pane) in self.center.panes().into_iter().enumerate() {
            for (ix, item) in pane.read(cx).items().enumerate() {
                if selected.contains(&item.item_id()) {
                    located.push((pane_ix, ix, pane.clone(), item.item_id()));
                }
            }
        }
        located.sort_by_key(|(pane_ix, ix, _, _)| (*pane_ix, *ix));
        let located: Vec<_> = located
            .into_iter()
            .map(|(_, ix, pane, item_id)| (pane, ix, item_id))
            .collect();
        if located.is_empty() {
            return;
        }

        let mut claimed: Vec<Entity<Pane>> = Vec::new();
        let mut anchor = located[0].0.clone();
        for (pane, ix, item_id) in located {
            if !claimed.contains(&pane) {
                pane.update(cx, |pane, cx| {
                    pane.activate_item(ix, false, false, window, cx);
                });
                anchor = pane.clone();
                claimed.push(pane);
                continue;
            }
            let item = pane.update(cx, |pane, cx| {
                let ix = pane.items().position(|item| item.item_id() == item_id)?;
                pane.activate_item(ix, false, false, window, cx);
                pane.take_active_item(window, cx)
            });
            let Some(item) = item else {
                continue;
            };
            let new_pane =
                new_terminal_pane(self.workspace.clone(), project.clone(), false, window, cx);
            self.apply_tab_bar_buttons(&new_pane, cx);
            new_pane.update(cx, |pane, cx| {
                pane.add_item(item, true, true, None, window, cx);
            });
            self.center
                .split(&anchor, &new_pane, SplitDirection::Right, cx);
            anchor = new_pane.clone();
            claimed.push(new_pane);
        }

        let target = claimed[0].clone();
        for source in self.center.panes().into_iter().cloned().collect::<Vec<_>>() {
            let item_ids: Vec<_> = source
                .read(cx)
                .items()
                .map(|item| item.item_id())
                .filter(|id| !selected.contains(id))
                .collect();
            for item_id in item_ids {
                let destination_index = target.read(cx).items_len();
                workspace::move_item(
                    &source,
                    &target,
                    item_id,
                    destination_index,
                    false,
                    window,
                    cx,
                );
            }
        }
        for pane in self.center.panes().into_iter().cloned().collect::<Vec<_>>() {
            let ix = pane.read(cx).items().position(|item| item.item_id() == active);
            if let Some(ix) = ix {
                pane.update(cx, |pane, cx| {
                    pane.activate_item(ix, true, true, window, cx);
                });
                break;
            }
        }
        self.serialize(cx);
        cx.notify();
    }

    fn present_terminal_ids(&self, cx: &App) -> Vec<EntityId> {
        self.center
            .panes()
            .into_iter()
            .flat_map(|pane| pane.read(cx).items().map(|item| item.item_id()))
            .collect()
    }

    fn pane_active_ids(&self, cx: &App) -> Vec<EntityId> {
        self.center
            .panes()
            .into_iter()
            .filter_map(|pane| pane.read(cx).active_item().map(|item| item.item_id()))
            .collect()
    }

    fn sync_groups(&mut self, cx: &App) {
        let present = self.present_terminal_ids(cx);
        if self.groups.is_empty() {
            if self.center.panes().len() > 1 {
                let actives = self.pane_active_ids(cx);
                self.groups.push(actives);
            }
        }
        for group in &mut self.groups {
            group.retain(|id| present.contains(id));
        }
        self.groups.retain(|group| !group.is_empty());
        self.displayed.retain(|id| present.contains(id));
        for id in present {
            if self.groups.iter().any(|group| group.contains(&id)) {
                continue;
            }
            let parent = self.pending_split_source.take().and_then(|source| {
                self.groups
                    .iter()
                    .position(|group| group.contains(&source))
                    .map(|group_ix| (group_ix, source))
            });
            match parent {
                Some((group_ix, source)) => {
                    let at = self.groups[group_ix]
                        .iter()
                        .position(|member| *member == source)
                        .map_or(self.groups[group_ix].len(), |ix| ix + 1);
                    self.groups[group_ix].insert(at, id);
                }
                None => self.groups.push(vec![id]),
            }
        }
    }

    fn group_index_of(&self, id: EntityId) -> Option<usize> {
        self.groups.iter().position(|group| group.contains(&id))
    }

    fn normalize_layout(
        &mut self,
        intent: Option<EntityId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring {
            return;
        }
        self.sync_groups(cx);
        let active_item = self
            .active_pane
            .read(cx)
            .active_item()
            .map(|item| item.item_id());
        let (group_ix, anchor) = if let Some(intent) = intent {
            let Some(group_ix) = self.group_index_of(intent) else {
                return;
            };
            (group_ix, intent)
        } else if let Some(group_ix) = self
            .displayed
            .first()
            .and_then(|id| self.group_index_of(*id))
        {
            let group = &self.groups[group_ix];
            let anchor = active_item
                .filter(|id| group.contains(id))
                .unwrap_or(group[0]);
            (group_ix, anchor)
        } else if let Some(active_item) = active_item {
            let Some(group_ix) = self.group_index_of(active_item) else {
                return;
            };
            (group_ix, active_item)
        } else {
            return;
        };
        let members = self.groups[group_ix].clone();
        let mut actives = self.pane_active_ids(cx);
        actives.sort();
        let mut expected = members.clone();
        expected.sort();
        if actives == expected {
            self.displayed = members;
            return;
        }
        self.apply_layout(members, anchor, window, cx);
    }

    fn schedule_normalize(
        &mut self,
        intent: Option<EntityId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.defer_in(window, move |panel, window, cx| {
            panel.normalize_layout(intent, window, cx);
        });
    }

    fn join_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_terminals.len() < 2 {
            return;
        }
        self.sync_groups(cx);
        let flat: Vec<EntityId> = self.groups.iter().flatten().copied().collect();
        let selected: Vec<EntityId> = flat
            .into_iter()
            .filter(|id| self.selected_terminals.contains(id))
            .collect();
        if selected.len() < 2 {
            return;
        }
        let first = selected[0];
        let first_was_single = self
            .group_index_of(first)
            .is_some_and(|ix| self.groups[ix].len() == 1);
        let first_position = self.group_index_of(first).unwrap_or(0);
        for group in &mut self.groups {
            group.retain(|id| !selected.contains(id));
        }
        let insert_at = if first_was_single {
            let kept_before = self.groups[..first_position.min(self.groups.len())]
                .iter()
                .filter(|group| !group.is_empty())
                .count();
            kept_before
        } else {
            usize::MAX
        };
        self.groups.retain(|group| !group.is_empty());
        let insert_at = insert_at.min(self.groups.len());
        self.groups.insert(insert_at, selected.clone());
        self.apply_layout(selected, first, window, cx);
    }

    fn show_terminal(&mut self, item_id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        self.normalize_layout(Some(item_id), window, cx);
        for pane in self.center.panes().into_iter().cloned().collect::<Vec<_>>() {
            let ix = pane
                .read(cx)
                .items()
                .position(|item| item.item_id() == item_id);
            if let Some(ix) = ix {
                pane.update(cx, |pane, cx| {
                    pane.activate_item(ix, true, true, window, cx);
                });
                self.active_pane = pane;
                break;
            }
        }
        cx.notify();
    }

    fn click_terminal_row(
        &mut self,
        item_id: EntityId,
        ordered_ids: &[EntityId],
        modifiers: &Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if modifiers.secondary() {
            if let Some(pos) = self.selected_terminals.iter().position(|id| *id == item_id) {
                self.selected_terminals.remove(pos);
            } else {
                self.selected_terminals.push(item_id);
            }
            self.selection_anchor = Some(item_id);
        } else if modifiers.shift {
            let anchor = self.selection_anchor.unwrap_or(item_id);
            let a = ordered_ids.iter().position(|id| *id == anchor);
            let b = ordered_ids.iter().position(|id| *id == item_id);
            if let (Some(a), Some(b)) = (a, b) {
                let (lo, hi) = (a.min(b), a.max(b));
                self.selected_terminals = ordered_ids[lo..=hi].to_vec();
            }
        } else {
            self.selected_terminals = vec![item_id];
            self.selection_anchor = Some(item_id);
            self.show_terminal(item_id, window, cx);
        }
        cx.notify();
    }

    fn toggle_list_content_width(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previous) = self.list_width_before_fit.take() {
            self.terminal_list_width = previous;
            cx.notify();
            return;
        }
        let settings = theme_settings::ThemeSettings::get_global(cx);
        let font = settings.ui_font.clone();
        let font_size = settings.ui_font_size(cx);
        let color = cx.theme().colors().text;
        let mut widest = Pixels::ZERO;
        for (position, id) in self.groups.iter().flat_map(|group| {
            let len = group.len();
            group.iter().enumerate().map(move |(position, id)| ((position, len), *id))
        }) {
            let (position, len) = position;
            let title = self.center.panes().into_iter().find_map(|pane| {
                pane.read(cx)
                    .items()
                    .find(|item| item.item_id() == id)
                    .map(|item| item.tab_content_text(1, cx))
            });
            let Some(title) = title else { continue };
            let prefix = if len < 2 {
                ""
            } else if position == 0 {
                "┌ "
            } else if position + 1 == len {
                "└ "
            } else {
                "├ "
            };
            let text: SharedString = format!(
                "{prefix}{}",
                title.split_whitespace().collect::<Vec<_>>().join(" ")
            )
            .into();
            let run = gpui::TextRun {
                len: text.len(),
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let width = window
                .text_system()
                .shape_line(text, font_size, &[run], None)
                .width;
            widest = widest.max(width);
        }
        let chrome = ui::scaled_px(10.).to_pixels(window.rem_size()) * 2.
            + ui::scaled_px(16.).to_pixels(window.rem_size())
            + ui::scaled_px(12.).to_pixels(window.rem_size());
        let min_width = ui::scaled_px(46.).to_pixels(window.rem_size());
        let max_width = window.viewport_size().width / 2.;
        let fitted = (widest + chrome).max(min_width).min(max_width);
        self.list_width_before_fit = Some(self.terminal_list_width);
        self.terminal_list_width = Some(fitted);
        cx.notify();
    }

    fn split_active_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        pane.update(cx, |pane, cx| {
            pane.split(SplitDirection::Right, SplitMode::default(), window, cx);
        });
    }

    fn kill_active_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        let Some(item_id) = pane.read(cx).active_item().map(|item| item.item_id()) else {
            return;
        };
        pane.update(cx, |pane, cx| {
            pane.close_item_by_id(item_id, workspace::SaveIntent::Close, window, cx)
                .detach_and_log_err(cx);
        });
    }

    fn unjoin_terminal(&mut self, item_id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_groups(cx);
        let Some(group_ix) = self.group_index_of(item_id) else {
            return;
        };
        if self.groups[group_ix].len() < 2 {
            return;
        }
        self.groups[group_ix].retain(|id| *id != item_id);
        self.groups.push(vec![item_id]);
        let members = self.groups[group_ix].clone();
        let active = self
            .active_pane
            .read(cx)
            .active_item()
            .map(|item| item.item_id())
            .filter(|id| members.contains(id))
            .unwrap_or(members[0]);
        self.apply_layout(members, active, window, cx);
    }

    /// VS Code-style terminal list shown on the right of the panel: one row per
    /// terminal (across all split panes) plus a small header with new/split/zoom.
    fn render_terminal_list(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colors = cx.theme().colors().clone();
        let panel = cx.entity().downgrade();
        let list_width = self
            .terminal_list_width
            .unwrap_or_else(|| ui::scaled_px(120.).to_pixels(window.rem_size()));

        // Collect every terminal first so split groups can get VS Code's ┌ ├ └ prefixes.
        let mut entries = Vec::new();
        for pane in self.center.panes() {
            let pane_is_active = *pane == self.active_pane;
            let pane_ref = pane.read(cx);
            let active_ix = pane_ref.active_item_index();
            for (ix, item) in pane_ref.items().enumerate() {
                entries.push((
                    pane.clone(),
                    ix,
                    item.item_id(),
                    item.tab_content_text(1, cx),
                    pane_is_active && ix == active_ix,
                    item.downcast::<TerminalView>(),
                    ix == active_ix,
                ));
            }
        }
        let mut ordered_entries = Vec::with_capacity(entries.len());
        let mut group_positions = Vec::with_capacity(entries.len());
        for group in &self.groups {
            for (position, id) in group.iter().enumerate() {
                if let Some(entry_ix) = entries.iter().position(|entry| entry.2 == *id) {
                    ordered_entries.push(entries.remove(entry_ix));
                    group_positions.push((position, group.len()));
                }
            }
        }
        for entry in entries {
            ordered_entries.push(entry);
            group_positions.push((0, 1));
        }
        let entries = ordered_entries;
        let is_split_group = group_positions.iter().any(|(_, len)| *len > 1);
        let ordered_ids: Vec<EntityId> = entries.iter().map(|entry| entry.2).collect();
        let selected_terminals = self.selected_terminals.clone();
        let has_multi_selection = selected_terminals.len() > 1;

        let mut rows = Vec::new();
        for (row_ix, (pane, ix, item_id, title, selected, terminal_view, _)) in
            entries.into_iter().enumerate()
        {
            let (row_icon, row_icon_color, rename_editor) = match &terminal_view {
                Some(terminal_view) => {
                    let terminal_view = terminal_view.read(cx);
                    (
                        terminal_view.custom_icon().unwrap_or(IconName::Terminal),
                        terminal_view
                            .custom_color()
                            .map(|index| crate::ansi_theme_color(index, cx)),
                        terminal_view.rename_editor(),
                    )
                }
                None => (IconName::Terminal, None, None),
            };
            let menu_view = terminal_view.clone();
            let rename_view = terminal_view.clone();
            let (position, group_len) = group_positions[row_ix];
            let prefix = if group_len < 2 {
                ""
            } else if position == 0 {
                "┌ "
            } else if position + 1 == group_len {
                "└ "
            } else {
                "├ "
            };
            let group_name = SharedString::from(format!("terminal-list-group-{row_ix}"));
            let tooltip_info = TerminalInfoTooltip {
                title: title.clone(),
                pid: None,
                command_line: None,
                working_directory: None,
                shell_integration: false,
                shell_type: None,
                seen_sequences: Vec::new(),
                show_details: false,
                on_show_environment: std::rc::Rc::new(|_, _| {}),
            };
            let tooltip_info = match terminal_view.as_ref() {
                Some(view) => {
                    let terminal = view.read(cx).terminal().read(cx);
                    let directory_name = terminal
                        .working_directory()
                        .and_then(|path| {
                            path.file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                        .unwrap_or_else(|| title.to_string());
                    let contributions = terminal.env_contributions().to_vec();
                    let workspace = self.workspace.clone();
                    TerminalInfoTooltip {
                        on_show_environment: std::rc::Rc::new(move |window, cx| {
                            open_environment_changes(
                                workspace.clone(),
                                directory_name.clone(),
                                contributions.clone(),
                                window,
                                cx,
                            );
                        }),
                        pid: terminal.pid().map(|pid| pid.as_u32()),
                        command_line: terminal
                            .shell_command_line()
                            .or_else(|| terminal.foreground_command_line()),
                        shell_type: terminal.shell_type(),
                        seen_sequences: terminal.seen_sequences().to_vec(),
                        working_directory: terminal
                            .working_directory()
                            .map(|path| path.to_string_lossy().into_owned()),
                        shell_integration: terminal.shell_integration_injected(),
                        ..tooltip_info
                    }
                }
                None => tooltip_info,
            };
            let title: SharedString = title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .into();
            let close_pane = pane.clone();
            let split_pane = pane.clone();
            let menu_panel = panel.clone();
            let menu_pane = pane.clone();
            let click_ids = ordered_ids.clone();
            let selected = if has_multi_selection {
                selected_terminals.contains(&item_id)
            } else {
                selected
            };
            let row = h_flex()
                .id(("terminal-list-row", row_ix))
                .group(group_name.clone())
                .relative()
                .w_full()
                .h(ui::scaled_px(22.))
                .flex_none()
                .px(px(10.))
                .gap_1p5()
                .items_center()
                .cursor_pointer()
                .when(selected, |row| {
                    row.bg(colors.element_selected).child(
                        div()
                            .absolute()
                            .left(px(3.))
                            .top_0()
                            .bottom_0()
                            .w(px(1.))
                            .bg(colors.tab_active_foreground),
                    )
                })
                .hover(|row| row.bg(colors.element_hover))
                .tooltip_left_of_element()
                .tooltip_show_delay(std::time::Duration::from_millis(1000))
                .hoverable_tooltip(move |_, cx| {
                    let info = tooltip_info.clone();
                    cx.new(|_| info).into()
                })
                .child(
                    Icon::new(row_icon)
                        .size(IconSize::Medium)
                        .color(if let Some(color) = row_icon_color {
                            Color::Custom(color)
                        } else if selected {
                            Color::Custom(colors.tab_active_foreground)
                        } else {
                            Color::Default
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .map(|container| match (rename_editor, rename_view) {
                            (Some(editor), Some(rename_view)) => {
                                let cancel_view = rename_view.clone();
                                container
                                    .child(
                                        div()
                                            .w_full()
                                            .px_1()
                                            .bg(colors.editor_background)
                                            .border_1()
                                            .border_color(colors.border_focused)
                                            .rounded_sm()
                                            .child(editor),
                                    )
                                    .on_action(move |_: &menu::Confirm, window, cx| {
                                        rename_view.update(cx, |view, cx| {
                                            view.finish_renaming(true, window, cx)
                                        });
                                    })
                                    .on_action(move |_: &menu::Cancel, window, cx| {
                                        cancel_view.update(cx, |view, cx| {
                                            view.finish_renaming(false, window, cx)
                                        });
                                    })
                            }
                            _ => container.child(
                                Label::new(format!("{prefix}{title}"))
                                    .color(if selected {
                                        Color::Custom(colors.tab_active_foreground)
                                    } else {
                                        Color::Default
                                    })
                                    .truncate(),
                            ),
                        }),
                )
                .child(
                    h_flex()
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .px_1()
                        .bg(if selected {
                            colors.element_selected
                        } else {
                            colors.element_hover
                        })
                        .visible_on_hover(group_name)
                        .child(
                            IconButton::new(("terminal-list-split", row_ix), IconName::Split)
                                .icon_size(IconSize::Medium)
                                .icon_color(if selected {
                                    Color::Custom(colors.tab_active_foreground)
                                } else {
                                    Color::Default
                                })
                                .tooltip(Tooltip::text(t!("Split Right")))
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    split_pane.update(cx, |pane, cx| {
                                        pane.activate_item(ix, true, true, window, cx);
                                        pane.split(
                                            SplitDirection::Right,
                                            SplitMode::default(),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                        .child(
                            IconButton::new(("terminal-list-close", row_ix), IconName::Trash)
                                .icon_size(IconSize::Medium)
                                .icon_color(if selected {
                                    Color::Custom(colors.tab_active_foreground)
                                } else {
                                    Color::Default
                                })
                                .tooltip(Tooltip::text(t!("Kill Terminal")))
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    close_pane.update(cx, |pane, cx| {
                                        pane.close_item_by_id(
                                            item_id,
                                            workspace::SaveIntent::Close,
                                            window,
                                            cx,
                                        )
                                        .detach_and_log_err(cx);
                                    });
                                }),
                        ),
                )
                .on_click(cx.listener(move |panel, event: &ClickEvent, window, cx| {
                    panel.click_terminal_row(
                        item_id,
                        &click_ids,
                        &event.modifiers(),
                        window,
                        cx,
                    );
                }));
            rows.push(
                right_click_menu(("terminal-list-menu", row_ix))
                    .trigger(|_, _, _| row)
                    .menu(move |window, cx| {
                        let panel = menu_panel.clone();
                        let pane = menu_pane.clone();
                        let menu_view = menu_view.clone();
                        ContextMenu::build(window, cx, move |menu, _, _| {
                            let join_panel = panel.clone();
                            let unjoin_panel = panel.clone();
                            menu.entry(t!("Split Terminal"), None, {
                                let pane = pane.clone();
                                move |window, cx| {
                                    pane.update(cx, |pane, cx| {
                                        pane.activate_item(ix, true, true, window, cx);
                                        pane.split(
                                            SplitDirection::Right,
                                            SplitMode::default(),
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            })
                            .when_some(menu_view.clone(), |menu, view| {
                                let rename_view = view.clone();
                                let icon_view = view.clone();
                                let color_view = view;
                                let fit_panel = panel.clone();
                                let icon_panel = panel.clone();
                                let color_panel = panel.clone();
                                menu.separator()
                                    .entry(t!("Rename..."), None, move |window, cx| {
                                        rename_view.update(cx, |view, cx| {
                                            view.rename_terminal(&Default::default(), window, cx)
                                        });
                                    })
                                    .entry(
                                        t!("Toggle Size to Content Width"),
                                        None,
                                        move |window, cx| {
                                            fit_panel
                                                .update(cx, |panel, cx| {
                                                    panel.toggle_list_content_width(window, cx)
                                                })
                                                .ok();
                                        },
                                    )
                                    .entry(t!("Change Icon..."), None, move |window, cx| {
                                        let view = icon_view.clone();
                                        icon_panel
                                            .update(cx, |panel, cx| {
                                                panel.show_terminal_choice(
                                                    view,
                                                    TerminalChoiceKind::Icon,
                                                    window,
                                                    cx,
                                                )
                                            })
                                            .ok();
                                    })
                                    .entry(t!("Change Color..."), None, move |window, cx| {
                                        let view = color_view.clone();
                                        color_panel
                                            .update(cx, |panel, cx| {
                                                panel.show_terminal_choice(
                                                    view,
                                                    TerminalChoiceKind::Color,
                                                    window,
                                                    cx,
                                                )
                                            })
                                            .ok();
                                    })
                            })
                            .when(has_multi_selection || is_split_group, |menu| {
                                menu.separator()
                            })
                            .when(has_multi_selection, |menu| {
                                menu.entry(t!("Join Terminals"), None, move |window, cx| {
                                    join_panel
                                        .update(cx, |panel, cx| panel.join_terminals(window, cx))
                                        .ok();
                                })
                            })
                            .when(!has_multi_selection && group_len > 1, |menu| {
                                menu.entry(t!("Unjoin Terminals"), None, move |window, cx| {
                                    unjoin_panel
                                        .update(cx, |panel, cx| {
                                            panel.unjoin_terminal(item_id, window, cx)
                                        })
                                        .ok();
                                })
                            })
                            .separator()
                            .entry(t!("Kill Terminal"), None, {
                                let pane = pane.clone();
                                move |window, cx| {
                                    pane.update(cx, |pane, cx| {
                                        pane.close_item_by_id(
                                            item_id,
                                            workspace::SaveIntent::Close,
                                            window,
                                            cx,
                                        )
                                        .detach_and_log_err(cx);
                                    });
                                }
                            })
                        })
                    }),
            );
        }

        let sash_color = colors.border_focused;
        let dragging = cx.active_drag_value::<DraggedTerminalList>().is_some();
        v_flex()
            .relative()
            .h_full()
            .w(list_width)
            .flex_none()
            .pl(px(1.))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(1.))
                    .bg(colors.border),
            )
            .child(
                div()
                    .id("terminal-list-resize-handle")
                    .group("terminal-list-sash")
                    .absolute()
                    .left(px(-2.))
                    .top_0()
                    .bottom_0()
                    .w(px(4.))
                    .flex()
                    .justify_center()
                    .cursor_col_resize()
                    .child(
                        div()
                            .id("terminal-list-sash")
                            .h_full()
                            .w(px(4.))
                            .rounded(px(2.))
                            .group_hover("terminal-list-sash", move |style| style.bg(sash_color))
                            .when(dragging, move |style| style.bg(sash_color)),
                    )
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.notify()),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.notify()),
                    )
                    .on_click(cx.listener(|panel, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            panel.toggle_list_content_width(window, cx);
                        }
                    }))
                    .on_drag(DraggedTerminalList, |dragged, _, _, cx| {
                        cx.new(|_| dragged.clone())
                    }),
            )
            .child(
                v_flex()
                    .id("terminal-list-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }

    fn show_terminal_choice(
        &mut self,
        view: Entity<TerminalView>,
        kind: TerminalChoiceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            TerminalChoiceModal::toggle(workspace, view, kind, window, cx)
        });
    }

    fn toggle_maximize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.position(window, cx) != DockPosition::Bottom {
            window.dispatch_action(ToggleZoom.boxed_clone(), cx);
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let dock = workspace.read(cx).bottom_dock().clone();
        let new_size = if let Some(previous) = self.maximized_from.take() {
            previous
        } else {
            let current = dock
                .read(cx)
                .active_panel_size()
                .and_then(|state| state.size)
                .unwrap_or(px(300.));
            let chrome = ui::utils::platform_title_bar_height(window) + px(22. + 8. + 2.);
            self.maximized_from = Some(current);
            window.viewport_size().height - chrome
        };
        window.defer(cx, move |window, cx| {
            dock.update(cx, |dock, cx| {
                dock.resize_panel_sizes(Some(new_size), None, window, cx);
            });
        });
        cx.notify();
    }

    fn render_panel_header(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let maximized = self.maximized_from.is_some();
        let showing_problems = self.show_problems;
        let showing_debug = self.show_debug_console && self.debug_console_view.is_some();
        let showing_logs = showing_debug && self.show_debug_logs;
        let show_tab_icons = self.show_tab_icons;
        let tab_content = move |tab: PanelTab| -> AnyElement {
            if show_tab_icons {
                Icon::new(tab.icon())
                    .size(IconSize::Small)
                    .into_any_element()
            } else {
                div().child(tab.label()).into_any_element()
            }
        };
        let problems_toolbar = self
            .problems_view
            .clone()
            .filter(|_| showing_problems)
            .map(|view| {
                view.update(cx, |view, cx| {
                    view.toolbar(window, cx).into_any_element()
                })
            });
        let problem_count = self
            .workspace
            .upgrade()
            .map(|workspace| {
                let summary = workspace
                    .read(cx)
                    .project()
                    .read(cx)
                    .diagnostic_summary(false, cx);
                summary.error_count + summary.warning_count
            })
            .unwrap_or(0);
        let hidden_tabs = self.hidden_tabs.clone();
        let colors = cx.theme().colors().clone();
        let item_count: usize = self
            .center
            .panes()
            .into_iter()
            .map(|pane| pane.read(cx).items_len())
            .sum();
        let single_title = (item_count == 1)
            .then(|| {
                self.center.panes().into_iter().find_map(|pane| {
                    pane.read(cx)
                        .active_item()
                        .map(|item| item.tab_content_text(0, cx))
                })
            })
            .flatten();
        h_flex()
            .w_full()
            .h(ui::scaled_px(32.))
            .flex_none()
            .pl(px(4.))
            .pr(px(4.))
            .justify_between()
            .bg(colors.panel_background)
            .border_b_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .h_full()
                    .child(
                        div()
                            .id("panel-tab-terminal")
                            .h_full()
                            .flex()
                            .items_center()
                            .px(ui::scaled_px(10.))
                            .cursor_pointer()
                            .text_size(ui::scaled_px(13.))
                            .when(hidden_tabs.contains(&PanelTab::Terminal), |tab| tab.hidden())
                            .map(|tab| {
                                if showing_problems || showing_debug {
                                    tab.text_color(colors.text_muted)
                                        .hover(|style| style.text_color(colors.text))
                                } else {
                                    tab.rounded_t(px(4.))
                                        .bg(colors.panel_background)
                                        .text_color(colors.tab_active_foreground)
                                }
                            })
                            .on_click(cx.listener(|panel, _, _, cx| {
                                panel.show_problems = false;
                                panel.show_debug_console = false;
                                cx.notify();
                            }))
                            .child(tab_content(PanelTab::Terminal)),
                    )
                    .child(
                        div()
                            .id("panel-tab-problems")
                            .h_full()
                            .flex()
                            .items_center()
                            .px(ui::scaled_px(10.))
                            .cursor_pointer()
                            .text_size(ui::scaled_px(13.))
                            .when(hidden_tabs.contains(&PanelTab::Problems), |tab| tab.hidden())
                            .map(|tab| {
                                if showing_problems {
                                    tab.rounded_t(px(4.))
                                        .bg(colors.panel_background)
                                        .text_color(colors.tab_active_foreground)
                                } else {
                                    tab.text_color(colors.text_muted)
                                        .hover(|style| style.text_color(colors.text))
                                }
                            })
                            .on_click(cx.listener(|panel, _, window, cx| {
                                panel.show_problems_view(window, cx);
                            }))
                            .child(tab_content(PanelTab::Problems))
                            .when(problem_count > 0, |tab| {
                                tab.child(
                                    div()
                                        .ml(px(6.))
                                        .px(px(5.))
                                        .h(ui::scaled_px(16.))
                                        .rounded(px(10.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(ui::scaled_px(10.))
                                        .bg(colors.tab_active_foreground)
                                        .text_color(colors.title_bar_background)
                                        .child(format_problem_count(problem_count)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("panel-tab-output")
                            .h_full()
                            .flex()
                            .when(hidden_tabs.contains(&PanelTab::Output), |tab| tab.hidden())
                            .items_center()
                            .px(ui::scaled_px(10.))
                            .cursor_pointer()
                            .text_size(ui::scaled_px(13.))
                            .text_color(colors.text_muted)
                            .hover(|style| style.text_color(colors.text))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(workspace::OpenLog.boxed_clone(), cx);
                            })
                            .child(tab_content(PanelTab::Output)),
                    )
                    .child(
                        div()
                            .id("panel-tab-ports")
                            .h_full()
                            .flex()
                            .when(hidden_tabs.contains(&PanelTab::Ports), |tab| tab.hidden())
                            .items_center()
                            .px(ui::scaled_px(10.))
                            .cursor_pointer()
                            .text_size(ui::scaled_px(13.))
                            .text_color(colors.text_muted)
                            .hover(|style| style.text_color(colors.text))
                            .tooltip(Tooltip::text(t!("Incoming")))
                            .on_click({
                                let workspace = self.workspace.clone();
                                move |_, _, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            workspace.show_toast(
                                                workspace::Toast::new(
                                                    workspace::notifications::NotificationId::unique::<PortsTabToast>(),
                                                    t!("Ports is not available yet").to_string(),
                                                ),
                                                cx,
                                            );
                                        })
                                        .ok();
                                }
                            })
                            .child(tab_content(PanelTab::Ports)),
                    )
                    .child(
                        div()
                            .id("panel-tab-debug-console")
                            .h_full()
                            .flex()
                            .when(hidden_tabs.contains(&PanelTab::DebugConsole), |tab| tab.hidden())
                            .items_center()
                            .px(ui::scaled_px(10.))
                            .cursor_pointer()
                            .text_size(ui::scaled_px(13.))
                            .map(|tab| {
                                if showing_debug {
                                    tab.rounded_t(px(4.))
                                        .bg(colors.panel_background)
                                        .text_color(colors.tab_active_foreground)
                                } else {
                                    tab.text_color(colors.text_muted)
                                        .hover(|style| style.text_color(colors.text))
                                }
                            })
                            .on_click(cx.listener(|panel, _, window, cx| {
                                if panel.debug_console_view.is_some() {
                                    panel.show_debug_console(false, cx);
                                } else if let Ok(action) =
                                    cx.build_action("debug_panel::ToggleFocus", None)
                                {
                                    window.dispatch_action(action, cx);
                                }
                            }))
                            .child(tab_content(PanelTab::DebugConsole)),
                    ),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .items_center()
                    .children(problems_toolbar)
                    .when(showing_debug, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    Button::new("debug-view-console", t!("Console"))
                                        .label_size(LabelSize::Small)
                                        .toggle_state(!showing_logs)
                                        .on_click(cx.listener(|panel, _, _, cx| {
                                            panel.show_debug_logs = false;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("debug-view-logs", t!("Adapter Logs"))
                                        .label_size(LabelSize::Small)
                                        .toggle_state(showing_logs)
                                        .disabled(self.debug_log_view.is_none())
                                        .on_click(cx.listener(|panel, _, _, cx| {
                                            panel.show_debug_logs = true;
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .child(
                        h_flex()
                    .gap_0p5()
                    .items_center()
                    .when(showing_problems || showing_debug, |this| this.hidden())
                    .when_some(single_title, |this, title| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .px_1()
                                .items_center()
                                .child(Icon::new(IconName::Terminal).size(IconSize::Medium))
                                .child(Label::new(title)),
                        )
                    })
                    .child(
                        IconButton::new("terminal-header-new", IconName::Plus)
                            .icon_size(IconSize::Medium)
                            .tooltip(Tooltip::text(t!("New Terminal")))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    workspace::NewTerminal::default().boxed_clone(),
                                    cx,
                                );
                            }),
                    )
                    .child(
                        PopoverMenu::new("terminal-header-new-menu")
                            .trigger(
                                IconButton::new(
                                    "terminal-header-new-menu-button",
                                    IconName::ChevronDown,
                                )
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text(t!("Launch Profile…"))),
                            )
                            .anchor(Anchor::TopRight)
                            .menu(|window, cx| {
                                Some(ContextMenu::build(window, cx, |menu, _, _| {
                                    menu.action(
                                        t!("New Terminal"),
                                        workspace::NewTerminal::default().boxed_clone(),
                                    )
                                    .action(
                                        t!("New Terminal in Editor Area"),
                                        workspace::NewCenterTerminal::default().boxed_clone(),
                                    )
                                    .action(
                                        t!("Split Terminal"),
                                        SplitRight::default().boxed_clone(),
                                    )
                                    .separator()
                                    .action(
                                        t!("Configure Terminal Settings"),
                                        crab_actions::OpenSettings.boxed_clone(),
                                    )
                                }))
                            }),
                    )
                    .child(
                        PopoverMenu::new("terminal-header-more-actions")
                            .trigger(
                                IconButton::new("terminal-header-more", IconName::Ellipsis)
                                    .icon_size(IconSize::Medium)
                                    .tooltip(Tooltip::text(t!("Views and More Actions…"))),
                            )
                            .anchor(Anchor::TopRight)
                            .menu({
                                let panel = cx.entity().downgrade();
                                move |window, cx| {
                                    let split_panel = panel.clone();
                                    let kill_panel = panel.clone();
                                    Some(ContextMenu::build(window, cx, move |menu, _, _| {
                                        menu.entry(t!("Split Terminal"), None, move |window, cx| {
                                            split_panel
                                                .update(cx, |panel, cx| {
                                                    panel.split_active_terminal(window, cx)
                                                })
                                                .ok();
                                        })
                                        .entry(t!("Kill Terminal"), None, move |window, cx| {
                                            kill_panel
                                                .update(cx, |panel, cx| {
                                                    panel.kill_active_terminal(window, cx)
                                                })
                                                .ok();
                                        })
                                    }))
                                }
                            }),
                    )
                    )
                    .child(div().w(px(1.)).h(px(16.)).mx_1().bg(colors.border))
                    .child(
                        IconButton::new(
                            "terminal-header-zoom",
                            if maximized {
                                IconName::Minimize
                            } else {
                                IconName::Maximize
                            },
                        )
                        .icon_size(IconSize::Medium)
                        .tooltip(Tooltip::text(if maximized {
                            t!("Restore Panel Size")
                        } else {
                            t!("Maximize Panel Size")
                        }))
                        .on_click(cx.listener(|panel, _, window, cx| {
                            panel.toggle_maximize(window, cx);
                        })),
                    )
                    .child(
                        IconButton::new("terminal-header-close", IconName::Close)
                            .icon_size(IconSize::Medium)
                            .tooltip(Tooltip::text(t!("Close Panel")))
                            .on_click(|_, window, cx| {
                                window
                                    .dispatch_action(workspace::ToggleBottomDock.boxed_clone(), cx);
                            }),
                    ),
            )
    }
}

#[derive(Clone)]
struct DraggedTerminalList;

fn open_environment_changes(
    workspace: WeakEntity<Workspace>,
    directory_name: String,
    contributions: Vec<(String, String)>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(workspace_entity) = workspace.upgrade() else {
        return;
    };
    let title = format!("Environment changes - {directory_name}");
    let mut text = String::from("# Terminal Environment Changes\n\n");
    if contributions.is_empty() {
        text.push_str("Nothing has contributed to this terminal's environment.\n");
    } else {
        text.push_str("## Crab shell integration\n\n");
        text.push_str("Enables shell integration by modifying `ZDOTDIR`\n\n");
        for (name, value) in &contributions {
            text.push_str(&format!("- `{name}={value}`\n"));
        }
    }
    let language = workspace_entity
        .read(cx)
        .app_state()
        .languages
        .language_for_name("Markdown");
    window
        .spawn(cx, async move |cx| {
            let language = language.await.log_err();
            let (buffer, project) = workspace.update(cx, |workspace, cx| {
                let project = workspace.project().clone();
                let buffer =
                    project.update(cx, |project, cx| project.create_buffer(language, false, cx));
                (buffer, project)
            })?;
            let buffer = buffer.await?;
            buffer.update(cx, |buffer, cx| {
                buffer.set_text(text, cx);
                buffer.set_capability(language::Capability::ReadOnly, cx);
            });
            let multi_buffer = cx.new(|cx| {
                editor::MultiBuffer::singleton(buffer, cx).with_title(title.clone())
            });
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.add_item_to_active_pane(
                    Box::new(cx.new(|cx| {
                        let mut editor =
                            editor::Editor::for_multibuffer(multi_buffer, Some(project), window, cx);
                        editor.set_read_only(true);
                        editor.set_should_serialize(false, cx);
                        editor
                    })),
                    None,
                    true,
                    window,
                    cx,
                )
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
}

#[derive(Clone)]
struct TerminalInfoTooltip {
    title: SharedString,
    pid: Option<u32>,
    command_line: Option<String>,
    working_directory: Option<String>,
    shell_integration: bool,
    shell_type: Option<String>,
    seen_sequences: Vec<&'static str>,
    show_details: bool,
    on_show_environment: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
}

impl Render for TerminalInfoTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let ui_font = theme_settings::ThemeSettings::get_global(cx).ui_font.clone();
        let mode = if self.shell_integration { "Rich" } else { "None" };
        let link = |id: &'static str, label: SharedString, color: gpui::Hsla| {
            div()
                .id(id)
                .cursor_pointer()
                .text_color(color)
                .hover(|style| style.underline())
                .child(label)
        };
        let link_color = colors.text_accent;

        let popup_bg: gpui::Hsla = gpui::rgb(0x31363A).into();
        let popup_border: gpui::Hsla = gpui::rgb(0x40454C).into();
        let popup_footer_bg: gpui::Hsla = gpui::rgb(0x383D41).into();
        let arrow_fill = popup_bg;
        let arrow_border = popup_border;
        let arrow = gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let mut triangle = |inset: Pixels, base: Pixels, color: gpui::Hsla| {
                    let mut builder = gpui::PathBuilder::fill();
                    builder.move_to(gpui::point(bounds.left() - base, bounds.top() + inset));
                    builder.line_to(gpui::point(
                        bounds.left() + px(4.) - inset,
                        bounds.center().y,
                    ));
                    builder.line_to(gpui::point(bounds.left() - base, bounds.bottom() - inset));
                    builder.close();
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }
                };
                triangle(px(0.), px(1.), arrow_border);
                triangle(px(1.), px(1.5), arrow_fill);
            },
        )
        .w(px(0.))
        .h(px(8.));

        h_flex().items_center().child(
            v_flex()
                .elevation_2(cx)
                .bg(popup_bg)
                .border_color(popup_border)
                .font(ui_font)
                .text_ui(cx)
                .text_color(colors.text)
                .max_w(px(520.))
                .overflow_hidden()
                .child(
                    v_flex()
                        .gap_3()
                        .px_3()
                        .py_2p5()
                        .child(div().child(self.title.clone()))
                        .when_some(self.pid, |this, pid| {
                            this.child(div().child(t!("Process ID (PID): {pid}", pid = pid)))
                        })
                        .when_some(self.command_line.clone(), |this, command| {
                            this.child(div().child(t!("Command line: {command}", command = command)))
                        })
                        .child(div().child(t!("Shell integration: {mode}", mode = mode)))
                        .when(self.show_details, |this| {
                            this.child(
                                v_flex()
                                    .when_some(self.shell_type.clone(), |this, shell| {
                                        this.child(div().child(t!(
                                            "Shell type: {shell}",
                                            shell = shell
                                        )))
                                    })
                                    .when_some(self.working_directory.clone(), |this, cwd| {
                                        this.child(div().child(t!(
                                            "Current working directory: {path}",
                                            path = cwd
                                        )))
                                    })
                                    .when(!self.seen_sequences.is_empty(), |this| {
                                        this.child(div().child(t!(
                                            "Seen sequences: {sequences}",
                                            sequences = self.seen_sequences.join(", ")
                                        )))
                                    }),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap_4()
                        .px_3()
                        .py_1()
                        .bg(popup_footer_bg)
                        .child(
                            link(
                                "terminal-tooltip-environment",
                                t!("Show Environment Contributions").into(),
                                link_color,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                (this.on_show_environment)(window, cx);
                            })),
                        )
                        .child(
                            link(
                                "terminal-tooltip-details",
                                if self.show_details {
                                    t!("Hide Details").into()
                                } else {
                                    t!("Show Details").into()
                                },
                                link_color,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_details = !this.show_details;
                                cx.notify();
                            })),
                        ),
                ),
        )
        .child(arrow)
    }
}

impl Render for DraggedTerminalList {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_resize = cx.listener(
            |panel, event: &gpui::DragMoveEvent<DraggedTerminalList>, window, cx| {
                let max_width = event.bounds.size.width / 2.;
                let min_width = ui::scaled_px(46.).to_pixels(window.rem_size());
                let width = (event.bounds.right() - event.event.position.x)
                    .max(min_width)
                    .min(max_width);
                panel.terminal_list_width = Some(width);
                cx.notify();
            },
        );
        self.sync_groups(cx);
        let terminal_list = self.render_terminal_list(window, cx);
        let panel_header = self.render_panel_header(window, cx);
        let panel_header = {
            let panel = cx.entity().downgrade();
            let hidden_tabs = self.hidden_tabs.clone();
            let show_tab_icons = self.show_tab_icons;
            let current_position = self.position(window, cx);
            right_click_menu("terminal-panel-header-menu")
                .trigger(move |_, _, _| panel_header)
                .menu(move |window, cx| {
                    let panel = panel.clone();
                    let hidden_tabs = hidden_tabs.clone();
                    ContextMenu::build(window, cx, move |mut menu, _, _| {
                        for tab in PanelTab::all() {
                            let panel = panel.clone();
                            menu = menu.toggleable_entry(
                                tab.label(),
                                !hidden_tabs.contains(&tab),
                                IconPosition::Start,
                                None,
                                move |_, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            panel.toggle_tab_visibility(tab, cx)
                                        })
                                        .ok();
                                },
                            );
                        }
                        let position_panel = panel.clone();
                        menu.separator()
                            .submenu(t!("Panel Position"), move |menu, _, _| {
                                let mut menu = menu;
                                for (position, label) in [
                                    (DockPosition::Left, t!("Left")),
                                    (DockPosition::Right, t!("Right")),
                                    (DockPosition::Bottom, t!("Bottom")),
                                ] {
                                    let panel = position_panel.clone();
                                    menu = menu.toggleable_entry(
                                        label,
                                        current_position == position,
                                        IconPosition::Start,
                                        None,
                                        move |window, cx| {
                                            panel
                                                .update(cx, |panel, cx| {
                                                    panel.set_position(position, window, cx)
                                                })
                                                .ok();
                                        },
                                    );
                                }
                                menu
                            })
                            .submenu(t!("Align Panel"), |menu, _, _| {
                                let mut menu = menu;
                                for (label, checked) in [
                                    (t!("Left"), false),
                                    (t!("Center"), true),
                                    (t!("Right"), false),
                                    (t!("Justify"), false),
                                ] {
                                    menu = menu.toggleable_entry_disabled_when(
                                        label,
                                        checked,
                                        true,
                                        IconPosition::Start,
                                        None,
                                        |_, _| {},
                                    );
                                }
                                menu
                            })
                            .item(
                                ui::ContextMenuEntry::new(if show_tab_icons {
                                    t!("Show Labels")
                                } else {
                                    t!("Show Icons")
                                })
                                .handler({
                                    let panel = panel.clone();
                                    move |_, cx| {
                                        panel
                                            .update(cx, |panel, cx| panel.toggle_tab_icons(cx))
                                            .ok();
                                    }
                                }),
                            )
                            .action(t!("Hide Panel"), workspace::ToggleBottomDock.boxed_clone())
                    })
                    .into()
                })
        };
        let problems_overlay = self
            .show_problems
            .then(|| self.problems_view.clone())
            .flatten()
            .map(|view| {
                div()
                    .absolute()
                    .top(ui::scaled_px(32.))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
                    .bg(cx.theme().colors().panel_background)
                    .child(view)
            });
        let debug_overlay = (self.show_debug_console && !self.show_problems)
            .then(|| {
                if self.show_debug_logs {
                    self.debug_log_view.clone()
                } else {
                    self.debug_console_view.clone()
                }
            })
            .flatten()
            .map(|view| {
                div()
                    .absolute()
                    .top(ui::scaled_px(32.))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
                    .bg(cx.theme().colors().panel_background)
                    .child(view)
            });
        let show_terminal_list = self
            .center
            .panes()
            .into_iter()
            .map(|pane| pane.read(cx).items_len())
            .sum::<usize>()
            > 1;
        let registrar = cx
            .try_global::<workspace::PaneSearchBarCallbacks>()
            .map(|callbacks| {
                (callbacks.wrap_div_with_search_actions)(div(), self.active_pane.clone())
            })
            .unwrap_or_else(div);
        let no_items_in_panes = self
            .center
            .panes()
            .into_iter()
            .all(|pane| pane.read(cx).items_len() == 0);
        let waiting_for_terminals = self.restoring || self.pending_terminals_to_add > 0;
        let restoring_placeholder = (waiting_for_terminals && no_items_in_panes).then(|| {
            let label = if self.restoring {
                t!("Restoring terminals…")
            } else {
                t!("Starting terminal…")
            };
            h_flex()
                .absolute()
                .inset_0()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::ArrowCircle)
                        .color(Color::Muted)
                        .size(IconSize::Small)
                        .with_rotate_animation(2),
                )
                .child(Label::new(label).color(Color::Muted))
        });
        self.workspace
            .update(cx, |workspace, cx| {
                registrar
                    .track_focus(&self.focus_handle)
                    .size_full()
                    .relative()
                    .on_drag_move(list_resize)
                    .child(
                        v_flex().size_full().child(panel_header).child(
                            h_flex()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .child(div().flex_1().min_w_0().h_full().child(self.center.render(
                                    workspace.zoomed_item(),
                                    None,
                                    &workspace::PaneRenderContext {
                                        active_pane: &self.active_pane,
                                        app_state: workspace.app_state(),
                                        project: workspace.project(),
                                        workspace: &workspace.weak_handle(),
                                    },
                                    window,
                                    cx,
                                )))
                                .children(show_terminal_list.then_some(terminal_list)),
                        ),
                    )
                    .children(restoring_placeholder)
                    .children(problems_overlay)
                    .children(debug_overlay)
            })
            .ok()
            .map(|div| {
                div.on_action({
                    cx.listener(|terminal_panel, _: &ActivatePaneLeft, window, cx| {
                        terminal_panel.activate_pane_in_direction(SplitDirection::Left, window, cx);
                    })
                })
                .on_action({
                    cx.listener(|terminal_panel, _: &ActivatePaneRight, window, cx| {
                        terminal_panel.activate_pane_in_direction(
                            SplitDirection::Right,
                            window,
                            cx,
                        );
                    })
                })
                .on_action({
                    cx.listener(|terminal_panel, _: &ActivatePaneUp, window, cx| {
                        terminal_panel.activate_pane_in_direction(SplitDirection::Up, window, cx);
                    })
                })
                .on_action({
                    cx.listener(|terminal_panel, _: &ActivatePaneDown, window, cx| {
                        terminal_panel.activate_pane_in_direction(SplitDirection::Down, window, cx);
                    })
                })
                .on_action(
                    cx.listener(|terminal_panel, _action: &ActivateNextPane, window, cx| {
                        let panes = terminal_panel.center.panes();
                        if let Some(ix) = panes
                            .iter()
                            .position(|pane| **pane == terminal_panel.active_pane)
                        {
                            let next_ix = (ix + 1) % panes.len();
                            window.focus(&panes[next_ix].focus_handle(cx), cx);
                        }
                    }),
                )
                .on_action(cx.listener(
                    |terminal_panel, _action: &ActivatePreviousPane, window, cx| {
                        let panes = terminal_panel.center.panes();
                        if let Some(ix) = panes
                            .iter()
                            .position(|pane| **pane == terminal_panel.active_pane)
                        {
                            let prev_ix = cmp::min(ix.wrapping_sub(1), panes.len() - 1);
                            window.focus(&panes[prev_ix].focus_handle(cx), cx);
                        }
                    },
                ))
                .on_action(
                    cx.listener(|terminal_panel, action: &ActivatePane, window, cx| {
                        let panes = terminal_panel.center.panes();
                        if let Some(&pane) = panes.get(action.0) {
                            window.focus(&pane.read(cx).focus_handle(cx), cx);
                        } else {
                            let future =
                                terminal_panel.new_pane_with_active_terminal(true, window, cx);
                            cx.spawn_in(window, async move |terminal_panel, cx| {
                                if let Some(new_pane) = future.await {
                                    _ = terminal_panel.update_in(
                                        cx,
                                        |terminal_panel, window, cx| {
                                            terminal_panel.center.split(
                                                &terminal_panel.active_pane,
                                                &new_pane,
                                                SplitDirection::Right,
                                                cx,
                                            );
                                            let new_pane = new_pane.read(cx);
                                            window.focus(&new_pane.focus_handle(cx), cx);
                                        },
                                    );
                                }
                            })
                            .detach();
                        }
                    }),
                )
                .on_action(cx.listener(|terminal_panel, _: &SwapPaneLeft, _, cx| {
                    terminal_panel.swap_pane_in_direction(SplitDirection::Left, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &SwapPaneRight, _, cx| {
                    terminal_panel.swap_pane_in_direction(SplitDirection::Right, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &SwapPaneUp, _, cx| {
                    terminal_panel.swap_pane_in_direction(SplitDirection::Up, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &SwapPaneDown, _, cx| {
                    terminal_panel.swap_pane_in_direction(SplitDirection::Down, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &MovePaneLeft, _, cx| {
                    terminal_panel.move_pane_to_border(SplitDirection::Left, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &MovePaneRight, _, cx| {
                    terminal_panel.move_pane_to_border(SplitDirection::Right, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &MovePaneUp, _, cx| {
                    terminal_panel.move_pane_to_border(SplitDirection::Up, cx);
                }))
                .on_action(cx.listener(|terminal_panel, _: &MovePaneDown, _, cx| {
                    terminal_panel.move_pane_to_border(SplitDirection::Down, cx);
                }))
                .on_action(
                    cx.listener(|terminal_panel, action: &MoveItemToPane, window, cx| {
                        let Some(&target_pane) =
                            terminal_panel.center.panes().get(action.destination)
                        else {
                            return;
                        };
                        move_active_item(
                            &terminal_panel.active_pane,
                            target_pane,
                            action.focus,
                            true,
                            window,
                            cx,
                        );
                    }),
                )
                .on_action(cx.listener(
                    |terminal_panel, action: &MoveItemToPaneInDirection, window, cx| {
                        let source_pane = &terminal_panel.active_pane;
                        if let Some(destination_pane) = terminal_panel
                            .center
                            .find_pane_in_direction(source_pane, action.direction, cx)
                        {
                            move_active_item(
                                source_pane,
                                destination_pane,
                                action.focus,
                                true,
                                window,
                                cx,
                            );
                        };
                    },
                ))
            })
            .unwrap_or_else(|| div())
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for TerminalPanel {
    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_pane.focus_handle(cx)
    }

    fn position(&self, _window: &Window, cx: &App) -> DockPosition {
        TerminalSettings::get_global(cx).dock.into()
    }

    fn position_is_valid(&self, _: DockPosition) -> bool {
        true
    }

    fn starts_open(&self, _: &Window, cx: &App) -> bool {
        TerminalSettings::get_global(cx).starts_open
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left => TerminalDockPosition::Left,
                DockPosition::Bottom => TerminalDockPosition::Bottom,
                DockPosition::Right => TerminalDockPosition::Right,
            };
            settings.terminal.get_or_insert_default().dock = Some(dock);
        });
    }

    fn default_size(&self, window: &Window, cx: &App) -> Pixels {
        let settings = TerminalSettings::get_global(cx);
        match self.position(window, cx) {
            DockPosition::Left | DockPosition::Right => settings.default_width,
            DockPosition::Bottom => settings.default_height,
        }
    }

    fn supports_flexible_size(&self) -> bool {
        true
    }

    fn has_flexible_size(&self, _window: &Window, cx: &App) -> bool {
        TerminalSettings::get_global(cx).flexible
    }

    fn set_flexible_size(&mut self, flexible: bool, _window: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            settings.terminal.get_or_insert_default().flexible = Some(flexible);
        });
    }

    fn is_zoomed(&self, _window: &Window, cx: &App) -> bool {
        self.active_pane.read(cx).is_zoomed()
    }

    fn set_zoomed(&mut self, zoomed: bool, _: &mut Window, cx: &mut Context<Self>) {
        for pane in self.center.panes() {
            pane.update(cx, |pane, cx| {
                pane.set_zoomed(zoomed, cx);
            })
        }
        cx.notify();
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        let old_active = self.active;
        self.active = active;
        if !active || old_active == active || self.restoring || !self.has_no_terminals(cx) {
            return;
        }
        cx.defer_in(window, |this, window, cx| {
            let Ok(kind) = this
                .workspace
                .update(cx, |workspace, cx| default_working_directory(workspace, cx))
            else {
                return;
            };

            this.add_terminal_shell(false, kind, RevealStrategy::Always, window, cx)
                .detach_and_log_err(cx)
        })
    }

    fn icon_label(&self, _window: &Window, cx: &App) -> Option<String> {
        if !TerminalSettings::get_global(cx).show_count_badge {
            return None;
        }
        let count = self
            .center
            .panes()
            .into_iter()
            .map(|pane| pane.read(cx).items_len())
            .sum::<usize>();
        if count == 0 {
            None
        } else {
            Some(count.to_string())
        }
    }

    fn persistent_name() -> &'static str {
        "TerminalPanel"
    }

    fn panel_key() -> &'static str {
        TERMINAL_PANEL_KEY
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<IconName> {
        if (self.is_enabled(cx) || !self.has_no_terminals(cx))
            && TerminalSettings::get_global(cx).button
        {
            Some(IconName::TerminalAlt)
        } else {
            None
        }
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some(n!("Terminal Panel"))
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(Toggle)
    }

    fn pane(&self) -> Option<Entity<Pane>> {
        Some(self.active_pane.clone())
    }

    fn activation_priority(&self) -> u32 {
        2
    }

    fn hide_button_setting(&self, _: &App) -> Option<workspace::HideStatusItem> {
        Some(workspace::HideStatusItem::new(|settings| {
            settings.terminal.get_or_insert_default().button = Some(false);
        }))
    }
}

struct TerminalProvider(Entity<TerminalPanel>);

impl workspace::TerminalProvider for TerminalProvider {
    fn spawn(
        &self,
        task: SpawnInTerminal,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Option<Result<ExitStatus>>> {
        let terminal_panel = self.0.clone();
        window.spawn(cx, async move |cx| {
            let terminal = terminal_panel
                .update_in(cx, |terminal_panel, window, cx| {
                    terminal_panel.spawn_task(&task, window, cx)
                })
                .ok()?
                .await;
            match terminal {
                Ok(terminal) => {
                    let exit_status = terminal
                        .read_with(cx, |terminal, cx| terminal.wait_for_completed_task(cx))
                        .ok()?
                        .await?;
                    Some(Ok(exit_status))
                }
                Err(e) => Some(Err(e)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use super::*;
    use crate::persistence::{SerializedPane, SerializedPaneGroup};
    use gpui::{TestAppContext, UpdateGlobal as _, VisualTestContext};
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use settings::SettingsStore;
    use workspace::{MultiWorkspace, WorkspaceId};

    #[test]
    fn test_prepare_empty_task() {
        let input = SpawnInTerminal::default();
        let shell = Shell::System;

        let result = prepare_task_for_spawn(&input, &shell, false);

        let expected_shell = util::get_system_shell();
        assert_eq!(result.env, HashMap::default());
        assert_eq!(result.cwd, None);
        assert_eq!(result.shell, Shell::System);
        assert_eq!(
            result.command,
            Some(expected_shell.clone()),
            "Empty tasks should spawn a -i shell"
        );
        assert_eq!(result.args, Vec::<String>::new());
        assert_eq!(
            result.command_label, expected_shell,
            "We show the shell launch for empty commands"
        );
    }

    #[gpui::test]
    async fn test_bypass_max_tabs_limit(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        set_max_tabs(cx, Some(3));

        for _ in 0..5 {
            let task = window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| {
                        panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                    })
                })
                .unwrap();
            task.await.unwrap();
        }

        cx.run_until_parked();

        let item_count =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());

        assert_eq!(
            item_count, 5,
            "Terminal panel should bypass max_tabs limit and have all 5 terminals"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_prepare_script_like_task() {
        let user_command = r#"REPO_URL=$(git remote get-url origin | sed -e \"s/^git@\\(.*\\):\\(.*\\)\\.git$/https:\\/\\/\\1\\/\\2/\"); COMMIT_SHA=$(git log -1 --format=\"%H\" -- \"${ZED_RELATIVE_FILE}\"); echo \"${REPO_URL}/blob/${COMMIT_SHA}/${ZED_RELATIVE_FILE}#L${ZED_ROW}-$(echo $(($(wc -l <<< \"$ZED_SELECTED_TEXT\") + $ZED_ROW - 1)))\" | xclip -selection clipboard"#.to_string();
        let expected_cwd = PathBuf::from("/some/work");

        let input = SpawnInTerminal {
            command: Some(user_command.clone()),
            cwd: Some(expected_cwd.clone()),
            ..SpawnInTerminal::default()
        };
        let shell = Shell::System;

        let result = prepare_task_for_spawn(&input, &shell, false);

        let system_shell = util::get_system_shell();
        assert_eq!(result.env, HashMap::default());
        assert_eq!(result.cwd, Some(expected_cwd));
        assert_eq!(result.shell, Shell::System);
        assert_eq!(result.command, Some(system_shell.clone()));
        assert_eq!(
            result.args,
            vec!["-i".to_string(), "-c".to_string(), user_command.clone()],
            "User command should have been moved into the arguments, as we're spawning a new -i shell",
        );
        assert_eq!(
            result.command_label,
            format!(
                "{system_shell} {interactive}-c '{user_command}'",
                interactive = if cfg!(windows) { "" } else { "-i " }
            ),
            "We want to show to the user the entire command spawned"
        );
    }

    #[gpui::test]
    async fn renders_error_if_default_shell_fails(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.terminal.get_or_insert_default().project.shell =
                        Some(settings::Shell::Program("__nonexistent_shell__".to_owned()));
                });
            });
        });

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.add_terminal_shell(
                        false,
                        None,
                        RevealStrategy::Always,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap_err();

        window_handle
            .update(cx, |_, _, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    assert!(
                        terminal_panel
                            .active_pane
                            .read(cx)
                            .items()
                            .any(|item| item.downcast::<FailedToSpawnTerminal>().is_some()),
                        "should spawn `FailedToSpawnTerminal` pane"
                    );
                    assert_eq!(terminal_panel.pending_terminals_to_add, 0);
                })
            })
            .unwrap();
    }

    #[gpui::test]
    async fn test_failed_task_spawn_does_not_leak_pending_terminal_count(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.add_terminal_task(
                        SpawnInTerminal {
                            command: Some("__nonexistent_program__".to_owned()),
                            ..SpawnInTerminal::default()
                        },
                        RevealStrategy::Never,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap_err();

        cx.run_until_parked();
        terminal_panel.read_with(cx, |terminal_panel, cx| {
            assert_eq!(terminal_panel.pending_terminals_to_add, 0);
            assert_eq!(terminal_panel.active_pane.read(cx).items_len(), 0);
        });
    }

    #[gpui::test]
    async fn test_pending_terminal_count_tracks_spawn_lifecycle(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        let task = window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    let task =
                        terminal_panel.add_terminal_shell(false, None, RevealStrategy::Never, window, cx);
                    assert_eq!(
                        terminal_panel.pending_terminals_to_add, 1,
                        "pending count should be incremented synchronously to avoid double default terminal spawns"
                    );
                    assert!(!terminal_panel.has_no_terminals(cx));
                    task
                })
            })
            .unwrap();
        task.await.unwrap();

        cx.run_until_parked();
        terminal_panel.read_with(cx, |terminal_panel, cx| {
            assert_eq!(terminal_panel.pending_terminals_to_add, 0);
            assert_eq!(terminal_panel.active_pane.read(cx).items_len(), 1);
        });
    }

    #[gpui::test]
    async fn test_pending_terminal_count_resets_when_spawn_cancelled(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    let task = terminal_panel.add_terminal_shell(
                        false,
                        None,
                        RevealStrategy::Never,
                        window,
                        cx,
                    );
                    assert_eq!(terminal_panel.pending_terminals_to_add, 1);
                    drop(task);
                })
            })
            .unwrap();

        cx.run_until_parked();
        terminal_panel.read_with(cx, |terminal_panel, _| {
            assert_eq!(terminal_panel.pending_terminals_to_add, 0);
        });
    }

    #[gpui::test]
    async fn test_load_without_serialized_state_does_not_persist_empty_state(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let serialization_key = window_handle
            .update(cx, |multi_workspace, _, cx| {
                TerminalPanel::serialization_key(multi_workspace.workspace().read(cx)).unwrap()
            })
            .unwrap();

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().downgrade();
                window.spawn(cx, async move |cx| {
                    TerminalPanel::load(workspace, cx.clone()).await
                })
            })
            .unwrap()
            .await
            .unwrap();

        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();

        terminal_panel.read_with(cx, |terminal_panel, cx| {
            assert!(!terminal_panel.restoring);
            assert_eq!(terminal_panel.active_pane.read(cx).items_len(), 0);
        });
        let serialized_state = cx
            .update(|cx| KeyValueStore::global(cx))
            .read_kvp(&serialization_key)
            .unwrap();
        assert_eq!(serialized_state, None);
    }

    #[gpui::test]
    async fn test_terminal_added_during_restore_is_serialized_after_restore(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let serialization_key = window_handle
            .update(cx, |multi_workspace, _, cx| {
                TerminalPanel::serialization_key(multi_workspace.workspace().read(cx)).unwrap()
            })
            .unwrap();

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.restoring = true;
                    terminal_panel.add_terminal_shell(
                        false,
                        None,
                        RevealStrategy::Never,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap();

        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        let suppressed_state = cx
            .update(|cx| KeyValueStore::global(cx))
            .read_kvp(&serialization_key)
            .unwrap();
        assert_eq!(
            suppressed_state, None,
            "serialization should stay suppressed while the panel is restoring"
        );

        let default_shell_task = window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.finish_restoration(false, window, cx)
                })
            })
            .unwrap();
        assert!(
            default_shell_task.is_none(),
            "no default terminal should spawn when a terminal already exists"
        );

        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        let serialized_state = cx
            .update(|cx| KeyValueStore::global(cx))
            .read_kvp(&serialization_key)
            .unwrap();
        assert!(
            serialized_state.is_some(),
            "terminal added during restore must be serialized once restoration finishes"
        );
    }

    #[gpui::test]
    async fn test_legacy_serialized_restore_keeps_interim_terminal_active(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.add_terminal_shell(
                        false,
                        None,
                        RevealStrategy::Never,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap();
        let interim_item_id = terminal_panel.read_with(cx, |terminal_panel, cx| {
            let pane = terminal_panel.active_pane.read(cx);
            assert_eq!(pane.items_len(), 1);
            pane.active_item().unwrap().item_id()
        });
        terminal_panel.update(cx, |terminal_panel, _| terminal_panel.restoring = true);

        let restored_items = window_handle
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                let project = workspace.read(cx).project().clone();
                deserialize_terminal_panel(
                    workspace.downgrade(),
                    project,
                    WorkspaceId::default(),
                    SerializedTerminalPanel {
                        items: SerializedItems::NoSplits(vec![12345]),
                        active_item_id: Some(12345),
                    },
                    terminal_panel.downgrade(),
                    window,
                    cx,
                )
            })
            .unwrap()
            .await
            .unwrap();
        cx.run_until_parked();
        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.finish_restoration(true, window, cx);
                })
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(restored_items, 1);
        terminal_panel.read_with(cx, |terminal_panel, cx| {
            let pane = terminal_panel.active_pane.read(cx);
            assert_eq!(pane.items_len(), 2);
            assert_eq!(
                pane.active_item().map(|item| item.item_id()),
                Some(interim_item_id),
                "interim terminal must stay active after a legacy format restore"
            );
        });
    }

    #[gpui::test]
    async fn test_split_restore_grafts_interim_pane(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.add_terminal_shell(
                        false,
                        None,
                        RevealStrategy::Never,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap();
        let interim_pane =
            terminal_panel.read_with(cx, |terminal_panel, _| terminal_panel.active_pane.clone());

        let restored_items = window_handle
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                let project = workspace.read(cx).project().clone();
                deserialize_terminal_panel(
                    workspace.downgrade(),
                    project,
                    WorkspaceId::default(),
                    SerializedTerminalPanel {
                        items: SerializedItems::WithSplits(SerializedPaneGroup::Pane(
                            SerializedPane {
                                active: true,
                                children: vec![12345],
                                active_item: Some(12345),
                                pinned_count: 0,
                            },
                        )),
                        active_item_id: None,
                    },
                    terminal_panel.downgrade(),
                    window,
                    cx,
                )
            })
            .unwrap()
            .await
            .unwrap();
        cx.run_until_parked();

        assert_eq!(restored_items, 1);
        terminal_panel.read_with(cx, |terminal_panel, cx| {
            let panes = terminal_panel.center.panes();
            assert_eq!(
                panes.len(),
                2,
                "both the restored pane and the interim pane must survive"
            );
            let interim_panes_kept = panes.iter().filter(|pane| **pane == &interim_pane).count();
            assert_eq!(
                interim_panes_kept, 1,
                "interim pane must be grafted into the restored center"
            );
            assert_eq!(interim_pane.read(cx).items_len(), 1);
            assert_ne!(
                terminal_panel.active_pane, interim_pane,
                "unfocused interim pane must not become the active pane"
            );
        });
    }

    #[gpui::test]
    async fn test_local_terminal_in_local_project(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        let result = window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    terminal_panel.add_terminal_shell(
                        true,
                        None,
                        RevealStrategy::Always,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await;

        assert!(
            result.is_ok(),
            "local terminal should successfully create in local project"
        );
    }

    struct FocusOnlyModal {
        focus_handle: gpui::FocusHandle,
    }
    impl gpui::EventEmitter<gpui::DismissEvent> for FocusOnlyModal {}
    impl gpui::Focusable for FocusOnlyModal {
        fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
            self.focus_handle.clone()
        }
    }
    impl Render for FocusOnlyModal {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            gpui::div().track_focus(&self.focus_handle)
        }
    }
    impl workspace::ModalView for FocusOnlyModal {}

    async fn open_center_display_terminal(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) {
        workspace
            .update_in(cx, |workspace, window, cx| {
                TerminalPanel::add_center_terminal(workspace, window, cx, |_, cx| {
                    let terminal = cx.new(|cx| {
                        terminal::TerminalBuilder::new_display_only(
                            terminal::terminal_settings::CursorShape::default(),
                            terminal::terminal_settings::AlternateScroll::On,
                            None,
                            0,
                            cx.background_executor(),
                            util::paths::PathStyle::local(),
                        )
                        .subscribe(cx)
                    });
                    gpui::Task::ready(Ok(terminal))
                })
            })
            .await
            .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_center_terminal_keeps_focus_on_active_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window_handle
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        let modal_focus_handle = workspace.update_in(cx, |workspace, window, cx| {
            let focus_handle = cx.focus_handle();
            workspace.toggle_modal(window, cx, {
                let focus_handle = focus_handle.clone();
                move |_, _| FocusOnlyModal { focus_handle }
            });
            focus_handle
        });

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(workspace.has_active_modal(window, cx));
            assert!(
                modal_focus_handle.is_focused(window),
                "the modal should hold focus before the terminal is created"
            );
        });

        open_center_display_terminal(&workspace, cx).await;

        workspace.update_in(cx, |_, window, _| {
            assert!(
                modal_focus_handle.is_focused(window),
                "a background center terminal must not steal focus from an active modal"
            );
        });
    }

    #[gpui::test]
    async fn test_center_terminal_takes_focus_without_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window_handle
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        open_center_display_terminal(&workspace, cx).await;

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(!workspace.has_active_modal(window, cx));
            let terminal_view = workspace
                .active_pane()
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<TerminalView>())
                .expect("the new center terminal should be the active item");
            assert!(
                terminal_view.focus_handle(cx).contains_focused(window, cx),
                "with no modal open, a new center terminal should take focus"
            );
        });
    }

    #[gpui::test]
    async fn test_panel_terminal_keeps_focus_on_active_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;
        let workspace = window_handle
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .expect("Failed to read workspace");
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        let modal_focus_handle = workspace.update_in(cx, |workspace, window, cx| {
            let focus_handle = cx.focus_handle();
            workspace.toggle_modal(window, cx, {
                let focus_handle = focus_handle.clone();
                move |_, _| FocusOnlyModal { focus_handle }
            });
            focus_handle
        });

        terminal_panel
            .update_in(cx, |terminal_panel, window, cx| {
                terminal_panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
            })
            .await
            .expect("Failed to spawn a panel terminal");
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(
                workspace.has_active_modal(window, cx),
                "a panel terminal that finishes spawning must not dismiss an active modal"
            );
            assert!(
                modal_focus_handle.is_focused(window),
                "a panel terminal that finishes spawning must not steal focus from an active modal"
            );
        });
        terminal_panel.read_with(cx, |terminal_panel, cx| {
            assert_eq!(
                terminal_panel.active_pane.read(cx).items_len(),
                1,
                "the terminal should still be added to the panel"
            );
        });
    }

    #[gpui::test]
    async fn test_panel_terminal_takes_focus_without_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        terminal_panel
            .update_in(cx, |terminal_panel, window, cx| {
                terminal_panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
            })
            .await
            .expect("Failed to spawn a panel terminal");
        cx.run_until_parked();

        terminal_panel.update_in(cx, |terminal_panel, window, cx| {
            let terminal_view = terminal_panel
                .active_pane
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<TerminalView>())
                .expect("the new terminal should be the active panel item");
            assert!(
                terminal_view.focus_handle(cx).contains_focused(window, cx),
                "with no modal open, a new panel terminal should take focus"
            );
        });
    }

    #[gpui::test]
    async fn test_task_terminal_keeps_focus_on_active_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;
        let workspace = window_handle
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .expect("Failed to read workspace");
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        let modal_focus_handle = workspace.update_in(cx, |workspace, window, cx| {
            let focus_handle = cx.focus_handle();
            workspace.toggle_modal(window, cx, {
                let focus_handle = focus_handle.clone();
                move |_, _| FocusOnlyModal { focus_handle }
            });
            focus_handle
        });

        terminal_panel
            .update_in(cx, |terminal_panel, window, cx| {
                terminal_panel.add_terminal_task(echo_task(), RevealStrategy::Always, window, cx)
            })
            .await
            .expect("Failed to spawn a task terminal");
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(
                workspace.has_active_modal(window, cx),
                "a task terminal that finishes spawning must not dismiss an active modal"
            );
            assert!(
                modal_focus_handle.is_focused(window),
                "a task terminal that finishes spawning must not steal focus from an active modal"
            );
        });
        terminal_panel.read_with(cx, |terminal_panel, cx| {
            assert_eq!(
                terminal_panel.active_pane.read(cx).items_len(),
                1,
                "the task terminal should still be added to the panel"
            );
        });
    }

    #[gpui::test]
    async fn test_task_terminal_takes_focus_without_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        terminal_panel
            .update_in(cx, |terminal_panel, window, cx| {
                terminal_panel.add_terminal_task(echo_task(), RevealStrategy::Always, window, cx)
            })
            .await
            .expect("Failed to spawn a task terminal");
        cx.run_until_parked();

        terminal_panel.update_in(cx, |terminal_panel, window, cx| {
            let terminal_view = terminal_panel
                .active_pane
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<TerminalView>())
                .expect("the new task terminal should be the active panel item");
            assert!(
                terminal_view.focus_handle(cx).contains_focused(window, cx),
                "with no modal open, a new task terminal should take focus"
            );
        });
    }

    #[gpui::test]
    async fn test_finished_restoration_keeps_focus_on_active_modal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;
        let workspace = window_handle
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .expect("Failed to read workspace");
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        terminal_panel
            .update_in(cx, |terminal_panel, window, cx| {
                terminal_panel.add_terminal_shell(false, None, RevealStrategy::Never, window, cx)
            })
            .await
            .expect("Failed to spawn a panel terminal");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_panel::<TerminalPanel>(window, cx);
        });
        cx.run_until_parked();

        let modal_focus_handle = workspace.update_in(cx, |workspace, window, cx| {
            let focus_handle = cx.focus_handle();
            workspace.toggle_modal(window, cx, {
                let focus_handle = focus_handle.clone();
                move |_, _| FocusOnlyModal { focus_handle }
            });
            focus_handle
        });
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(
                workspace.active_item(cx).is_none()
                    && workspace
                        .is_dock_at_position_open(terminal_panel.read(cx).position(window, cx), cx),
                "the restoration focus conditions should hold, otherwise this test is vacuous"
            );
        });

        window_handle
            .update(cx, |_, window, cx| {
                let workspace = workspace.downgrade();
                let terminal_panel = terminal_panel.downgrade();
                window.spawn(cx, async move |cx| {
                    TerminalPanel::restore_serialized_state(workspace, terminal_panel, cx).await
                })
            })
            .expect("Failed to restore serialized state")
            .await
            .expect("Failed to restore serialized state");
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(
                workspace.has_active_modal(window, cx),
                "finishing restoration must not dismiss an active modal"
            );
            assert!(
                modal_focus_handle.is_focused(window),
                "finishing restoration must not steal focus from an active modal"
            );
        });
    }

    // On Windows `echo` is a shell builtin rather than an executable, so spawning it directly fails.
    fn echo_task() -> SpawnInTerminal {
        let (command, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".to_owned(), "echo".to_owned()])
        } else {
            ("echo", Vec::new())
        };
        SpawnInTerminal {
            command: Some(command.to_owned()),
            args,
            ..SpawnInTerminal::default()
        }
    }

    async fn init_workspace_with_panel(
        cx: &mut TestAppContext,
    ) -> (gpui::WindowHandle<MultiWorkspace>, Entity<TerminalPanel>) {
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                    workspace.add_panel(panel.clone(), window, cx);
                    panel
                })
            })
            .expect("Failed to initialize workspace with terminal panel");

        (window_handle, terminal_panel)
    }

    #[gpui::test]
    async fn test_terminal_panel_starts_open_follows_setting(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    assert!(
                        !terminal_panel.starts_open(window, cx),
                        "terminal panel should not start open by default"
                    );
                });
            })
            .expect("Failed to read terminal panel starts_open default");

        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.terminal.get_or_insert_default().starts_open = Some(true);
            });
        });

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |terminal_panel, cx| {
                    assert!(
                        terminal_panel.starts_open(window, cx),
                        "terminal panel should start open when configured"
                    );
                });
            })
            .expect("Failed to read configured terminal panel starts_open");
    }

    #[gpui::test]
    async fn test_new_terminal_opens_in_panel_by_default(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;

        let panel_items_before =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());
        let center_items_before = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::new_terminal(
                        workspace,
                        &workspace::NewTerminal::default(),
                        window,
                        cx,
                    );
                })
            })
            .expect("Failed to dispatch new_terminal");

        cx.run_until_parked();

        let panel_items_after =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());
        let center_items_after = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");

        assert_eq!(
            panel_items_after,
            panel_items_before + 1,
            "Terminal should be added to the panel when no center terminal is focused"
        );
        assert_eq!(
            center_items_after, center_items_before,
            "Center pane should not gain a new terminal"
        );
    }

    #[gpui::test]
    async fn test_new_terminal_opens_in_center_when_center_terminal_focused(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::add_center_terminal(workspace, window, cx, |project, cx| {
                        project.create_terminal_shell(None, cx)
                    })
                })
            })
            .expect("Failed to update workspace")
            .await
            .expect("Failed to create center terminal");
        cx.run_until_parked();

        let center_items_before = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");
        assert_eq!(center_items_before, 1, "Center pane should have 1 terminal");

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let active_item = workspace
                        .active_pane()
                        .read(cx)
                        .active_item()
                        .expect("Center pane should have an active item");
                    let terminal_view = active_item
                        .downcast::<TerminalView>()
                        .expect("Active center item should be a TerminalView");
                    window.focus(&terminal_view.focus_handle(cx), cx);
                })
            })
            .expect("Failed to focus terminal view");
        cx.run_until_parked();

        let panel_items_before =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::new_terminal(
                        workspace,
                        &workspace::NewTerminal::default(),
                        window,
                        cx,
                    );
                })
            })
            .expect("Failed to dispatch new_terminal");
        cx.run_until_parked();

        let center_items_after = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");
        let panel_items_after =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());

        assert_eq!(
            center_items_after,
            center_items_before + 1,
            "New terminal should be added to the center pane"
        );
        assert_eq!(
            panel_items_after, panel_items_before,
            "Terminal panel should not gain a new terminal"
        );
    }

    #[gpui::test]
    async fn test_new_terminal_opens_in_panel_when_panel_focused(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| {
                    panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                })
            })
            .expect("Failed to update workspace")
            .await
            .expect("Failed to create panel terminal");
        cx.run_until_parked();

        window_handle
            .update(cx, |_, window, cx| {
                window.focus(&terminal_panel.read(cx).focus_handle(cx), cx);
            })
            .expect("Failed to focus terminal panel");
        cx.run_until_parked();

        let panel_items_before =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());

        let center_items_before = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::new_terminal(
                        workspace,
                        &workspace::NewTerminal::default(),
                        window,
                        cx,
                    );
                })
            })
            .expect("Failed to dispatch new_terminal");
        cx.run_until_parked();

        let panel_items_after =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());
        let center_items_after = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");

        assert_eq!(
            panel_items_after,
            panel_items_before + 1,
            "New terminal should be added to the panel when panel is focused"
        );
        assert_eq!(
            center_items_after, center_items_before,
            "Center pane should not gain a new terminal"
        );
    }

    #[gpui::test]
    async fn test_new_local_terminal_opens_in_center_when_center_terminal_focused(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::add_center_terminal(workspace, window, cx, |project, cx| {
                        project.create_terminal_shell(None, cx)
                    })
                })
            })
            .expect("Failed to update workspace")
            .await
            .expect("Failed to create center terminal");
        cx.run_until_parked();

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let active_item = workspace
                        .active_pane()
                        .read(cx)
                        .active_item()
                        .expect("Center pane should have an active item");
                    let terminal_view = active_item
                        .downcast::<TerminalView>()
                        .expect("Active center item should be a TerminalView");
                    window.focus(&terminal_view.focus_handle(cx), cx);
                })
            })
            .expect("Failed to focus terminal view");
        cx.run_until_parked();

        let center_items_before = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");
        let panel_items_before =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::new_terminal(
                        workspace,
                        &workspace::NewTerminal { local: true },
                        window,
                        cx,
                    );
                })
            })
            .expect("Failed to dispatch new_terminal with local=true");
        cx.run_until_parked();

        let center_items_after = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");
        let panel_items_after =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());

        assert_eq!(
            center_items_after,
            center_items_before + 1,
            "New local terminal should be added to the center pane"
        );
        assert_eq!(
            panel_items_after, panel_items_before,
            "Terminal panel should not gain a new terminal"
        );
    }

    #[gpui::test]
    async fn test_new_terminal_opens_in_panel_when_panel_focused_and_center_has_terminal(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);

        let (window_handle, terminal_panel) = init_workspace_with_panel(cx).await;

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::add_center_terminal(workspace, window, cx, |project, cx| {
                        project.create_terminal_shell(None, cx)
                    })
                })
            })
            .expect("Failed to update workspace")
            .await
            .expect("Failed to create center terminal");
        cx.run_until_parked();

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| {
                    panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                })
            })
            .expect("Failed to update workspace")
            .await
            .expect("Failed to create panel terminal");
        cx.run_until_parked();

        window_handle
            .update(cx, |_, window, cx| {
                window.focus(&terminal_panel.read(cx).focus_handle(cx), cx);
            })
            .expect("Failed to focus terminal panel");
        cx.run_until_parked();

        let panel_items_before =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());
        let center_items_before = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    TerminalPanel::new_terminal(
                        workspace,
                        &workspace::NewTerminal::default(),
                        window,
                        cx,
                    );
                })
            })
            .expect("Failed to dispatch new_terminal");
        cx.run_until_parked();

        let panel_items_after =
            terminal_panel.read_with(cx, |panel, cx| panel.active_pane.read(cx).items_len());
        let center_items_after = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items_len()
            })
            .expect("Failed to read center pane items");

        assert_eq!(
            panel_items_after,
            panel_items_before + 1,
            "New terminal should go to panel when panel is focused, even if center has a terminal"
        );
        assert_eq!(
            center_items_after, center_items_before,
            "Center pane should not gain a new terminal when panel is focused"
        );
    }

    fn group_state(panel: &TerminalPanel, cx: &App) -> Vec<(Vec<EntityId>, EntityId)> {
        panel
            .center
            .panes()
            .into_iter()
            .map(|pane| {
                let pane = pane.read(cx);
                (
                    pane.items().map(|item| item.item_id()).collect(),
                    pane.active_item().unwrap().item_id(),
                )
            })
            .collect()
    }

    #[gpui::test]
    async fn test_join_and_unjoin_selected_terminals(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        for _ in 0..4 {
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| {
                        panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                    })
                })
                .unwrap()
                .await
                .unwrap();
        }
        cx.run_until_parked();

        let ids: Vec<EntityId> = terminal_panel.read_with(cx, |panel, cx| {
            panel.active_pane.read(cx).items().map(|i| i.item_id()).collect()
        });
        assert_eq!(ids.len(), 4);

        terminal_panel.update(cx, |panel, _| {
            panel.selected_terminals = vec![ids[0], ids[2]];
        });
        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| panel.join_terminals(window, cx))
            })
            .unwrap();
        cx.run_until_parked();

        terminal_panel.read_with(cx, |panel, cx| {
            let state = group_state(panel, cx);
            let mut active: Vec<EntityId> = state.iter().map(|(_, active)| *active).collect();
            active.sort();
            let mut expected = vec![ids[0], ids[2]];
            expected.sort();
            assert_eq!(state.len(), 2, "{state:?}");
            assert_eq!(active, expected, "joined terminals must be the visible group");
            let total: usize = state.iter().map(|(items, _)| items.len()).sum();
            assert_eq!(total, 4);
        });

        let item_id = terminal_panel.read_with(cx, |panel, cx| {
            panel.center.panes()[1]
                .read(cx)
                .active_item()
                .unwrap()
                .item_id()
        });
        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| panel.unjoin_terminal(item_id, window, cx))
            })
            .unwrap();
        cx.run_until_parked();
        terminal_panel.read_with(cx, |panel, cx| {
            let state = group_state(panel, cx);
            assert_eq!(state.len(), 1, "{state:?}");
            assert_eq!(state[0].0.len(), 4);
        });
    }

    #[gpui::test]
    async fn test_join_sequences_show_exactly_selected(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();

        for _ in 0..5 {
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| {
                        panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                    })
                })
                .unwrap()
                .await
                .unwrap();
        }
        cx.run_until_parked();
        let ids: Vec<EntityId> = terminal_panel.read_with(cx, |panel, cx| {
            panel.active_pane.read(cx).items().map(|i| i.item_id()).collect()
        });

        let sequences: Vec<Vec<usize>> = vec![
            vec![0, 2],
            vec![1, 3],
            vec![4, 0, 3],
            vec![2, 3],
            vec![0, 1, 2, 3, 4],
            vec![3, 1],
            vec![4, 2],
        ];
        for sequence in sequences {
            let selection: Vec<EntityId> = sequence.iter().map(|ix| ids[*ix]).collect();
            terminal_panel.update(cx, |panel, _| {
                panel.selected_terminals = selection.clone();
            });
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| panel.join_terminals(window, cx))
                })
                .unwrap();
            cx.run_until_parked();
            terminal_panel.read_with(cx, |panel, cx| {
                let state = group_state(panel, cx);
                let actives: Vec<EntityId> = state.iter().map(|(_, active)| *active).collect();
                let mut sorted_actives = actives.clone();
                sorted_actives.sort();
                let mut sorted_selection = selection.clone();
                sorted_selection.sort();
                assert_eq!(
                    sorted_actives, sorted_selection,
                    "sequence {sequence:?}: visible group must equal selection, state {state:?}"
                );
                let total: usize = state.iter().map(|(items, _)| items.len()).sum();
                assert_eq!(total, 5, "sequence {sequence:?}: no terminal lost");
            });
        }
    }

    #[gpui::test]
    async fn test_unjoin_removes_only_one_member(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();
        for _ in 0..5 {
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| {
                        panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                    })
                })
                .unwrap()
                .await
                .unwrap();
        }
        cx.run_until_parked();
        let ids: Vec<EntityId> = terminal_panel.read_with(cx, |panel, cx| {
            panel.active_pane.read(cx).items().map(|i| i.item_id()).collect()
        });
        terminal_panel.update(cx, |panel, _| {
            panel.selected_terminals = vec![ids[0], ids[1], ids[2], ids[3]];
        });
        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| panel.join_terminals(window, cx))
            })
            .unwrap();
        cx.run_until_parked();

        for expected_visible in [3usize, 2] {
            let item_id = terminal_panel.read_with(cx, |panel, cx| {
                panel.center.panes()[1]
                    .read(cx)
                    .active_item()
                    .unwrap()
                    .item_id()
            });
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel
                        .update(cx, |panel, cx| panel.unjoin_terminal(item_id, window, cx))
                })
                .unwrap();
            cx.run_until_parked();
            terminal_panel.read_with(cx, |panel, cx| {
                let state = group_state(panel, cx);
                assert_eq!(state.len(), expected_visible, "{state:?}");
                let total: usize = state.iter().map(|(items, _)| items.len()).sum();
                assert_eq!(total, 5);
            });
        }
    }

    #[gpui::test]
    async fn test_clicking_other_group_switches_group_without_merging(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();
        for _ in 0..4 {
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| {
                        panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                    })
                })
                .unwrap()
                .await
                .unwrap();
            cx.run_until_parked();
        }
        let ids: Vec<EntityId> = terminal_panel.read_with(cx, |panel, cx| {
            let mut ids = panel.present_terminal_ids(cx);
            ids.sort_by_key(|id| {
                panel
                    .groups
                    .iter()
                    .position(|group| group.contains(id))
                    .unwrap()
            });
            ids
        });
        terminal_panel.read_with(cx, |panel, cx| {
            let state = group_state(panel, cx);
            assert_eq!(state.len(), 1, "new terminals must not split: {state:?}");
            assert_eq!(panel.groups.len(), 4);
        });

        terminal_panel.update(cx, |panel, _| {
            panel.selected_terminals = vec![ids[0], ids[1]];
        });
        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| panel.join_terminals(window, cx))
            })
            .unwrap();
        cx.run_until_parked();

        for (target, expected_visible) in [(ids[2], 1usize), (ids[0], 2), (ids[3], 1), (ids[1], 2)]
        {
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| panel.show_terminal(target, window, cx))
                })
                .unwrap();
            cx.run_until_parked();
            terminal_panel.read_with(cx, |panel, cx| {
                let state = group_state(panel, cx);
                assert_eq!(state.len(), expected_visible, "{state:?}");
                assert!(
                    panel.pane_active_ids(cx).contains(&target),
                    "target must be visible: {state:?}"
                );
                let joined = panel.group_index_of(ids[0]) == panel.group_index_of(ids[1]);
                assert!(joined, "group membership must not change when switching");
                assert_eq!(panel.groups.len(), 3);
            });
        }
    }

    #[gpui::test]
    async fn test_split_and_kill_active_terminal(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    cx.new(|cx| TerminalPanel::new(workspace, window, cx))
                })
            })
            .unwrap();
        for _ in 0..2 {
            window_handle
                .update(cx, |_, window, cx| {
                    terminal_panel.update(cx, |panel, cx| {
                        panel.add_terminal_shell(false, None, RevealStrategy::Always, window, cx)
                    })
                })
                .unwrap()
                .await
                .unwrap();
            cx.run_until_parked();
        }

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| panel.split_active_terminal(window, cx))
            })
            .unwrap();
        cx.run_until_parked();
        terminal_panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.present_terminal_ids(cx).len(), 3);
            assert_eq!(panel.center.panes().len(), 2);
        });

        window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| panel.kill_active_terminal(window, cx))
            })
            .unwrap();
        cx.run_until_parked();
        terminal_panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.present_terminal_ids(cx).len(), 2);
        });
    }

    fn set_max_tabs(cx: &mut TestAppContext, value: Option<usize>) {
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.workspace.max_tabs = value.map(|v| NonZero::new(v).unwrap())
            });
        });
    }

    pub fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }
}
