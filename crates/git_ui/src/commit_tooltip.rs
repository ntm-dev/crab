use crate::commit_view::CommitView;
use anyhow::Result;
use askpass::AskPassDelegate;
use editor::hover_markdown_style;
use futures::Future;
use git::blame::BlameEntry;
use git::repository::CommitSummary;
use git::{GitRemote, commit::ParsedCommitMessage};
use git_ui_core::askpass_modal::AskPassModal;
use git_ui_core::notifications::show_error_toast;
use gpui::{
    AbsoluteLength, App, Asset, Element, Entity, MouseButton, ParentElement, Pixels, Render,
    ScrollHandle, StatefulInteractiveElement, Task, WeakEntity, Window, prelude::*,
};
use i18n::t;
use language::{DiffOptions, word_diff_ranges};
use markdown::{Markdown, MarkdownElement};
use notifications::status_toast::StatusToast;
use project::git_store::{Repository, UnshallowState};
use settings::Settings;
use std::hash::Hash;
use theme_settings::ThemeSettings;
use time::{OffsetDateTime, UtcOffset};
use ui::{
    Avatar, Chip, CopyButton, Divider, ScrollAxes, Scrollbars, Tooltip, WithScrollbar, prelude::*,
    tooltip_container,
};
use util::ResultExt as _;
use workspace::Workspace;

#[derive(Clone, Debug)]
pub struct CommitDetails {
    pub sha: SharedString,
    pub author_name: SharedString,
    pub author_email: SharedString,
    pub commit_time: OffsetDateTime,
    pub message: Option<ParsedCommitMessage>,
    pub tag_names: Vec<SharedString>,
    pub boundary: bool,
}

const MAX_COMMIT_TOOLTIP_TAG_CHIPS: usize = 2;

pub(crate) fn commit_tag_chips(tag_names: &[SharedString]) -> Option<impl IntoElement> {
    if tag_names.is_empty() {
        return None;
    }

    let (visible_tags, hidden_tags) =
        tag_names.split_at(tag_names.len().min(MAX_COMMIT_TOOLTIP_TAG_CHIPS));

    Some(
        h_flex().max_w(relative(0.6)).gap_1().child(
            h_flex()
                .gap_1()
                .min_w_0()
                .children(
                    visible_tags
                        .iter()
                        .map(|tag_name| Chip::new(tag_name.clone()).truncate()),
                )
                .when(!hidden_tags.is_empty(), |this| {
                    let hidden_tags = hidden_tags.to_vec();
                    this.child(Chip::new(format!("+{}", hidden_tags.len())).tooltip(
                        Tooltip::element(move |_window, cx| {
                            v_flex()
                                .gap_1()
                                .children(itertools::Itertools::intersperse_with(
                                    hidden_tags.iter().map(|tag_name| {
                                        Label::new(tag_name.clone())
                                            .size(LabelSize::Small)
                                            .buffer_font(cx)
                                            .into_any_element()
                                    }),
                                    || Divider::horizontal().into_any_element(),
                                ))
                                .into_any_element()
                        }),
                    ))
                })
                .child(Divider::vertical()),
        ),
    )
}

const COMMIT_AVATAR_BORDER_WIDTH: Pixels = px(1.);

pub struct CommitAvatar<'a> {
    sha: &'a SharedString,
    author_email: Option<SharedString>,
    remote: Option<&'a GitRemote>,
    size: Option<AbsoluteLength>,
}

impl<'a> CommitAvatar<'a> {
    pub fn new(
        sha: &'a SharedString,
        author_email: Option<SharedString>,
        remote: Option<&'a GitRemote>,
    ) -> Self {
        Self {
            sha,
            author_email,
            remote,
            size: None,
        }
    }

    pub fn from_commit_details(details: &'a CommitDetails) -> Self {
        Self {
            sha: &details.sha,
            author_email: Some(details.author_email.clone()),
            remote: details
                .message
                .as_ref()
                .and_then(|details| details.remote.as_ref()),
            size: None,
        }
    }

