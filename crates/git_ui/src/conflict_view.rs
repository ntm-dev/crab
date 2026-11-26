use collections::{HashMap, HashSet};
use editor::{
    ConflictsOurs, ConflictsOursMarker, ConflictsOuter, ConflictsTheirs, ConflictsTheirsMarker,
    Editor, MultiBuffer, RowHighlightOptions,
    display_map::{BlockContext, BlockPlacement, BlockProperties, BlockStyle, CustomBlockId},
};
use gpui::{
    App, Context, Empty, Entity, EventEmitter, InteractiveElement as _, ParentElement as _,
    Subscription, Task, WeakEntity,
};
use i18n::t;
use language::{Anchor, Buffer, BufferId};
use project::{ConflictRegion, ConflictSet, ConflictSetUpdate};
use std::{ops::Range, sync::Arc};
use ui::{Tooltip, prelude::*};
use util::debug_panic;
use workspace::{ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, item::ItemHandle};

pub(crate) struct ConflictAddon {
    buffers: HashMap<BufferId, BufferConflicts>,
}

struct BufferConflicts {
    block_ids: Vec<(Range<Anchor>, CustomBlockId)>,
    conflict_set: Entity<ConflictSet>,
    _subscription: Subscription,
}

impl editor::Addon for ConflictAddon {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

pub fn register_editor(editor: &mut Editor, buffer: Entity<MultiBuffer>, cx: &mut Context<Editor>) {
    let is_singleton = editor.buffer().read(cx).is_singleton();
    if !editor.mode().is_full()
        || (!is_singleton && !editor.buffer().read(cx).all_diff_hunks_expanded())
        || editor.read_only(cx)
    {
        return;
    }

    editor.register_addon(ConflictAddon {
        buffers: Default::default(),
    });

    if is_singleton {
        let buffers = buffer.read(cx).all_buffers();
        for buffer in buffers {
            open_conflict_set_for_buffer(editor, buffer, cx);
        }
    }
}

fn open_conflict_set_for_buffer(
    _editor: &mut Editor,
    buffer: Entity<Buffer>,
    cx: &mut Context<Editor>,
) {
    let buffer = buffer.downgrade();

    cx.spawn(async move |editor, cx| {
        let buffer_id = buffer.read_with(cx, |buffer, _| buffer.remote_id())?;
        if let Some(conflict_set) = editor.read_with(cx, |editor, _| {
            editor
                .addon::<ConflictAddon>()
                .and_then(|addon| addon.buffers.get(&buffer_id))
                .map(|buffer_conflicts| buffer_conflicts.conflict_set.clone())
        })? {
            editor.update(cx, |editor, cx| {
                buffer_ranges_updated(editor, conflict_set, cx);
            })?;
            return anyhow::Ok(());
        }

        let Some(project) = editor.read_with(cx, |editor, _| editor.project().cloned())? else {
            return anyhow::Ok(());
        };
        let git_store = project.read_with(cx, |project, _| project.git_store().clone());
        let Some(buffer) = buffer.upgrade() else {
            return Ok(());
        };
        let conflict_set = git_store
            .update(cx, |git_store, cx| {
                git_store.open_conflict_set(buffer.clone(), cx)
            })
            .await;
        editor.update(cx, |editor, cx| {
            buffer_ranges_updated(editor, conflict_set, cx);
        })?;
        Ok(())
    })
    .detach();
}

pub(crate) fn buffer_ranges_updated(
    editor: &mut Editor,
    conflict_set: Entity<ConflictSet>,
    cx: &mut Context<Editor>,
) {
    let buffer_id = conflict_set.read(cx).snapshot.buffer_id;
    if editor.buffer().read(cx).buffer(buffer_id).is_none() {
        return;
    }

    let Some(conflict_addon) = editor.addon_mut::<ConflictAddon>() else {
        return;
    };
    let buffer_conflicts = conflict_addon.buffers.entry(buffer_id).or_insert_with(|| {
        let subscription = cx.subscribe(&conflict_set, conflicts_updated);
        BufferConflicts {
            block_ids: Vec::new(),
            conflict_set: conflict_set.clone(),
            _subscription: subscription,
        }
    });

    let conflict_set = buffer_conflicts.conflict_set.clone();
    let conflicts_len = conflict_set.read(cx).snapshot().conflicts.len();
    let addon_conflicts_len = buffer_conflicts.block_ids.len();
    conflicts_updated(
        editor,
        conflict_set,
        &ConflictSetUpdate {
            buffer_range: None,
            old_range: 0..addon_conflicts_len,
            new_range: 0..conflicts_len,
        },
        cx,
    );
}

pub(crate) fn buffers_removed(
    editor: &mut Editor,
    removed_buffer_ids: &[BufferId],
    cx: &mut Context<Editor>,
) {
    let mut removed_block_ids = HashSet::default();
    let Some(conflict_addon) = editor.addon_mut::<ConflictAddon>() else {
        return;
    };
    conflict_addon.buffers.retain(|buffer_id, buffer| {
        if removed_buffer_ids.contains(buffer_id) {
            removed_block_ids.extend(buffer.block_ids.iter().map(|(_, block_id)| *block_id));
            false
        } else {
            true
        }
    });
    editor.remove_blocks(removed_block_ids, None, cx);
}

#[ztracing::instrument(skip_all)]
fn conflicts_updated(
    editor: &mut Editor,
    conflict_set: Entity<ConflictSet>,
    event: &ConflictSetUpdate,
    cx: &mut Context<Editor>,
) {
    let buffer_id = conflict_set.read(cx).snapshot.buffer_id;
    let conflict_set = conflict_set.read(cx).snapshot();
    let multibuffer = editor.buffer().read(cx);
    let snapshot = multibuffer.snapshot(cx);
    let old_range = {
        let Some(conflict_addon) = editor.addon_mut::<ConflictAddon>() else {
            return;
        };
        let Some(buffer_conflicts) = conflict_addon.buffers.get(&buffer_id) else {
            return;
        };
        match buffer_conflicts.block_ids.get(event.old_range.clone()) {
            Some(_) => Some(event.old_range.clone()),
            None => {
                debug_panic!(
                    "conflicts updated event old range is invalid for buffer conflicts view (block_ids len is {:?}, old_range is {:?})",
                    buffer_conflicts.block_ids.len(),
                    event.old_range,
                );
                if event.old_range.start <= event.old_range.end {
                    Some(
                        event.old_range.start.min(buffer_conflicts.block_ids.len())
                            ..event.old_range.end.min(buffer_conflicts.block_ids.len()),
                    )
                } else {
                    None
                }
            }
        }
    };

    // Remove obsolete highlights and blocks
    let Some(conflict_addon) = editor.addon_mut::<ConflictAddon>() else {
        return;
    };
    if let Some((buffer_conflicts, old_range)) = conflict_addon
        .buffers
        .get_mut(&buffer_id)
        .zip(old_range.clone())
    {
        let old_conflicts = buffer_conflicts.block_ids[old_range].to_owned();
        let mut removed_highlighted_ranges = Vec::new();
        let mut removed_block_ids = HashSet::default();
        for (conflict_range, block_id) in old_conflicts {
            let Some(range) = snapshot.buffer_anchor_range_to_anchor_range(conflict_range) else {
                continue;
            };
            removed_highlighted_ranges.push(range.clone());
            removed_block_ids.insert(block_id);
        }

        editor.remove_gutter_highlights::<ConflictsOuter>(removed_highlighted_ranges.clone(), cx);

        editor.remove_highlighted_rows::<ConflictsOuter>(removed_highlighted_ranges.clone(), cx);
        editor.remove_highlighted_rows::<ConflictsOurs>(removed_highlighted_ranges.clone(), cx);
        editor
            .remove_highlighted_rows::<ConflictsOursMarker>(removed_highlighted_ranges.clone(), cx);
        editor.remove_highlighted_rows::<ConflictsTheirs>(removed_highlighted_ranges.clone(), cx);
        editor.remove_highlighted_rows::<ConflictsTheirsMarker>(
            removed_highlighted_ranges.clone(),
            cx,
        );
        editor.remove_blocks(removed_block_ids, None, cx);
    }

    // Add new highlights and blocks
    let editor_handle = cx.weak_entity();
    let new_conflicts = &conflict_set.conflicts[event.new_range.clone()];
    let mut converted_ranges = snapshot
        .ordered_buffer_anchor_ranges_to_anchor_ranges(new_conflicts.iter().flat_map(|conflict| {
            [
                conflict.range.start..conflict.range.start,
                conflict.range.clone(),
                conflict.ours.clone(),
                conflict.theirs.clone(),
            ]
        }))
        .into_iter();
    let mut blocks = Vec::new();
    for conflict in new_conflicts {
        let block_anchor = converted_ranges.next().flatten().map(|range| range.start);
        let outer = converted_ranges.next().flatten();
        let ours = converted_ranges.next().flatten();
        let theirs = converted_ranges.next().flatten();
        if let (Some(outer), Some(ours), Some(theirs)) = (outer, ours, theirs) {
            update_conflict_highlighting(editor, conflict, outer, ours, theirs, cx);
        }

        if let Some(anchor) = block_anchor {
            let editor_handle = editor_handle.clone();
            blocks.push(BlockProperties {
                placement: BlockPlacement::Above(anchor),
                height: Some(1),
                style: BlockStyle::Sticky,
                render: Arc::new({
                    let conflict = conflict.clone();
                    move |cx| render_conflict_buttons(&conflict, editor_handle.clone(), cx)
                }),
                priority: 0,
            })
        }
    }
    let new_block_ids = editor.insert_blocks(blocks, None, cx);

    let Some(conflict_addon) = editor.addon_mut::<ConflictAddon>() else {
        return;
    };
    if let Some((buffer_conflicts, old_range)) =
        conflict_addon.buffers.get_mut(&buffer_id).zip(old_range)
    {
        buffer_conflicts.block_ids.splice(
            old_range,
            new_conflicts
                .iter()
                .map(|conflict| conflict.range.clone())
                .zip(new_block_ids),
        );
    }
}

#[ztracing::instrument(skip_all)]
fn update_conflict_highlighting(
    editor: &mut Editor,
    conflict: &ConflictRegion,
    outer: Range<editor::Anchor>,
    ours: Range<editor::Anchor>,
    theirs: Range<editor::Anchor>,
    cx: &mut Context<Editor>,
) {
    log::debug!("update conflict highlighting for {conflict:?}");

    let ours_background = |cx: &App| cx.theme().colors().version_control_conflict_marker_ours;
    let theirs_background = |cx: &App| cx.theme().colors().version_control_conflict_marker_theirs;

    let options = RowHighlightOptions {
        include_gutter: true,
        ..Default::default()
    };

    editor.insert_gutter_highlight::<ConflictsOuter>(
        outer.start..theirs.end,
        |cx| cx.theme().colors().editor_background,
        cx,
    );

    // Prevent diff hunk highlighting within the entire conflict region.
    editor.highlight_rows::<ConflictsOuter>(outer.clone(), theirs_background, options, cx);
    editor.highlight_rows::<ConflictsOurs>(ours.clone(), ours_background, options, cx);
    editor.highlight_rows::<ConflictsOursMarker>(
        outer.start..ours.start,
        ours_background,
        options,
        cx,
    );
    editor.highlight_rows::<ConflictsTheirs>(theirs.clone(), theirs_background, options, cx);
    editor.highlight_rows::<ConflictsTheirsMarker>(
        theirs.end..outer.end,
        theirs_background,
        options,
        cx,
    );
}

fn render_conflict_buttons(
    conflict: &ConflictRegion,
    editor: WeakEntity<Editor>,
    cx: &mut BlockContext,
) -> AnyElement {
    h_flex()
        .id(cx.block_id)
        .h(cx.line_height)
        .ml(cx.margins.gutter.width)
        .gap_1()
        .bg(cx.theme().colors().editor_background)
        .child(
            Button::new("head", t!("Accept Current Change"))
            .style(ButtonStyle::Transparent)
            .color(Color::Muted)
            .label_size(LabelSize::Small)
            .on_click({
                let editor = editor.clone();
                let conflict = conflict.clone();
                let ours = conflict.ours.clone();
                move |_, window, cx| {
                    resolve_conflict(
                        editor.clone(),
                        conflict.clone(),
                        vec![ours.clone()],
                        window,
                        cx,
                    )
                    .detach()
                }
            }),
        )
        .child(Label::new("|").size(LabelSize::Small).color(Color::Muted))
        .child(
            Button::new("origin", t!("Accept Incoming Change"))
            .style(ButtonStyle::Transparent)
            .color(Color::Muted)
            .label_size(LabelSize::Small)
            .on_click({
                let editor = editor.clone();
                let conflict = conflict.clone();
                let theirs = conflict.theirs.clone();
                move |_, window, cx| {
                    resolve_conflict(
                        editor.clone(),
                        conflict.clone(),
                        vec![theirs.clone()],
                        window,
                        cx,
                    )
                    .detach()
                }
            }),
        )
        .child(Label::new("|").size(LabelSize::Small).color(Color::Muted))
        .child(
            Button::new("both", t!("Accept Both Changes"))
                .style(ButtonStyle::Transparent)
                .color(Color::Muted)
                .label_size(LabelSize::Small)
                .on_click({
                    let conflict = conflict.clone();
                    let ours = conflict.ours.clone();
                    let theirs = conflict.theirs.clone();
                    move |_, window, cx| {
                        resolve_conflict(
                            editor.clone(),
                            conflict.clone(),
                            vec![ours.clone(), theirs.clone()],
                            window,
                            cx,
                        )
                        .detach()
                    }
                }),
        )
        .into_any()
}

pub(crate) fn resolve_conflict(
    editor: WeakEntity<Editor>,
    resolved_conflict: ConflictRegion,
    ranges: Vec<Range<Anchor>>,
    window: &mut Window,
    cx: &mut App,
) -> Task<()> {
    window.spawn(cx, async move |cx| {
        editor
            .update(cx, |editor, cx| {
                let multibuffer = editor.buffer().clone();
                let buffer_id = resolved_conflict.ours.end.buffer_id;
                let buffer = multibuffer.read(cx).buffer(buffer_id)?;
                resolved_conflict.resolve(buffer.clone(), &ranges, cx);
                let conflict_addon = editor.addon_mut::<ConflictAddon>()?;
                let snapshot = multibuffer.read(cx).snapshot(cx);
                let buffer_snapshot = buffer.read(cx).snapshot();
                let state = conflict_addon
                    .buffers
                    .get_mut(&buffer_snapshot.remote_id())?;
                let ix = state
                    .block_ids
                    .binary_search_by(|(range, _)| {
                        range
                            .start
                            .cmp(&resolved_conflict.range.start, &buffer_snapshot)
                    })
                    .ok()?;
                let &(_, block_id) = &state.block_ids[ix];
                let range =
                    snapshot.buffer_anchor_range_to_anchor_range(resolved_conflict.range)?;

                editor.remove_gutter_highlights::<ConflictsOuter>(vec![range.clone()], cx);

                editor.remove_highlighted_rows::<ConflictsOuter>(vec![range.clone()], cx);
                editor.remove_highlighted_rows::<ConflictsOurs>(vec![range.clone()], cx);
                editor.remove_highlighted_rows::<ConflictsTheirs>(vec![range.clone()], cx);
                editor.remove_highlighted_rows::<ConflictsOursMarker>(vec![range.clone()], cx);
                editor.remove_highlighted_rows::<ConflictsTheirsMarker>(vec![range], cx);
                editor.remove_blocks(HashSet::from_iter([block_id]), None, cx);
                Some(())
            })
            .ok();
    })
}

pub struct ConflictNavigationToolbar {
    editor: Option<WeakEntity<Editor>>,
    _subscription: Option<Subscription>,
}

impl ConflictNavigationToolbar {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            editor: None,
            _subscription: None,
        }
    }

    fn conflict_starts(editor: &Editor, cx: &App) -> Vec<editor::Anchor> {
        let Some(addon) = editor.addon::<ConflictAddon>() else {
            return Vec::new();
        };
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let mut starts: Vec<editor::Anchor> = addon
            .buffers
            .values()
            .flat_map(|conflicts| conflicts.block_ids.iter())
            .filter_map(|(range, _)| {
                snapshot
                    .buffer_anchor_range_to_anchor_range(range.clone())
                    .map(|range| range.start)
            })
            .collect();
        starts.sort_by(|a, b| a.cmp(b, &snapshot));
        starts
    }

    fn navigate(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref().and_then(|editor| editor.upgrade()) else {
            return;
        };
        editor.update(cx, |editor, cx| {
            let starts = Self::conflict_starts(editor, cx);
            if starts.is_empty() {
                return;
            }
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let cursor = editor.selections.newest_anchor().head();
            let target = if forward {
                starts
                    .iter()
                    .find(|start| start.cmp(&cursor, &snapshot).is_gt())
                    .or(starts.first())
            } else {
                starts
                    .iter()
                    .rev()
                    .find(|start| start.cmp(&cursor, &snapshot).is_lt())
                    .or(starts.last())
            };
            if let Some(&target) = target {
                editor.change_selections(
                    editor::SelectionEffects::scroll(editor::scroll::Autoscroll::center()),
                    window,
                    cx,
                    |selections| selections.select_ranges([target..target]),
                );
            }
        });
    }
}

impl EventEmitter<ToolbarItemEvent> for ConflictNavigationToolbar {}

impl ToolbarItemView for ConflictNavigationToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        let editor = active_pane_item.and_then(|item| item.act_as::<Editor>(cx));
        self._subscription = editor
            .as_ref()
            .map(|editor| cx.observe(editor, |_, _, cx| cx.notify()));
        self.editor = editor.map(|editor| editor.downgrade());
        ToolbarItemLocation::Hidden
    }
}

impl Render for ConflictNavigationToolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_conflicts = self
            .editor
            .as_ref()
            .and_then(|editor| editor.upgrade())
            .is_some_and(|editor| !Self::conflict_starts(editor.read(cx), cx).is_empty());
        if !has_conflicts {
            return Empty.into_any_element();
        }
        h_flex()
            .gap_0p5()
            .child(
                IconButton::new("conflict-previous", IconName::ArrowUp)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(t!("Previous Conflict")))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate(false, window, cx))),
            )
            .child(
                IconButton::new("conflict-next", IconName::ArrowDown)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(t!("Next Conflict")))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate(true, window, cx))),
            )
            .into_any_element()
    }
}
