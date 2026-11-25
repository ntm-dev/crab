use crate::{git_panel::show_error_toast, picker_prompt};
use editor::Editor;
use futures::channel::oneshot;
use gpui::{
    App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Task,
    WeakEntity,
};
use i18n::t;
use project::git_store::Repository;
use ui::prelude::*;
use workspace::{ModalView, Workspace};

struct TextPrompt {
    input: Entity<Editor>,
    title: SharedString,
    sender: Option<oneshot::Sender<String>>,
    focus_handle: FocusHandle,
}

impl TextPrompt {
    fn new(
        title: SharedString,
        placeholder: SharedString,
        initial: String,
        sender: oneshot::Sender<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(&placeholder, window, cx);
            editor.set_text(initial, window, cx);
            editor.select_all(&editor::actions::SelectAll, window, cx);
            editor
        });
        let focus_handle = input.focus_handle(cx);
        window.focus(&focus_handle, cx);
        Self {
            input,
            title,
            sender: Some(sender),
            focus_handle,
        }
    }
}

impl Focusable for TextPrompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .elevation_3(cx)
            .w(rems(34.))
            .flex_1()
            .overflow_hidden()
            .child(
                div()
                    .w_full()
                    .px_2()
                    .pt_2()
                    .child(Label::new(self.title.clone()).color(Color::Muted)),
            )
            .child(div().w_full().p_2().child(self.input.clone()))
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| {
                cx.emit(DismissEvent);
            }))
            .on_action(cx.listener(|this, _: &menu::Confirm, _, cx| {
                let text = this.input.read(cx).text(cx).trim().to_string();
                if text.is_empty() {
                    return;
                }
                if let Some(sender) = this.sender.take() {
                    sender.send(text).ok();
                }
                cx.emit(DismissEvent);
            }))
    }
}

impl EventEmitter<DismissEvent> for TextPrompt {}

impl ModalView for TextPrompt {}

fn text_prompt(
    title: SharedString,
    placeholder: SharedString,
    initial: String,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<String>> {
    window.spawn(cx, async move |cx| {
        let (tx, rx) = oneshot::channel();
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    TextPrompt::new(title, placeholder, initial, tx, window, cx)
                })
            })
            .ok();
        rx.await.ok()
    })
}

fn active_repository(workspace: &Workspace, cx: &App) -> Option<Entity<Repository>> {
    workspace.project().read(cx).active_repository(cx)
}

fn report(
    workspace: &WeakEntity<Workspace>,
    action: &'static str,
    result: anyhow::Result<()>,
    cx: &mut gpui::AsyncWindowContext,
) {
    if let Err(error) = result {
        if let Some(workspace) = workspace.upgrade() {
            cx.update(|_, cx| show_error_toast(workspace, action, error, cx))
                .ok();
        }
    }
}