    pub fn size(mut self, size: impl Into<AbsoluteLength>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn rendered_size(size: impl Into<AbsoluteLength>, window: &Window) -> Pixels {
        size.into().to_pixels(window.rem_size()) + COMMIT_AVATAR_BORDER_WIDTH * 2.
    }

    pub fn render(&'a self, window: &mut Window, cx: &mut App) -> AnyElement {
        let border_color = cx.theme().colors().border_variant;

        match self.avatar(window, cx) {
            None => {
                let container_size = self.size.map(|size| Self::rendered_size(size, window));

                h_flex()
                    .when_some(container_size, |this, size| this.size(size))
                    .justify_center()
                    .rounded_full()
                    .border(COMMIT_AVATAR_BORDER_WIDTH)
                    .border_color(border_color)
                    .bg(cx.theme().colors().element_disabled)
                    .child(
                        Icon::new(IconName::Person)
                            .color(Color::Muted)
                            .size(IconSize::XSmall),
                    )
                    .into_any_element()
            }
            Some(avatar) => avatar
                .when_some(self.size, |this, size| this.size(size))
                .border_color(border_color)
                .into_any_element(),
        }
    }

    pub fn avatar(&'a self, window: &mut Window, cx: &mut App) -> Option<Avatar> {
        // Bail early if the email isn't available yet. Without it,
        // the GitHub provider skips the fast CDN path and falls back
        // to an unauthenticated per-commit API call that is slow and
        // rate-limited. Worse, a failed lookup gets permanently
        // cached under the key (sha, host) — so even when the email
        // arrives on a later render, the cached None shadows the
        // fast path forever.
        self.author_email.as_ref()?;

        let remote = self
            .remote
            .filter(|remote| remote.host_supports_avatars())?;
        let avatar_url =
            CommitAvatarAsset::new(remote.clone(), self.sha.clone(), self.author_email.clone());

        let url = window.use_asset::<CommitAvatarAsset>(&avatar_url, cx)??;
        Some(Avatar::new(url.to_string()))
    }
}

#[derive(Clone, Debug)]
struct CommitAvatarAsset {
    sha: SharedString,
    author_email: Option<SharedString>,
    remote: GitRemote,
}

impl Hash for CommitAvatarAsset {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sha.hash(state);
        self.remote.host.name().hash(state);
    }
}

impl CommitAvatarAsset {
    fn new(remote: GitRemote, sha: SharedString, author_email: Option<SharedString>) -> Self {
        Self {
            remote,
            sha,
            author_email,
        }
    }
}

impl Asset for CommitAvatarAsset {
    type Source = Self;
    type Output = Option<SharedString>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let client = cx.http_client();

        async move {
            source
                .remote
                .avatar_url(source.sha, source.author_email, client)
                .await
                .map(|url| SharedString::from(url.to_string()))
        }
    }
}

const MAX_LINE_CHANGE_ROWS: usize = 6;

fn shown_whitespace_offsets(
    line: &str,
    setting: language::language_settings::ShowWhitespaceSetting,
) -> std::collections::HashSet<usize> {
    use language::language_settings::ShowWhitespaceSetting;

    let whitespace: Vec<(usize, char)> = line
        .char_indices()
        .filter(|(_, character)| matches!(character, ' ' | '\t'))
        .collect();
    let is_whitespace_at =
        |offset: usize| matches!(line.as_bytes().get(offset), Some(b' ' | b'\t'));
    match setting {
        ShowWhitespaceSetting::None | ShowWhitespaceSetting::Selection => Default::default(),
        ShowWhitespaceSetting::All => whitespace.into_iter().map(|(offset, _)| offset).collect(),
        ShowWhitespaceSetting::Trailing => {
            let trimmed_len = line.trim_end_matches([' ', '\t']).len();
            whitespace
                .into_iter()
                .map(|(offset, _)| offset)
                .filter(|offset| *offset >= trimmed_len)
                .collect()
        }
        ShowWhitespaceSetting::Boundary => whitespace
            .into_iter()
            .filter(|(offset, character)| {
                *character == '\t'
                    || *offset == 0
                    || *offset + 1 == line.len()
                    || (*offset > 0 && is_whitespace_at(*offset - 1))
                    || is_whitespace_at(*offset + 1)
            })
            .map(|(offset, _)| offset)
            .collect(),
    }
}

#[derive(Default, Clone)]
pub struct LineChange {
    removed: Vec<SharedString>,
    added: Vec<SharedString>,
}

pub struct LineChangeState {
    change: Option<LineChange>,
    loaded: bool,
    scroll_handle: ScrollHandle,
    _task: Task<()>,
}

impl LineChangeState {
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn new(
        repository: &Entity<Repository>,
        sha: String,
        filename: String,
        line_start: u32,
        line_count: u32,
        cx: &mut Context<Self>,
    ) -> Self {
        let receiver =
            repository.update(cx, |repository, _| repository.load_commit_diff(sha, true));
        let task = cx.spawn(async move |this, cx| {
            let change = async {
                let Some(diff) = receiver.await.ok().and_then(|result| result.log_err()) else {
                    return None;
                };
                let Some(file) = diff
                    .files
                    .into_iter()
                    .find(|file| file.path.as_unix_str() == filename)
                else {
                    return None;
                };
                if file.is_binary {
                    return None;
                }
                let old_text = file.old_text.unwrap_or_default();
                let new_text = file.new_text.unwrap_or_default();
                let old_lines: Vec<&str> = old_text.lines().collect();
                let new_lines: Vec<&str> = new_text.lines().collect();
                let target_start = line_start.saturating_sub(1);
                let target_end = target_start + line_count.max(1);

                let mut change = LineChange::default();
                for (old_rows, new_rows) in language::line_diff(&old_text, &new_text) {
                    if new_rows.is_empty()
                        || new_rows.start >= target_end
                        || new_rows.end <= target_start
                    {
                        continue;
                    }
                    let overlapping_rows =
                        new_rows.start.max(target_start)..new_rows.end.min(target_end);
                    let is_one_to_one = old_rows.len() == new_rows.len();
                    if !is_one_to_one {
                        for row in old_rows.clone() {
                            if let Some(line) = old_lines.get(row as usize) {
                                change.removed.push(SharedString::from(line.to_string()));
                            }
                        }
                    }
                    for row in overlapping_rows {
                        if is_one_to_one
                            && let Some(line) =
                                old_lines.get((old_rows.start + row - new_rows.start) as usize)
                        {
                            change.removed.push(SharedString::from(line.to_string()));
                        }
                        if let Some(line) = new_lines.get(row as usize) {
                            change.added.push(SharedString::from(line.to_string()));
                        }
                    }
                }
                if change.removed.is_empty() && change.added.is_empty() {
                    return None;
                }
                Some(change)
            }
            .await;
            this.update(cx, |this, cx| {
                this.change = change;
                this.loaded = true;
                cx.notify();
            })
            .log_err();
        });
        Self {
            change: None,
            loaded: false,
            scroll_handle: ScrollHandle::new(),
            _task: task,
        }
    }