fn create_remote(
    workspace: &mut Workspace,
    _: &crab_actions::git::CreateRemote,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repo) = active_repository(workspace, cx) else {
        return;
    };
    let weak = workspace.weak_handle();
    window
        .spawn(cx, async move |cx| {
            let url = cx
                .update(|window, cx| {
                    text_prompt(
                        t!("Remote URL"),
                        t!("Enter the remote repository URL…"),
                        String::new(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let name = cx
                .update(|window, cx| {
                    text_prompt(
                        t!("Remote name"),
                        t!("Enter a name for this remote…"),
                        "origin".to_string(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let result = repo
                .update(cx, |repo, _| repo.create_remote(name, url))
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            report(&weak, "remote add", result, cx);
            Some(())
        })
        .detach();
}

fn remove_remote(
    workspace: &mut Workspace,
    _: &crab_actions::git::RemoveRemote,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repo) = active_repository(workspace, cx) else {
        return;
    };
    let weak = workspace.weak_handle();
    window
        .spawn(cx, async move |cx| {
            let remotes = repo
                .update(cx, |repo, _| repo.get_remotes(None, false))
                .await
                .ok()?
                .ok()?;
            let names: Vec<SharedString> = remotes.into_iter().map(|r| r.name).collect();
            if names.is_empty() {
                return None;
            }
            let selected = cx
                .update(|window, cx| {
                    picker_prompt::prompt(
                        &t!("Pick a remote to remove"),
                        names.clone(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let name = names.get(selected)?.to_string();
            let result = repo
                .update(cx, |repo, _| repo.remove_remote(name))
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            report(&weak, "remote remove", result, cx);
            Some(())
        })
        .detach();
}

fn delete_branch(
    workspace: &mut Workspace,
    _: &crab_actions::git::DeleteBranch,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repo) = active_repository(workspace, cx) else {
        return;
    };
    let weak = workspace.weak_handle();
    window
        .spawn(cx, async move |cx| {
            let scan = repo
                .update(cx, |repo, _| repo.branches())
                .await
                .ok()?
                .ok()?;
            let names: Vec<SharedString> = scan
                .branches
                .iter()
                .filter(|branch| !branch.is_head && branch.ref_name.starts_with("refs/heads/"))
                .map(|branch| SharedString::from(branch.name().to_string()))
                .collect();
            if names.is_empty() {
                return None;
            }
            let selected = cx
                .update(|window, cx| {
                    picker_prompt::prompt(
                        &t!("Pick a branch to delete"),
                        names.clone(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let name = names.get(selected)?.to_string();
            let result = repo
                .update(cx, |repo, _| repo.delete_branch(false, name, false))
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            report(&weak, "branch delete", result, cx);
            Some(())
        })
        .detach();
}

fn create_tag(
    workspace: &mut Workspace,
    _: &crab_actions::git::CreateTag,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repo) = active_repository(workspace, cx) else {
        return;
    };
    let weak = workspace.weak_handle();
    window
        .spawn(cx, async move |cx| {
            let name = cx
                .update(|window, cx| {
                    text_prompt(
                        t!("Create tag"),
                        t!("Tag name…"),
                        String::new(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let result = repo
                .update(cx, |repo, _| repo.create_tag(name, None))
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            report(&weak, "tag", result, cx);
            Some(())
        })
        .detach();
}

fn delete_tag(
    workspace: &mut Workspace,
    _: &crab_actions::git::DeleteTag,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repo) = active_repository(workspace, cx) else {
        return;
    };
    let weak = workspace.weak_handle();
    window
        .spawn(cx, async move |cx| {
            let tags = repo
                .update(cx, |repo, _| repo.list_tags())
                .await
                .ok()?
                .ok()?;
            let names: Vec<SharedString> = tags.into_iter().map(SharedString::from).collect();
            if names.is_empty() {
                return None;
            }
            let selected = cx
                .update(|window, cx| {
                    picker_prompt::prompt(
                        &t!("Pick a tag to delete"),
                        names.clone(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let name = names.get(selected)?.to_string();
            let result = repo
                .update(cx, |repo, _| repo.delete_tag(name))
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            report(&weak, "tag -d", result, cx);
            Some(())
        })
        .detach();
}

fn push_tags(
    workspace: &mut Workspace,
    _: &crab_actions::git::PushTags,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repo) = active_repository(workspace, cx) else {
        return;
    };
    let weak = workspace.weak_handle();
    window
        .spawn(cx, async move |cx| {
            let remotes = repo
                .update(cx, |repo, _| repo.get_remotes(None, true))
                .await
                .ok()?
                .ok()?;
            let names: Vec<SharedString> = remotes.into_iter().map(|r| r.name).collect();
            if names.is_empty() {
                return None;
            }
            let selected = cx
                .update(|window, cx| {
                    picker_prompt::prompt(
                        &t!("Pick a remote to push tags to"),
                        names.clone(),
                        weak.clone(),
                        window,
                        cx,
                    )
                })
                .ok()?
                .await?;
            let name = names.get(selected)?.to_string();
            let result = repo
                .update(cx, |repo, _| repo.push_tags(name))
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::anyhow!(error)),
            };
            report(&weak, "push --tags", result, cx);
            Some(())
        })
        .detach();
}

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(create_remote);
    workspace.register_action(remove_remote);
    workspace.register_action(delete_branch);
    workspace.register_action(create_tag);
    workspace.register_action(delete_tag);
    workspace.register_action(push_tags);
}