    pub fn render(
        state: &Entity<Self>,
        language_settings: &language::language_settings::LanguageSettings,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<impl IntoElement + use<>> {
        let (change, scroll_handle) = {
            let state = state.read(cx);
            (state.change.clone()?, state.scroll_handle.clone())
        };
        let change = &change;
        let status = cx.theme().status();
        let removed_background = status.deleted_background;
        let added_background = status.created_background;
        let colors = cx.theme().colors();
        let invisible_color = colors.text_muted;
        let whitespace_setting = language_settings.show_whitespaces;
        let whitespace_map = language_settings.whitespace_map.clone();
        let removed_word_background = colors.version_control_word_deleted;
        let added_word_background = colors.version_control_word_added;
        let theme_settings = ThemeSettings::get_global(cx);
        let buffer_font = theme_settings.buffer_font.clone();

        let mut removed_highlights = vec![Vec::new(); change.removed.len()];
        let mut added_highlights = vec![Vec::new(); change.added.len()];
        let mut used_removed = vec![false; change.removed.len()];
        for (added_index, added) in change.added.iter().enumerate() {
            let mut best: Option<(usize, usize, Vec<_>, Vec<_>)> = None;
            for (removed_index, removed) in change.removed.iter().enumerate() {
                if used_removed[removed_index] || removed.trim().is_empty() {
                    continue;
                }
                let (removed_ranges, added_ranges) =
                    word_diff_ranges(removed, added, DiffOptions::default());
                let changed: usize = removed_ranges
                    .iter()
                    .map(|range| range.len())
                    .sum::<usize>()
                    + added_ranges.iter().map(|range| range.len()).sum::<usize>();
                if best.as_ref().is_none_or(|(_, cost, _, _)| changed < *cost) {
                    best = Some((removed_index, changed, removed_ranges, added_ranges));
                }
            }
            if let Some((removed_index, _, removed_ranges, added_ranges)) = best {
                used_removed[removed_index] = true;
                removed_highlights[removed_index] = removed_ranges;
                added_highlights[added_index] = added_ranges;
            } else {
                added_highlights[added_index] = vec![0..added.len()];
            }
        }
        if change.added.is_empty() {
            for (index, removed) in change.removed.iter().enumerate() {
                used_removed[index] = true;
                removed_highlights[index] = vec![0..removed.len()];
            }
        }

        let font_size = rems(0.75).to_pixels(window.rem_size());
        let cell_width = {
            let text_system = window.text_system();
            let font_id = text_system.resolve_font(&buffer_font);
            text_system
                .em_advance(font_id, font_size)
                .unwrap_or(font_size * 0.6)
        };
        let line_height = font_size * 1.5;
        let tab_size = language_settings.tab_size.get();

        let row = |prefix: &'static str,
                   line: &SharedString,
                   background,
                   word_background,
                   ranges: &[std::ops::Range<usize>]| {
            let shown = shown_whitespace_offsets(line, whitespace_setting);
            let in_word = |offset: usize| ranges.iter().any(|range| range.contains(&offset));
            let mut segments: Vec<AnyElement> = Vec::new();
            let mut text = String::new();
            let mut text_in_word = false;
            let flush = |segments: &mut Vec<AnyElement>, text: &mut String, text_in_word: bool| {
                if text.is_empty() {
                    return;
                }
                let content = SharedString::from(std::mem::take(text));
                segments.push(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .when(text_in_word, |this| this.bg(word_background))
                        .child(content)
                        .into_any_element(),
                );
            };
            for (offset, character) in line.char_indices() {
                let marked = in_word(offset);
                let symbol = match character {
                    ' ' if shown.contains(&offset) => Some((whitespace_map.space.clone(), 1)),
                    '\t' if shown.contains(&offset) => Some((whitespace_map.tab.clone(), tab_size)),
                    _ => None,
                };
                match symbol {
                    Some((symbol, cells)) => {
                        flush(&mut segments, &mut text, text_in_word);
                        segments.push(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .self_stretch()
                                .w(cell_width * cells as f32)
                                .text_size(font_size / 2.)
                                .line_height(line_height)
                                .text_color(invisible_color)
                                .when(marked, |this| this.bg(word_background))
                                .child(SharedString::from(symbol.to_string()))
                                .into_any_element(),
                        );
                    }
                    None => {
                        if marked != text_in_word {
                            flush(&mut segments, &mut text, text_in_word);
                        }
                        text_in_word = marked;
                        text.push(character);
                    }
                }
            }
            flush(&mut segments, &mut text, text_in_word);
            h_flex()
                .px_1()
                .gap_1()
                .bg(background)
                .font(buffer_font.clone())
                .text_xs()
                .line_height(line_height)
                .child(div().flex_none().child(prefix))
                .child(h_flex().flex_none().children(segments))
        };
        Some(
            h_flex()
                .id("line-change-scroll")
                .items_start()
                .w_full()
                .py_1()
                .custom_scrollbars(
                    Scrollbars::new(ScrollAxes::Horizontal)
                        .id(("line-change-scrollbar", state.entity_id()))
                        .tracked_scroll_handle(&scroll_handle),
                    window,
                    cx,
                )
                .overflow_x_scroll()
                .child(
                    v_flex()
                        .min_w_full()
                        .flex_none()
                        .children(
                            change
                                .removed
                                .iter()
                                .zip(&removed_highlights)
                                .zip(&used_removed)
                                .filter(|(_, used)| **used)
                                .take(MAX_LINE_CHANGE_ROWS)
                                .map(|((line, ranges), _)| {
                                    row(
                                        "-",
                                        line,
                                        removed_background,
                                        removed_word_background,
                                        ranges,
                                    )
                                }),
                        )
                        .children(
                            change
                                .added
                                .iter()
                                .zip(&added_highlights)
                                .take(MAX_LINE_CHANGE_ROWS)
                                .map(|(line, ranges)| {
                                    row("+", line, added_background, added_word_background, ranges)
                                }),
                        ),
                ),
        )
    }
}

pub struct CommitTooltip {
    commit: CommitDetails,
    scroll_handle: ScrollHandle,
    markdown: Entity<Markdown>,
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    line_change: Option<Entity<LineChangeState>>,
}

impl CommitTooltip {
    pub fn blame_entry(
        blame: &BlameEntry,
        details: Option<ParsedCommitMessage>,
        tag_names: Vec<SharedString>,
        repository: Entity<Repository>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let commit_time = blame
            .committer_time
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
            .unwrap_or(OffsetDateTime::now_utc());

        let filename = blame.filename.clone();
        let line_start = blame.original_line_number;
        let line_count = blame.range.end.saturating_sub(blame.range.start);

        let mut this = Self::new(
            CommitDetails {
                sha: blame.sha.to_string().into(),
                commit_time,
                author_name: blame
                    .author
                    .clone()
                    .unwrap_or(t!("<no name>").to_string())
                    .into(),
                author_email: blame.author_mail.clone().unwrap_or("".to_string()).into(),
                message: details,
                tag_names,
                boundary: blame.boundary,
            },
            repository,
            workspace,
            cx,
        );
        let sha = this.commit.sha.to_string();
        let repository = this.repository.clone();
        this.line_change = Some(cx.new(|cx| {
            LineChangeState::new(&repository, sha, filename, line_start, line_count, cx)
        }));
        this
    }

    pub fn new(
        commit: CommitDetails,
        repository: Entity<Repository>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let markdown = cx.new(|cx| {
            Markdown::new(
                commit
                    .message
                    .as_ref()
                    .map(|message| message.message.clone())
                    .unwrap_or_default(),
                None,
                None,
                cx,
            )
        });
        Self {
            commit,
            repository,
            workspace,
            scroll_handle: ScrollHandle::new(),
            markdown,
            line_change: None,
        }
    }
}

impl Render for CommitTooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let avatar = CommitAvatar::from_commit_details(&self.commit).render(window, cx);

        let author = self.commit.author_name.clone();

        let author_email = self.commit.author_email.clone();

        let short_commit_id = self
            .commit
            .sha
            .get(0..git::SHORT_SHA_LENGTH)
            .map(|sha| sha.to_string().into())
            .unwrap_or_else(|| self.commit.sha.clone());
        let full_sha = self.commit.sha.to_string();
        let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        let absolute_timestamp = time_format::format_localized_timestamp(
            self.commit.commit_time,
            OffsetDateTime::now_utc(),
            local_offset,
            time_format::TimestampFormat::MediumAbsolute,
        );
        let markdown_style = {
            let style = hover_markdown_style(window, cx);
            style
        };

        let message = self
            .commit
            .message
            .as_ref()
            .map(|_| {
                MarkdownElement::new(self.markdown.clone(), markdown_style)
                    .scroll_handle(self.scroll_handle.clone())
                    .into_any()
            })
            .unwrap_or(t!("<no commit message>").into_any());

        let pull_request = self
            .commit
            .message
            .as_ref()
            .and_then(|details| details.pull_request.clone());
        let tag_names = self.commit.tag_names.clone();

        let ui_font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
        let message_max_height = window.line_height() * 12 + (ui_font_size / 0.4);
        let repo = self.repository.clone();
        let workspace = self.workspace.clone();
        let commit_summary = CommitSummary {
            sha: self.commit.sha.clone(),
            subject: self
                .commit
                .message
                .as_ref()
                .map_or(Default::default(), |message| {
                    message
                        .message
                        .split('\n')
                        .next()
                        .unwrap()
                        .trim_end()
                        .to_string()
                        .into()
                }),
            commit_timestamp: self.commit.commit_time.unix_timestamp(),
            author_name: self.commit.author_name.clone(),
            has_parent: false,
        };
        let boundary_notice = self
            .commit
            .boundary
            .then(|| {
                shallow_boundary_notice(self.repository.clone(), self.workspace.clone(), window, cx)
            })
            .flatten();

        let language_settings =
            language::language_settings::LanguageSettings::resolve(None, None, cx);
        let line_change = self
            .line_change
            .as_ref()
            .and_then(|state| LineChangeState::render(state, &language_settings, window, cx));

        tooltip_container(cx, move |this, cx| {
            this.occlude()
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    v_flex()
                        .w(gpui::rems(30.))
                        .child(
                            h_flex()
                                .pb_1()
                                .gap_2()
                                .overflow_x_hidden()
                                .flex_wrap()
                                .child(avatar)
                                .child(author)
                                .when(!author_email.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .text_color(cx.theme().colors().text_muted)
                                            .child(author_email),
                                    )
                                })
                                .border_b_1()
                                .border_color(cx.theme().colors().border_variant),
                        )
                        .children(boundary_notice)
                        .child(
                            div()
                                .id("inline-blame-commit-message")
                                .track_scroll(&self.scroll_handle)
                                .py_1p5()
                                .max_h(message_max_height)
                                .overflow_y_scroll()
                                .child(message),
                        )
                        .children(line_change)
                        .child(
                            h_flex()
                                .text_color(cx.theme().colors().text_muted)
                                .w_full()
                                .justify_between()
                                .pt_1()
                                .gap_1()
                                .flex_wrap()
                                .border_t_1()
                                .border_color(cx.theme().colors().border_variant)
                                .child(absolute_timestamp)
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .min_w_0()
                                        .children(commit_tag_chips(&tag_names))
                                        .when_some(pull_request, |this, pr| {
                                            this.child(
                                                Button::new(
                                                    "pull-request-button",
                                                    format!("#{}", pr.number),
                                                )
                                                .color(Color::Muted)
                                                .start_icon(
                                                    Icon::new(IconName::PullRequest)
                                                        .size(IconSize::Small)
                                                        .color(Color::Muted),
                                                )
                                                .on_click(move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    cx.open_url(pr.url.as_str())
                                                }),
                                            )
                                            .child(Divider::vertical())
                                        })
                                        .child(
                                            Button::new(
                                                "commit-sha-button",
                                                short_commit_id.clone(),
                                            )
                                            .color(Color::Muted)
                                            .start_icon(
                                                Icon::new(IconName::FileGit)
                                                    .size(IconSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .on_click(
                                                move |_, window, cx| {
                                                    CommitView::open(
                                                        commit_summary.sha.to_string(),
                                                        repo.downgrade(),
                                                        workspace.clone(),
                                                        None,
                                                        None,
                                                        window,
                                                        cx,
                                                    );
                                                    cx.stop_propagation();
                                                },
                                            ),
                                        )
                                        .child(Divider::vertical())
                                        .child(
                                            CopyButton::new("copy-commit-sha", full_sha)
                                                .tooltip_label(t!("Copy SHA")),
                                        ),
                                ),
                        ),
                )
        })
    }
}

fn blame_entry_timestamp(blame_entry: &BlameEntry, format: time_format::TimestampFormat) -> String {
    match blame_entry.author_offset_date_time() {
        Ok(timestamp) => {
            let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
            time_format::format_localized_timestamp(
                timestamp,
                time::OffsetDateTime::now_utc(),
                local_offset,
                format,
            )
        }
        Err(_) => t!("Error parsing date").to_string(),
    }
}

pub fn blame_entry_relative_timestamp(blame_entry: &BlameEntry) -> String {
    blame_entry_timestamp(blame_entry, time_format::TimestampFormat::Relative)
}

pub(crate) fn shallow_boundary_notice(
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    window: &Window,
    cx: &App,
) -> Option<impl IntoElement + use<>> {
    let unshallow_state = repository.read(cx).unshallow_state();
    if unshallow_state == UnshallowState::Unshallowed {
        return None;
    }
    let in_flight = unshallow_state == UnshallowState::InProgress;
    let avatar_width = CommitAvatar::rendered_size(rems(1.), window);
    let can_fetch = workspace.upgrade().is_some();
    Some(
        v_flex()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        h_flex().w(avatar_width).justify_center().child(
                            Icon::new(IconName::Warning)
                                .size(IconSize::Small)
                                .color(Color::Warning),
                        ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Label::new(t!(
                                "Shallow clone boundary: earlier history is missing, so these lines may come from an older commit."
                            ))
                            .size(LabelSize::Small)
                            .line_height_style(LineHeightStyle::UiLabel),
                        ),
                    ),
            )
            .when(can_fetch, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(div().w(avatar_width))
                        .child(
                            Button::new(
                                "fetch-unshallow",
                                if in_flight {
                                    t!("Fetching…")
                                } else {
                                    t!("Fetch Missing History")
                                },
                            )
                            .style(ButtonStyle::Outlined)
                            .label_size(LabelSize::Small)
                            .disabled(in_flight)
                            .tooltip(Tooltip::text(t!(
                                "Run `git fetch --unshallow` to download the full history"
                            )))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                fetch_unshallow(
                                    repository.clone(),
                                    workspace.clone(),
                                    window,
                                    cx,
                                )
                                .detach_and_log_err(cx);
                            }),
                        ),
                )
            }),
    )
}

pub(crate) fn fetch_unshallow(
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<()>> {
    if repository.read(cx).unshallow_state() != UnshallowState::Idle {
        return Task::ready(Ok(()));
    }
    let askpass = {
        let workspace = workspace.clone();
        let window_handle = window.window_handle();
        AskPassDelegate::new_with_cancellation(
            &mut cx.to_async(),
            move |prompt, tx, cancellation, cx| {
                window_handle
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.toggle_modal(window, cx, |window, cx| {
                                    AskPassModal::new(
                                        "git fetch --unshallow".into(),
                                        prompt.into(),
                                        tx,
                                        cancellation,
                                        window,
                                        cx,
                                    )
                                });
                            })
                            .ok();
                    })
                    .ok();
            },
        )
    };
    let fetch = repository.update(cx, |repository, cx| repository.fetch_unshallow(askpass, cx));
    window.refresh();
    window.spawn(cx, async move |cx| {
        let result = match fetch.await {
            Ok(result) => result,
            Err(canceled) => Err(anyhow::Error::from(canceled)),
        };
        cx.update(|window, cx| {
            window.refresh();
            let Some(workspace) = workspace.upgrade() else {
                return Ok(());
            };
            match result {
                Ok(_) => {
                    workspace.update(cx, |workspace, cx| {
                        let toast = StatusToast::new(
                            t!("Fetched the missing commit history"),
                            cx,
                            |this, _| {
                                this.icon(
                                    Icon::new(IconName::GitBranch)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                            },
                        );
                        workspace.toggle_status_toast(toast, cx);
                    });
                    Ok(())
                }
                Err(error) => {
                    show_error_toast(workspace, "fetch --unshallow", error, cx);
                    Err(anyhow::anyhow!("git fetch --unshallow failed"))
                }
            }
        })?
    })
}
