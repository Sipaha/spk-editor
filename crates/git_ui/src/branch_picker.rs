use anyhow::Context as _;
use editor::Editor;
use fuzzy_nucleo::StringMatchCandidate;

use collections::HashSet;
use git::repository::Branch;
use gpui::http_client::Url;
use gpui::{
    Action, AnyElement, App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, MouseDownEvent, Modifiers, ModifiersChangedEvent,
    ParentElement, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, actions,
    rems, uniform_list,
};
use menu::{Cancel, Confirm};
use picker::{Picker, PickerDelegate, PickerEditorPosition};
use project::git_store::{Repository, RepositoryEvent};
use project::project_settings::ProjectSettings;
use settings::Settings;
use std::path::Path;
use std::sync::Arc;
use time::OffsetDateTime;
use ui::{
    Divider, Headline, HeadlineSize, HighlightedLabel, KeyBinding, ListItem, ListItemSpacing,
    Tooltip, prelude::*,
};
use ui_input::ErasedEditor;
use util::ResultExt;
use workspace::notifications::DetachAndPromptErr;
use workspace::{ModalView, Workspace};

pub mod context_menu;
pub mod favorites;
pub mod tabs;
pub mod tree;

use crate::{branch_picker, git_panel::show_error_toast};

actions!(
    branch_picker,
    [
        /// Deletes the selected git branch or remote.
        DeleteBranch,
        /// Filter the list of remotes
        FilterRemotes
    ]
);

pub fn checkout_branch(
    workspace: &mut Workspace,
    _: &zed_actions::git::CheckoutBranch,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open(workspace, &zed_actions::git::Branch, window, cx);
}

pub fn switch(
    workspace: &mut Workspace,
    _: &zed_actions::git::Switch,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open(workspace, &zed_actions::git::Branch, window, cx);
}

pub fn open(
    workspace: &mut Workspace,
    _: &zed_actions::git::Branch,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = workspace.weak_handle();
    let repository = workspace.project().read(cx).active_repository(cx);

    workspace.toggle_modal(window, cx, |window, cx| {
        BranchList::new(
            workspace_handle,
            repository,
            BranchListStyle::Modal,
            rems(34.),
            window,
            cx,
        )
    })
}

pub fn popover(
    workspace: WeakEntity<Workspace>,
    modal_style: bool,
    repository: Option<Entity<Repository>>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<BranchList> {
    let (style, width) = if modal_style {
        (BranchListStyle::Modal, rems(34.))
    } else {
        (BranchListStyle::Popover, rems(20.))
    };

    cx.new(|cx| {
        let list = BranchList::new(workspace, repository, style, width, window, cx);
        list.focus_handle(cx).focus(window, cx);
        list
    })
}

pub fn create_embedded(
    workspace: WeakEntity<Workspace>,
    repository: Option<Entity<Repository>>,
    width: Rems,
    show_footer: bool,
    window: &mut Window,
    cx: &mut Context<BranchList>,
) -> BranchList {
    BranchList::new_embedded(workspace, repository, width, show_footer, window, cx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BranchListStyle {
    Modal,
    Popover,
}

pub struct BranchList {
    width: Rems,
    pub picker: Entity<Picker<BranchListDelegate>>,
    picker_focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    embedded: bool,
}

impl BranchList {
    fn new(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        style: BranchListStyle,
        width: Rems,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new_inner(workspace, repository, style, width, false, window, cx);
        this._subscriptions
            .push(cx.subscribe(&this.picker, |_, _, _, cx| {
                cx.emit(DismissEvent);
            }));
        this
    }

    fn new_inner(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        style: BranchListStyle,
        width: Rems,
        embedded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let all_branches = repository
            .as_ref()
            .map(|repo| process_branches(&repo.read(cx).branch_list))
            .unwrap_or_default();

        let default_branch_request = repository.clone().map(|repository| {
            repository.update(cx, |repository, _| repository.default_branch(false))
        });

        let mut delegate = BranchListDelegate::new(workspace, repository.clone(), style, cx);
        delegate.all_branches = all_branches;

        let picker = cx.new(|cx| {
            Picker::uniform_list(delegate, window, cx)
                .show_scrollbar(true)
                .modal(!embedded)
        });
        let picker_focus_handle = picker.focus_handle(cx);

        picker.update(cx, |picker, _| {
            picker.delegate.focus_handle = picker_focus_handle.clone();
            picker.delegate.show_footer = !embedded;
        });

        let mut subscriptions = Vec::new();

        if let Some(repo) = &repository {
            subscriptions.push(cx.subscribe_in(
                repo,
                window,
                move |this, repo, event, window, cx| {
                    if matches!(event, RepositoryEvent::BranchListChanged) {
                        let branch_list = repo.read(cx).branch_list.clone();
                        let all_branches = process_branches(&branch_list);
                        this.picker.update(cx, |picker, cx| {
                            picker.delegate.restore_selected_branch = picker
                                .delegate
                                .matches
                                .get(picker.delegate.selected_index)
                                .and_then(|entry| entry.as_branch().map(|b| b.ref_name.clone()));
                            picker.delegate.all_branches = all_branches;
                            picker.refresh(window, cx);
                        });
                    }
                },
            ));
        }

        // Fetch default branch asynchronously since it requires a git operation
        cx.spawn_in(window, async move |this, cx| {
            let default_branch = default_branch_request
                .context("No active repository")?
                .await
                .map(Result::ok)
                .ok()
                .flatten()
                .flatten();

            let _ = this.update_in(cx, |this, _window, cx| {
                this.picker.update(cx, |picker, _cx| {
                    picker.delegate.default_branch = default_branch;
                });
            });

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);

        Self {
            picker,
            picker_focus_handle,
            width,
            _subscriptions: subscriptions,
            embedded,
        }
    }

    fn new_embedded(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        width: Rems,
        show_footer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new_inner(
            workspace,
            repository,
            BranchListStyle::Modal,
            width,
            true,
            window,
            cx,
        );
        this.picker.update(cx, |picker, _| {
            picker.delegate.show_footer = show_footer;
        });
        this._subscriptions
            .push(cx.subscribe(&this.picker, |_, _, _, cx| {
                cx.emit(DismissEvent);
            }));
        this
    }

    pub fn handle_modifiers_changed(
        &mut self,
        ev: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker
            .update(cx, |picker, _| picker.delegate.modifiers = ev.modifiers)
    }

    pub fn handle_delete(
        &mut self,
        _: &branch_picker::DeleteBranch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker.update(cx, |picker, cx| {
            picker
                .delegate
                .delete_at(picker.delegate.selected_index, window, cx)
        })
    }

    pub fn handle_filter(
        &mut self,
        _: &branch_picker::FilterRemotes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker.update(cx, |picker, cx| {
            picker.delegate.branch_filter = picker.delegate.branch_filter.invert();
            picker.update_matches(picker.query(cx), window, cx);
            picker.refresh_placeholder(window, cx);
            cx.notify();
        });
    }
}
impl ModalView for BranchList {}
impl EventEmitter<DismissEvent> for BranchList {}

impl Focusable for BranchList {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.picker_focus_handle.clone()
    }
}

impl Render for BranchList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("GitBranchSelector")
            .w(self.width)
            .on_modifiers_changed(cx.listener(Self::handle_modifiers_changed))
            .on_action(cx.listener(Self::handle_delete))
            .on_action(cx.listener(Self::handle_filter))
            .child(self.picker.clone())
            .when(!self.embedded, |this| {
                this.on_mouse_down_out({
                    cx.listener(move |this, _, window, cx| {
                        this.picker.update(cx, |this, cx| {
                            this.cancel(&Default::default(), window, cx);
                        })
                    })
                })
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Branch {
        branch: Branch,
        positions: Vec<usize>,
    },
    NewUrl {
        url: String,
    },
    NewBranch {
        name: String,
    },
    NewRemoteName {
        name: String,
        url: SharedString,
    },
}

impl Entry {
    fn as_branch(&self) -> Option<&Branch> {
        match self {
            Entry::Branch { branch, .. } => Some(branch),
            _ => None,
        }
    }

    fn name(&self) -> &str {
        match self {
            Entry::Branch { branch, .. } => branch.name(),
            Entry::NewUrl { url, .. } => url.as_str(),
            Entry::NewBranch { name, .. } => name.as_str(),
            Entry::NewRemoteName { name, .. } => name.as_str(),
        }
    }

    #[cfg(test)]
    fn is_new_url(&self) -> bool {
        matches!(self, Self::NewUrl { .. })
    }

    #[cfg(test)]
    fn is_new_branch(&self) -> bool {
        matches!(self, Self::NewBranch { .. })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum BranchFilter {
    /// Show both local and remote branches.
    All,
    /// Only show remote branches.
    Remote,
}

impl BranchFilter {
    fn invert(&self) -> Self {
        match self {
            BranchFilter::All => BranchFilter::Remote,
            BranchFilter::Remote => BranchFilter::All,
        }
    }
}

pub struct BranchListDelegate {
    workspace: WeakEntity<Workspace>,
    matches: Vec<Entry>,
    all_branches: Vec<Branch>,
    default_branch: Option<SharedString>,
    repo: Option<Entity<Repository>>,
    style: BranchListStyle,
    selected_index: usize,
    last_query: String,
    modifiers: Modifiers,
    branch_filter: BranchFilter,
    state: PickerState,
    focus_handle: FocusHandle,
    restore_selected_branch: Option<SharedString>,
    show_footer: bool,
}

#[derive(Debug)]
enum PickerState {
    /// When we display list of branches/remotes
    List,
    /// When we set an url to create a new remote
    NewRemote,
    /// When we confirm the new remote url (after NewRemote)
    CreateRemote(SharedString),
    /// When we set a new branch to create
    NewBranch,
}

fn process_branches(branches: &Arc<[Branch]>) -> Vec<Branch> {
    let remote_upstreams: HashSet<_> = branches
        .iter()
        .filter_map(|branch| {
            branch
                .upstream
                .as_ref()
                .filter(|upstream| upstream.is_remote())
                .map(|upstream| upstream.ref_name.clone())
        })
        .collect();

    let mut result: Vec<Branch> = branches
        .iter()
        .filter(|branch| !remote_upstreams.contains(&branch.ref_name))
        .cloned()
        .collect();

    result.sort_by_key(|branch| {
        (
            !branch.is_head,
            branch
                .most_recent_commit
                .as_ref()
                .map(|commit| 0 - commit.commit_timestamp),
        )
    });

    result
}

impl BranchListDelegate {
    fn new(
        workspace: WeakEntity<Workspace>,
        repo: Option<Entity<Repository>>,
        style: BranchListStyle,
        cx: &mut Context<BranchList>,
    ) -> Self {
        Self {
            workspace,
            matches: vec![],
            repo,
            style,
            all_branches: Vec::new(),
            default_branch: None,
            selected_index: 0,
            last_query: Default::default(),
            modifiers: Default::default(),
            branch_filter: BranchFilter::All,
            state: PickerState::List,
            focus_handle: cx.focus_handle(),
            restore_selected_branch: None,
            show_footer: false,
        }
    }

    fn create_branch(
        &self,
        from_branch: Option<SharedString>,
        new_branch_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let new_branch_name = new_branch_name.to_string().replace(' ', "-");
        let base_branch = from_branch.map(|b| b.to_string());
        cx.spawn(async move |_, cx| {
            repo.update(cx, |repo, _| {
                repo.create_branch(new_branch_name, base_branch)
            })
            .await??;

            Ok(())
        })
        .detach_and_prompt_err("Failed to create branch", window, cx, |e, _, _| {
            Some(e.to_string())
        });
        cx.emit(DismissEvent);
    }

    fn create_remote(
        &self,
        remote_name: String,
        remote_url: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };

        let receiver = repo.update(cx, |repo, _| repo.create_remote(remote_name, remote_url));

        cx.background_spawn(async move { receiver.await? })
            .detach_and_prompt_err("Failed to create remote", window, cx, |e, _, _cx| {
                Some(e.to_string())
            });
        cx.emit(DismissEvent);
    }

    fn delete_at(&self, idx: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self.matches.get(idx).cloned() else {
            return;
        };
        let Some(repo) = self.repo.clone() else {
            return;
        };

        let workspace = self.workspace.clone();

        cx.spawn_in(window, async move |picker, cx| {
            let is_remote;
            let result = match &entry {
                Entry::Branch { branch, .. } => {
                    if branch.is_head {
                        return Ok(());
                    }

                    is_remote = branch.is_remote();
                    repo.update(cx, |repo, _| {
                        repo.delete_branch(is_remote, branch.name().to_string())
                    })
                    .await?
                }
                _ => {
                    log::error!("Failed to delete entry: wrong entry to delete");
                    return Ok(());
                }
            };

            if let Err(e) = result {
                if is_remote {
                    log::error!("Failed to delete remote branch: {}", e);
                } else {
                    log::error!("Failed to delete branch: {}", e);
                }

                if let Some(workspace) = workspace.upgrade() {
                    cx.update(|_window, cx| {
                        if is_remote {
                            show_error_toast(
                                workspace,
                                format!("branch -dr {}", entry.name()),
                                e,
                                cx,
                            )
                        } else {
                            show_error_toast(
                                workspace,
                                format!("branch -d {}", entry.name()),
                                e,
                                cx,
                            )
                        }
                    })?;
                }

                return Ok(());
            }

            picker.update_in(cx, |picker, _, cx| {
                picker.delegate.matches.retain(|e| e != &entry);

                if let Entry::Branch { branch, .. } = &entry {
                    picker
                        .delegate
                        .all_branches
                        .retain(|e| e.ref_name != branch.ref_name);
                }

                if picker.delegate.matches.is_empty() {
                    picker.delegate.selected_index = 0;
                } else if picker.delegate.selected_index >= picker.delegate.matches.len() {
                    picker.delegate.selected_index = picker.delegate.matches.len() - 1;
                }

                cx.notify();
            })?;

            anyhow::Ok(())
        })
        .detach();
    }
}

impl PickerDelegate for BranchListDelegate {
    type ListItem = ListItem;

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        match self.state {
            PickerState::List | PickerState::NewRemote | PickerState::NewBranch => {
                match self.branch_filter {
                    BranchFilter::All | BranchFilter::Remote => "Switch branch…",
                }
            }
            PickerState::CreateRemote(_) => "Enter a name for this remote…",
        }
        .into()
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        match self.state {
            PickerState::CreateRemote(_) => {
                Some(SharedString::new_static("Remote name can't be empty"))
            }
            _ => None,
        }
    }

    fn render_editor(
        &self,
        editor: &Arc<dyn ErasedEditor>,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Div {
        let focus_handle = self.focus_handle.clone();
        let editor = editor.as_any().downcast_ref::<Entity<Editor>>().unwrap();

        let show_inline_filter =
            self.editor_position() == PickerEditorPosition::End || !self.show_footer;

        v_flex()
            .when(
                self.editor_position() == PickerEditorPosition::End,
                |this| this.child(Divider::horizontal()),
            )
            .child(
                h_flex()
                    .overflow_hidden()
                    .flex_none()
                    .h_9()
                    .px_2p5()
                    .child(editor.clone())
                    .when(show_inline_filter, |this| {
                        let tooltip_label = match self.branch_filter {
                            BranchFilter::All => "Filter Remote Branches",
                            BranchFilter::Remote => "Show All Branches",
                        };

                        this.gap_1().justify_between().child({
                            IconButton::new("filter-remotes", IconName::Filter)
                                .toggle_state(self.branch_filter == BranchFilter::Remote)
                                .icon_size(IconSize::Small)
                                .tooltip(move |_, cx| {
                                    Tooltip::for_action_in(
                                        tooltip_label,
                                        &branch_picker::FilterRemotes,
                                        &focus_handle,
                                        cx,
                                    )
                                })
                                .on_click(|_click, window, cx| {
                                    window.dispatch_action(
                                        branch_picker::FilterRemotes.boxed_clone(),
                                        cx,
                                    );
                                })
                        })
                    }),
            )
            .when(
                self.editor_position() == PickerEditorPosition::Start,
                |this| this.child(Divider::horizontal()),
            )
    }

    fn editor_position(&self) -> PickerEditorPosition {
        match self.style {
            BranchListStyle::Modal => PickerEditorPosition::Start,
            BranchListStyle::Popover => PickerEditorPosition::End,
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
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
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let all_branches = self.all_branches.clone();

        let branch_filter = self.branch_filter;
        cx.spawn_in(window, async move |picker, cx| {
            let branch_matches_filter = |branch: &Branch| match branch_filter {
                BranchFilter::All => true,
                BranchFilter::Remote => branch.is_remote(),
            };

            let mut matches: Vec<Entry> = if query.is_empty() {
                let mut matches: Vec<Entry> = all_branches
                    .into_iter()
                    .filter(|branch| branch_matches_filter(branch))
                    .map(|branch| Entry::Branch {
                        branch,
                        positions: Vec::new(),
                    })
                    .collect();

                // Keep the existing recency sort within each group, but show local branches first.
                matches.sort_by_key(|entry| entry.as_branch().is_some_and(|b| b.is_remote()));

                matches
            } else {
                let branches = all_branches
                    .iter()
                    .filter(|branch| branch_matches_filter(branch))
                    .collect::<Vec<_>>();
                let candidates = branches
                    .iter()
                    .enumerate()
                    .map(|(ix, branch)| StringMatchCandidate::new(ix, branch.name()))
                    .collect::<Vec<StringMatchCandidate>>();
                let mut matches: Vec<Entry> = fuzzy_nucleo::match_strings_async(
                    &candidates,
                    &query,
                    fuzzy_nucleo::Case::Smart,
                    fuzzy_nucleo::LengthPenalty::On,
                    10000,
                    &Default::default(),
                    cx.background_executor().clone(),
                )
                .await
                .into_iter()
                .map(|candidate| Entry::Branch {
                    branch: branches[candidate.candidate_id].clone(),
                    positions: candidate.positions,
                })
                .collect();

                // Keep fuzzy-relevance ordering within local/remote groups, but show locals first.
                matches.sort_by_key(|entry| entry.as_branch().is_some_and(|b| b.is_remote()));

                matches
            };
            picker
                .update(cx, |picker, _| {
                    if let PickerState::CreateRemote(url) = &picker.delegate.state {
                        let query = query.replace(' ', "-");
                        if !query.is_empty() {
                            picker.delegate.matches = vec![Entry::NewRemoteName {
                                name: query.clone(),
                                url: url.clone(),
                            }];
                            picker.delegate.selected_index = 0;
                        } else {
                            picker.delegate.matches = Vec::new();
                            picker.delegate.selected_index = 0;
                        }
                        picker.delegate.last_query = query;
                        return;
                    }

                    if !query.is_empty()
                        && !matches.first().is_some_and(|entry| entry.name() == query)
                    {
                        let query = query.replace(' ', "-");
                        let is_url = query.trim_start_matches("git@").parse::<Url>().is_ok();
                        let entry = if is_url {
                            Entry::NewUrl { url: query }
                        } else {
                            Entry::NewBranch { name: query }
                        };
                        // Only transition to NewBranch/NewRemote states when we only show their list item
                        // Otherwise, stay in List state so footer buttons remain visible
                        picker.delegate.state = if matches.is_empty() {
                            if is_url {
                                PickerState::NewRemote
                            } else {
                                PickerState::NewBranch
                            }
                        } else {
                            PickerState::List
                        };
                        matches.push(entry);
                    } else {
                        picker.delegate.state = PickerState::List;
                    }
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches;
                    if delegate.matches.is_empty() {
                        delegate.selected_index = 0;
                    } else if let Some(ref_name) = delegate.restore_selected_branch.take() {
                        delegate.selected_index = delegate
                            .matches
                            .iter()
                            .position(|entry| {
                                entry.as_branch().is_some_and(|b| b.ref_name == ref_name)
                            })
                            .unwrap_or(0);
                    } else {
                        delegate.selected_index =
                            core::cmp::min(delegate.selected_index, delegate.matches.len() - 1);
                    }
                    delegate.last_query = query;
                })
                .log_err();
        })
    }

    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self.matches.get(self.selected_index()) else {
            return;
        };

        match entry {
            Entry::Branch { branch, .. } => {
                let current_branch = self.repo.as_ref().map(|repo| {
                    repo.read_with(cx, |repo, _| {
                        repo.branch.as_ref().map(|branch| branch.ref_name.clone())
                    })
                });

                if current_branch
                    .flatten()
                    .is_some_and(|current_branch| current_branch == branch.ref_name)
                {
                    cx.emit(DismissEvent);
                    return;
                }

                let Some(repo) = self.repo.clone() else {
                    return;
                };

                let branch = branch.clone();
                cx.spawn(async move |_, cx| {
                    repo.update(cx, |repo, _| repo.change_branch(branch.name().to_string()))
                        .await??;

                    anyhow::Ok(())
                })
                .detach_and_prompt_err(
                    "Failed to change branch",
                    window,
                    cx,
                    |_, _, _| None,
                );
            }
            Entry::NewUrl { url } => {
                self.state = PickerState::CreateRemote(url.clone().into());
                self.matches = Vec::new();
                self.selected_index = 0;

                cx.defer_in(window, |picker, window, cx| {
                    picker.refresh_placeholder(window, cx);
                    picker.set_query("", window, cx);
                    cx.notify();
                });

                // returning early to prevent dismissing the modal, so a user can enter
                // a remote name first.
                return;
            }
            Entry::NewRemoteName { name, url } => {
                self.create_remote(name.clone(), url.to_string(), window, cx);
            }
            Entry::NewBranch { name } => {
                let from_branch = if secondary {
                    self.default_branch.clone()
                } else {
                    None
                };
                self.create_branch(from_branch, name.into(), window, cx);
            }
        }

        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.state = PickerState::List;
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = &self.matches.get(ix)?;

        let (commit_time, absolute_time, author_name, subject) = entry
            .as_branch()
            .and_then(|branch| {
                branch.most_recent_commit.as_ref().map(|commit| {
                    let subject = commit.subject.clone();
                    let commit_time = OffsetDateTime::from_unix_timestamp(commit.commit_timestamp)
                        .unwrap_or_else(|_| OffsetDateTime::now_utc());
                    let local_offset =
                        time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
                    let formatted_time = time_format::format_localized_timestamp(
                        commit_time,
                        OffsetDateTime::now_utc(),
                        local_offset,
                        time_format::TimestampFormat::Relative,
                    );
                    let absolute_time = time_format::format_localized_timestamp(
                        commit_time,
                        OffsetDateTime::now_utc(),
                        local_offset,
                        time_format::TimestampFormat::EnhancedAbsolute,
                    );
                    let author = commit.author_name.clone();
                    (
                        Some(formatted_time),
                        Some(absolute_time),
                        Some(author),
                        Some(subject),
                    )
                })
            })
            .unwrap_or_else(|| (None, None, None, None));

        let is_head_branch = entry.as_branch().is_some_and(|branch| branch.is_head);

        let entry_icon = match entry {
            Entry::NewUrl { .. } | Entry::NewBranch { .. } | Entry::NewRemoteName { .. } => {
                IconName::Plus
            }
            Entry::Branch { branch, .. } => {
                if is_head_branch {
                    IconName::Check
                } else if branch.is_remote() {
                    IconName::Screen
                } else {
                    IconName::GitBranch
                }
            }
        };

        let entry_title = match entry {
            Entry::NewUrl { .. } => Label::new("Create Remote Repository")
                .single_line()
                .truncate()
                .into_any_element(),
            Entry::NewBranch { name } => Label::new(format!("Create Branch: \"{name}\"…"))
                .single_line()
                .truncate()
                .into_any_element(),
            Entry::NewRemoteName { name, .. } => Label::new(format!("Create Remote: \"{name}\""))
                .single_line()
                .truncate()
                .into_any_element(),
            Entry::Branch { branch, positions } => {
                HighlightedLabel::new(branch.name().to_string(), positions.clone())
                    .single_line()
                    .truncate()
                    .into_any_element()
            }
        };

        let focus_handle = self.focus_handle.clone();
        let is_new_items = matches!(
            entry,
            Entry::NewUrl { .. } | Entry::NewBranch { .. } | Entry::NewRemoteName { .. }
        );

        let is_head_branch = entry.as_branch().is_some_and(|branch| branch.is_head);

        let deleted_branch_icon = |entry_ix: usize| {
            IconButton::new(("delete", entry_ix), IconName::Trash)
                .icon_size(IconSize::Small)
                .tooltip(move |_, cx| {
                    Tooltip::for_action_in(
                        "Delete Branch",
                        &branch_picker::DeleteBranch,
                        &focus_handle,
                        cx,
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.delegate.delete_at(entry_ix, window, cx);
                }))
        };

        let create_from_default_button = self.default_branch.as_ref().map(|default_branch| {
            let tooltip_label: SharedString = format!("Create New From: {default_branch}").into();
            let focus_handle = self.focus_handle.clone();

            IconButton::new("create_from_default", IconName::GitBranchPlus)
                .icon_size(IconSize::Small)
                .tooltip(move |_, cx| {
                    Tooltip::for_action_in(
                        tooltip_label.clone(),
                        &menu::SecondaryConfirm,
                        &focus_handle,
                        cx,
                    )
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.delegate.confirm(true, window, cx);
                }))
                .into_any_element()
        });

        Some(
            ListItem::new(format!("vcs-menu-{ix}"))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2p5()
                        .flex_grow()
                        .child(
                            Icon::new(entry_icon)
                                .color(if is_head_branch {
                                    Color::Accent
                                } else {
                                    Color::Muted
                                })
                                .size(IconSize::Small),
                        )
                        .child(
                            v_flex()
                                .id("info_container")
                                .w_full()
                                .child(entry_title)
                                .child({
                                    let message = match entry {
                                        Entry::NewUrl { url } => format!("Based off {url}"),
                                        Entry::NewRemoteName { url, .. } => {
                                            format!("Based off {url}")
                                        }
                                        Entry::NewBranch { .. } => {
                                            if let Some(current_branch) =
                                                self.repo.as_ref().and_then(|repo| {
                                                    repo.read(cx).branch.as_ref().map(|b| b.name())
                                                })
                                            {
                                                format!("Based off {}", current_branch)
                                            } else {
                                                "Based off the current branch".to_string()
                                            }
                                        }
                                        Entry::Branch { .. } => String::new(),
                                    };

                                    if matches!(entry, Entry::Branch { .. }) {
                                        let show_author_name = ProjectSettings::get_global(cx)
                                            .git
                                            .branch_picker
                                            .show_author_name;
                                        let has_author = show_author_name && author_name.is_some();
                                        let has_commit = commit_time.is_some();
                                        let author_for_meta =
                                            if show_author_name { author_name } else { None };

                                        let dot = || {
                                            Label::new("•")
                                                .alpha(0.5)
                                                .color(Color::Muted)
                                                .size(LabelSize::Small)
                                        };

                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .gap_1p5()
                                            .when_some(author_for_meta, |this, author| {
                                                this.child(
                                                    Label::new(author)
                                                        .color(Color::Muted)
                                                        .size(LabelSize::Small),
                                                )
                                            })
                                            .when_some(commit_time, |this, time| {
                                                this.when(has_author, |this| this.child(dot()))
                                                    .child(
                                                        Label::new(time)
                                                            .color(Color::Muted)
                                                            .size(LabelSize::Small),
                                                    )
                                            })
                                            .when_some(subject, |this, subj| {
                                                this.when(has_commit, |this| this.child(dot()))
                                                    .child(
                                                        Label::new(subj.to_string())
                                                            .color(Color::Muted)
                                                            .size(LabelSize::Small)
                                                            .truncate()
                                                            .flex_1(),
                                                    )
                                            })
                                            .when(!has_commit, |this| {
                                                this.child(
                                                    Label::new("No commits found")
                                                        .color(Color::Muted)
                                                        .size(LabelSize::Small),
                                                )
                                            })
                                            .into_any_element()
                                    } else {
                                        Label::new(message)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .truncate()
                                            .into_any_element()
                                    }
                                })
                                .when_some(
                                    entry.as_branch().map(|b| b.name().to_string()),
                                    |this, branch_name| {
                                        let absolute_time = absolute_time.clone();
                                        this.tooltip({
                                            let is_head = is_head_branch;
                                            Tooltip::element(move |_, _| {
                                                v_flex()
                                                    .child(Label::new(branch_name.clone()))
                                                    .when(is_head, |this| {
                                                        this.child(
                                                            Label::new("Current Branch")
                                                                .size(LabelSize::Small)
                                                                .color(Color::Muted),
                                                        )
                                                    })
                                                    .when_some(
                                                        absolute_time.clone(),
                                                        |this, time| {
                                                            this.child(
                                                                Label::new(time)
                                                                    .size(LabelSize::Small)
                                                                    .color(Color::Muted),
                                                            )
                                                        },
                                                    )
                                                    .into_any_element()
                                            })
                                        })
                                    },
                                ),
                        ),
                )
                .when(!is_new_items && !is_head_branch, |this| {
                    this.end_slot(deleted_branch_icon(ix))
                        .show_end_slot_on_hover()
                })
                .when_some(
                    if is_new_items {
                        create_from_default_button
                    } else {
                        None
                    },
                    |this, create_from_default_button| {
                        this.end_slot(create_from_default_button)
                            .show_end_slot_on_hover()
                    },
                ),
        )
    }

    fn render_footer(&self, _: &mut Window, cx: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        if !self.show_footer || self.editor_position() == PickerEditorPosition::End {
            return None;
        }
        let focus_handle = self.focus_handle.clone();

        let footer_container = || {
            h_flex()
                .w_full()
                .p_1p5()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
        };

        match self.state {
            PickerState::List => {
                let selected_entry = self.matches.get(self.selected_index);

                let branch_from_default_button = self
                    .default_branch
                    .as_ref()
                    .filter(|_| matches!(selected_entry, Some(Entry::NewBranch { .. })))
                    .map(|default_branch| {
                        let button_label = format!("Create New From: {default_branch}");

                        Button::new("branch-from-default", button_label)
                            .key_binding(
                                KeyBinding::for_action_in(
                                    &menu::SecondaryConfirm,
                                    &focus_handle,
                                    cx,
                                )
                                .map(|kb| kb.size(rems_from_px(12.))),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delegate.confirm(true, window, cx);
                            }))
                    });

                let delete_and_select_btns = h_flex()
                    .gap_1()
                    .when(
                        !selected_entry
                            .and_then(|entry| entry.as_branch())
                            .is_some_and(|branch| branch.is_head),
                        |this| {
                            this.child(
                                Button::new("delete-branch", "Delete")
                                    .key_binding(
                                        KeyBinding::for_action_in(
                                            &branch_picker::DeleteBranch,
                                            &focus_handle,
                                            cx,
                                        )
                                        .map(|kb| kb.size(rems_from_px(12.))),
                                    )
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(
                                            branch_picker::DeleteBranch.boxed_clone(),
                                            cx,
                                        );
                                    }),
                            )
                        },
                    )
                    .child(
                        Button::new("switch_branch", "Switch")
                            .key_binding(
                                KeyBinding::for_action_in(&menu::Confirm, &focus_handle, cx)
                                    .map(|kb| kb.size(rems_from_px(12.))),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delegate.confirm(false, window, cx);
                            })),
                    );

                Some(
                    footer_container()
                        .map(|this| {
                            if branch_from_default_button.is_some() {
                                this.justify_end().when_some(
                                    branch_from_default_button,
                                    |this, button| {
                                        this.child(button).child(
                                            Button::new("create", "Create")
                                                .key_binding(
                                                    KeyBinding::for_action_in(
                                                        &menu::Confirm,
                                                        &focus_handle,
                                                        cx,
                                                    )
                                                    .map(|kb| kb.size(rems_from_px(12.))),
                                                )
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.delegate.confirm(false, window, cx);
                                                })),
                                        )
                                    },
                                )
                            } else {
                                this.justify_between()
                                    .child({
                                        let focus_handle = focus_handle.clone();
                                        let filter_label = match self.branch_filter {
                                            BranchFilter::All => "Filter Remote",
                                            BranchFilter::Remote => "Show All",
                                        };
                                        Button::new("filter-remotes", filter_label)
                                            .toggle_state(matches!(
                                                self.branch_filter,
                                                BranchFilter::Remote
                                            ))
                                            .key_binding(
                                                KeyBinding::for_action_in(
                                                    &branch_picker::FilterRemotes,
                                                    &focus_handle,
                                                    cx,
                                                )
                                                .map(|kb| kb.size(rems_from_px(12.))),
                                            )
                                            .on_click(|_click, window, cx| {
                                                window.dispatch_action(
                                                    branch_picker::FilterRemotes.boxed_clone(),
                                                    cx,
                                                );
                                            })
                                    })
                                    .child(delete_and_select_btns)
                            }
                        })
                        .into_any_element(),
                )
            }
            PickerState::NewBranch => {
                let branch_from_default_button =
                    self.default_branch.as_ref().map(|default_branch| {
                        let button_label = format!("Create New From: {default_branch}");

                        Button::new("branch-from-default", button_label)
                            .key_binding(
                                KeyBinding::for_action_in(
                                    &menu::SecondaryConfirm,
                                    &focus_handle,
                                    cx,
                                )
                                .map(|kb| kb.size(rems_from_px(12.))),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delegate.confirm(true, window, cx);
                            }))
                    });

                Some(
                    footer_container()
                        .gap_1()
                        .justify_end()
                        .when_some(branch_from_default_button, |this, button| {
                            this.child(button)
                        })
                        .child(
                            Button::new("create-new-branch", "Create")
                                .key_binding(
                                    KeyBinding::for_action_in(&menu::Confirm, &focus_handle, cx)
                                        .map(|kb| kb.size(rems_from_px(12.))),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.delegate.confirm(false, window, cx);
                                })),
                        )
                        .into_any_element(),
                )
            }
            PickerState::CreateRemote(_) => Some(
                footer_container()
                    .justify_end()
                    .child(
                        Button::new("confirm-create-remote", "Confirm")
                            .key_binding(
                                KeyBinding::for_action_in(&menu::Confirm, &focus_handle, cx)
                                    .map(|kb| kb.size(rems_from_px(12.))),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delegate.confirm(false, window, cx);
                            }))
                            .disabled(self.last_query.is_empty()),
                    )
                    .into_any_element(),
            ),
            PickerState::NewRemote => None,
        }
    }
}

// =====================================================================
//  S-BRP — IDEA-style tabbed branches popup.
//
//  Lives alongside the legacy `BranchList` so `git_picker.rs` and
//  `commit_modal.rs` keep working unchanged. New surface, new keybinding.
// =====================================================================

actions!(
    git_branches_popup,
    [
        /// Opens the IDEA-style tabbed branches popup (S-BRP).
        Open,
        /// Toggle favorite-status for the currently-selected branch row.
        ToggleFavorite,
    ]
);

#[derive(Debug, Clone)]
struct BranchStatusEntry {
    name: SharedString,
    is_remote: bool,
    is_head: bool,
    upstream_track: Option<SharedString>,
    subject: Option<SharedString>,
    committer_date_relative: Option<SharedString>,
}

impl BranchStatusEntry {
    fn from_branch(b: &Branch) -> Self {
        let track = b
            .upstream
            .as_ref()
            .and_then(|u| u.tracking.status())
            .map(|s| {
                let mut buf = String::new();
                if s.ahead > 0 {
                    use std::fmt::Write as _;
                    let _ = write!(buf, "↑{}", s.ahead);
                }
                if s.behind > 0 {
                    use std::fmt::Write as _;
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    let _ = write!(buf, "↓{}", s.behind);
                }
                SharedString::from(buf)
            })
            .filter(|s| !s.is_empty());
        let (subject, committer_date_relative) = b
            .most_recent_commit
            .as_ref()
            .map(|c| {
                let local_offset =
                    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
                let commit_time = OffsetDateTime::from_unix_timestamp(c.commit_timestamp)
                    .unwrap_or_else(|_| OffsetDateTime::now_utc());
                let relative = time_format::format_localized_timestamp(
                    commit_time,
                    OffsetDateTime::now_utc(),
                    local_offset,
                    time_format::TimestampFormat::Relative,
                );
                (
                    Some(c.subject.clone()),
                    Some(SharedString::from(relative)),
                )
            })
            .unwrap_or((None, None));
        Self {
            name: SharedString::from(b.name().to_string()),
            is_remote: b.is_remote(),
            is_head: b.is_head,
            upstream_track: track,
            subject,
            committer_date_relative,
        }
    }
}

#[derive(Debug, Clone)]
enum PopupRow {
    Branch {
        entry: BranchStatusEntry,
        depth: usize,
    },
    Group {
        path: SharedString,
        depth: usize,
        expanded: bool,
    },
    Tag {
        name: SharedString,
    },
    Backup {
        branch: SharedString,
        op: SharedString,
        before_sha: SharedString,
    },
    Empty {
        message: SharedString,
    },
}

pub struct BranchesPopup {
    workspace: WeakEntity<Workspace>,
    repository: Option<Entity<Repository>>,
    work_dir: Option<Arc<Path>>,
    tab: tabs::Tab,
    query: Entity<Editor>,
    rows: Vec<PopupRow>,
    selected_index: usize,
    branches: Vec<BranchStatusEntry>,
    tags: Vec<SharedString>,
    favorites_snapshot: favorites::RepoFavoritesSnapshot,
    expanded_groups: std::collections::HashSet<String>,
    backups: Vec<crate::backup_mcp::BackupEntry>,
    default_branch: Option<SharedString>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl BranchesPopup {
    pub fn open_action(
        workspace: &mut Workspace,
        _: &Open,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let repository = workspace.project().read(cx).active_repository(cx);
        let workspace_handle = workspace.weak_handle();
        workspace.toggle_modal(window, cx, |window, cx| {
            BranchesPopup::new(workspace_handle, repository, window, cx)
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search branches…", window, cx);
            editor
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &query,
            window,
            |this, _editor, event: &editor::EditorEvent, _window, cx| {
                if matches!(
                    event,
                    editor::EditorEvent::BufferEdited | editor::EditorEvent::Edited { .. }
                ) {
                    this.rebuild_rows(cx);
                }
            },
        ));

        let work_dir = repository
            .as_ref()
            .map(|r| r.read(cx).work_directory_abs_path.clone());

        if let Some(repo) = &repository {
            subscriptions.push(cx.subscribe(repo, |this, _repo, event, cx| {
                if matches!(event, RepositoryEvent::BranchListChanged) {
                    this.refresh_branches_from_repo(cx);
                }
            }));
        }

        let branches = repository
            .as_ref()
            .map(|r| {
                r.read(cx)
                    .branch_list
                    .iter()
                    .map(BranchStatusEntry::from_branch)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let favorites_snapshot = work_dir
            .as_ref()
            .and_then(|wd| favorites::load_for_repo(wd).log_err())
            .unwrap_or_default();

        let mut this = Self {
            workspace,
            repository: repository.clone(),
            work_dir,
            tab: tabs::Tab::Recent,
            query,
            rows: Vec::new(),
            selected_index: 0,
            branches,
            tags: Vec::new(),
            favorites_snapshot,
            expanded_groups: std::collections::HashSet::new(),
            backups: Vec::new(),
            default_branch: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };

        // Async: load default branch + tags + initial backups list.
        if let Some(repo) = repository {
            let default_request = repo.update(cx, |repo, _| repo.default_branch(false));
            let tags_request = repo.update(cx, |repo, _| repo.tags());
            cx.spawn(async move |this, cx| {
                let default = default_request.await.ok().and_then(Result::ok).flatten();
                this.update(cx, |this, cx| {
                    this.default_branch = default;
                    cx.notify();
                })
                .ok();
                if let Ok(Ok(tags)) = tags_request.await {
                    this.update(cx, |this, cx| {
                        this.tags = tags;
                        this.rebuild_rows(cx);
                    })
                    .ok();
                }
            })
            .detach();
        }

        this.rebuild_rows(cx);
        cx.focus_self(window);
        this
    }

    fn refresh_branches_from_repo(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = &self.repository {
            self.branches = repo
                .read(cx)
                .branch_list
                .iter()
                .map(BranchStatusEntry::from_branch)
                .collect();
            self.rebuild_rows(cx);
        }
    }

    fn set_tab(&mut self, tab: tabs::Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.selected_index = 0;
        self.rebuild_rows(cx);
        if matches!(tab, tabs::Tab::Backups) {
            self.refresh_backups(cx);
        }
        cx.notify();
    }

    fn refresh_backups(&mut self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.work_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { git::backup::list(&work_dir, None, None) })
                .await;
            if let Ok(list) = result {
                let backups: Vec<crate::backup_mcp::BackupEntry> = list
                    .into_iter()
                    .map(|b| crate::backup_mcp::BackupEntry {
                        branch: b.branch,
                        op: b.op,
                        timestamp_unix: b.timestamp_unix,
                        before_sha: b.before_sha,
                    })
                    .collect();
                this.update(cx, |this, cx| {
                    this.backups = backups;
                    this.rebuild_rows(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text(cx);
        let lower_query = query.to_lowercase();
        let favorites: collections::HashSet<String> =
            self.favorites_snapshot.favorites.iter().cloned().collect();
        let recent_order: std::collections::HashMap<String, usize> = self
            .favorites_snapshot
            .recent
            .iter()
            .enumerate()
            .map(|(i, e)| (e.branch.clone(), i))
            .collect();

        self.rows.clear();
        match self.tab {
            tabs::Tab::Recent => {
                let mut entries: Vec<&BranchStatusEntry> = self
                    .branches
                    .iter()
                    .filter(|b| !b.is_remote)
                    .filter(|b| recent_order.contains_key(b.name.as_ref()))
                    .filter(|b| query.is_empty() || b.name.to_lowercase().contains(&lower_query))
                    .collect();
                entries.sort_by_key(|b| {
                    *recent_order
                        .get(b.name.as_ref())
                        .unwrap_or(&usize::MAX)
                });
                if entries.is_empty() {
                    self.rows.push(PopupRow::Empty {
                        message: SharedString::from(
                            "No recently checked-out branches yet — checkout one to populate.",
                        ),
                    });
                } else {
                    for entry in entries {
                        self.rows.push(PopupRow::Branch {
                            entry: entry.clone(),
                            depth: 0,
                        });
                    }
                }
            }
            tabs::Tab::Local | tabs::Tab::Remote => {
                let want_remote = matches!(self.tab, tabs::Tab::Remote);
                let mut entries: Vec<&BranchStatusEntry> = self
                    .branches
                    .iter()
                    .filter(|b| b.is_remote == want_remote)
                    .filter(|b| query.is_empty() || b.name.to_lowercase().contains(&lower_query))
                    .collect();
                entries.sort_by(|a, b| a.name.as_ref().cmp(b.name.as_ref()));
                let names: Vec<String> =
                    entries.iter().map(|e| e.name.to_string()).collect();
                let tree = tree::BranchTree::build(&names, self.expanded_groups.clone());
                let by_name: std::collections::HashMap<&str, &BranchStatusEntry> = entries
                    .iter()
                    .map(|e| (e.name.as_ref(), *e))
                    .collect();
                if names.is_empty() {
                    self.rows.push(PopupRow::Empty {
                        message: SharedString::from(if want_remote {
                            "No remote branches"
                        } else {
                            "No local branches"
                        }),
                    });
                } else {
                    for row in tree.rows {
                        match row {
                            tree::TreeRow::Group { path, depth, expanded } => {
                                self.rows.push(PopupRow::Group {
                                    path: SharedString::from(path),
                                    depth,
                                    expanded,
                                });
                            }
                            tree::TreeRow::Leaf {
                                full_name, depth, ..
                            } => {
                                if let Some(entry) = by_name.get(full_name.as_str()) {
                                    self.rows.push(PopupRow::Branch {
                                        entry: (*entry).clone(),
                                        depth,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            tabs::Tab::Tags => {
                let mut tags: Vec<SharedString> = self
                    .tags
                    .iter()
                    .filter(|t| query.is_empty() || t.to_lowercase().contains(&lower_query))
                    .cloned()
                    .collect();
                tags.sort();
                if tags.is_empty() {
                    self.rows.push(PopupRow::Empty {
                        message: SharedString::from("No tags"),
                    });
                } else {
                    for tag in tags {
                        self.rows.push(PopupRow::Tag { name: tag });
                    }
                }
            }
            tabs::Tab::Favorites => {
                let mut entries: Vec<&BranchStatusEntry> = self
                    .branches
                    .iter()
                    .filter(|b| favorites.contains(b.name.as_ref()))
                    .filter(|b| query.is_empty() || b.name.to_lowercase().contains(&lower_query))
                    .collect();
                entries.sort_by(|a, b| a.name.as_ref().cmp(b.name.as_ref()));
                if entries.is_empty() {
                    self.rows.push(PopupRow::Empty {
                        message: SharedString::from(
                            "No favorites yet — star a branch to keep it here.",
                        ),
                    });
                } else {
                    for entry in entries {
                        self.rows.push(PopupRow::Branch {
                            entry: entry.clone(),
                            depth: 0,
                        });
                    }
                }
            }
            tabs::Tab::Backups => {
                if self.backups.is_empty() {
                    self.rows.push(PopupRow::Empty {
                        message: SharedString::from("No backup refs."),
                    });
                } else {
                    let mut backups = self.backups.clone();
                    if !query.is_empty() {
                        backups.retain(|b| {
                            b.branch.to_lowercase().contains(&lower_query)
                                || b.op.to_lowercase().contains(&lower_query)
                        });
                    }
                    for backup in backups {
                        self.rows.push(PopupRow::Backup {
                            branch: SharedString::from(backup.branch),
                            op: SharedString::from(backup.op),
                            before_sha: SharedString::from(backup.before_sha),
                        });
                    }
                }
            }
        }

        if self.selected_index >= self.rows.len() {
            self.selected_index = 0;
        }
        cx.notify();
    }

    fn is_favorite(&self, branch_name: &str) -> bool {
        self.favorites_snapshot
            .favorites
            .iter()
            .any(|b| b == branch_name)
    }

    fn current_head(&self) -> Option<&str> {
        self.branches
            .iter()
            .find(|b| b.is_head)
            .map(|b| b.name.as_ref())
    }

    fn dispatch_default(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(idx).cloned() else {
            return;
        };
        match row {
            PopupRow::Branch { entry, .. } => {
                self.checkout_branch(entry.name, window, cx);
                cx.emit(DismissEvent);
            }
            PopupRow::Group { path, .. } => {
                if self.expanded_groups.contains(path.as_ref()) {
                    self.expanded_groups.remove(path.as_ref());
                } else {
                    self.expanded_groups.insert(path.to_string());
                }
                self.rebuild_rows(cx);
            }
            PopupRow::Tag { name } => {
                self.checkout_revision(name, window, cx);
                cx.emit(DismissEvent);
            }
            PopupRow::Backup { branch, before_sha, .. } => {
                self.restore_backup(branch, before_sha, window, cx);
            }
            PopupRow::Empty { .. } => {}
        }
    }

    fn checkout_branch(
        &mut self,
        branch: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repository.clone() else {
            return;
        };
        let work_dir = self.work_dir.clone();
        let workspace = self.workspace.clone();
        let branch_for_recent = branch.clone();
        cx.spawn_in(window, async move |_, cx| {
            let recv = repo.update(cx, |repo, _| repo.change_branch(branch.to_string()));
            match recv.await {
                Ok(Ok(())) => {
                    if let Some(work_dir) = work_dir {
                        favorites::record_checkout(&work_dir, branch_for_recent.as_ref())
                            .log_err();
                    }
                    anyhow::Ok(())
                }
                Ok(Err(e)) => {
                    if let Some(workspace) = workspace.upgrade() {
                        cx.update(|_window, cx| {
                            show_error_toast(
                                workspace,
                                format!("git switch {}", branch_for_recent),
                                e,
                                cx,
                            );
                        })?;
                    }
                    Ok(())
                }
                Err(_) => Err(anyhow::anyhow!("change_branch was canceled")),
            }
        })
        .detach_and_log_err(cx);
    }

    fn checkout_revision(
        &mut self,
        revision: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repository.clone() else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            let recv = repo.update(cx, |repo, _| repo.checkout_revision(revision.to_string()));
            recv.await??;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn restore_backup(
        &mut self,
        branch: SharedString,
        before_sha: SharedString,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(work_dir) = self.work_dir.clone() else {
            return;
        };
        cx.background_spawn(async move {
            crate::backup_mcp::create_restore_ref(&work_dir, branch.as_ref(), before_sha.as_ref())
                .log_err();
        })
        .detach();
    }

    fn handle_toggle_favorite(
        &mut self,
        _: &ToggleFavorite,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(work_dir) = self.work_dir.clone() else {
            return;
        };
        let Some(row) = self.rows.get(self.selected_index).cloned() else {
            return;
        };
        let branch_name = match row {
            PopupRow::Branch { entry, .. } => entry.name,
            _ => return,
        };
        let work_dir_clone = work_dir.clone();
        let branch_string = branch_name.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    favorites::toggle_favorite(&work_dir_clone, &branch_string)
                })
                .await;
            if result.is_ok() {
                let snapshot = cx
                    .background_spawn(async move { favorites::load_for_repo(&work_dir) })
                    .await
                    .ok();
                this.update(cx, |this, cx| {
                    if let Some(snapshot) = snapshot {
                        this.favorites_snapshot = snapshot;
                    }
                    this.rebuild_rows(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let idx = self.selected_index;
        self.dispatch_default(idx, window, cx);
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_prev(
        &mut self,
        _: &menu::SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            cx.notify();
        }
    }

    fn select_next(
        &mut self,
        _: &menu::SelectNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.rows.is_empty() && self.selected_index + 1 < self.rows.len() {
            self.selected_index += 1;
            cx.notify();
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tab;
        h_flex()
            .px_2()
            .pt_2()
            .pb_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .children(tabs::Tab::all().into_iter().enumerate().map(|(ix, tab)| {
                let label = tab.label();
                let is_active = tab == active;
                Button::new(("branches-popup-tab", ix), label)
                    .label_size(LabelSize::Small)
                    .toggle_state(is_active)
                    .start_icon(Icon::new(tab.icon()).size(IconSize::XSmall))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.set_tab(tab, cx);
                    }))
            }))
    }

    fn render_row(
        &self,
        ix: usize,
        row: &PopupRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = ix == self.selected_index;
        match row {
            PopupRow::Empty { message } => Label::new(message.clone())
                .color(Color::Muted)
                .size(LabelSize::Small)
                .into_any_element(),
            PopupRow::Group { path, depth, expanded } => {
                let path = path.clone();
                let chevron = if *expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                ListItem::new(("branches-popup-group", ix))
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(selected)
                    .start_slot(Icon::new(chevron).size(IconSize::Small))
                    .child(
                        h_flex()
                            .pl(rems(*depth as f32 * 1.0))
                            .child(Label::new(path.clone()).color(Color::Muted)),
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if this.expanded_groups.contains(path.as_ref()) {
                            this.expanded_groups.remove(path.as_ref());
                        } else {
                            this.expanded_groups.insert(path.to_string());
                        }
                        this.rebuild_rows(cx);
                    }))
                    .into_any_element()
            }
            PopupRow::Branch { entry, depth, .. } => self
                .render_branch_row(ix, entry, *depth, selected, cx)
                .into_any_element(),
            PopupRow::Tag { name } => {
                let tag_name = name.clone();
                ListItem::new(("branches-popup-tag", ix))
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(selected)
                    .start_slot(Icon::new(IconName::Hash).size(IconSize::Small))
                    .child(Label::new(tag_name.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.checkout_revision(tag_name.clone(), window, cx);
                        cx.emit(DismissEvent);
                    }))
                    .on_secondary_mouse_down(cx.listener({
                        let tag = name.clone();
                        move |this, _: &MouseDownEvent, window, cx| {
                            let workspace = this.workspace.clone();
                            let Some(repository) = this.repository.clone() else {
                                return;
                            };
                            let menu = context_menu::build_tag_menu(
                                context_menu::TagContext {
                                    workspace,
                                    repository,
                                    tag_name: tag.clone(),
                                },
                                window,
                                cx,
                            );
                            window.defer(cx, move |window, cx| {
                                menu.update(cx, |menu, cx| {
                                    menu.focus_handle(cx).focus(window, cx);
                                });
                            });
                        }
                    }))
                    .into_any_element()
            }
            PopupRow::Backup {
                branch,
                op,
                before_sha,
            } => {
                let short_sha: String = before_sha.chars().take(7).collect();
                let label = format!("{} ({}) — {}", branch, op, short_sha);
                let branch_clone = branch.clone();
                let sha_clone = before_sha.clone();
                ListItem::new(("branches-popup-backup", ix))
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(selected)
                    .start_slot(Icon::new(IconName::CountdownTimer).size(IconSize::Small))
                    .child(Label::new(label).color(Color::Muted))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.restore_backup(branch_clone.clone(), sha_clone.clone(), window, cx);
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_branch_row(
        &self,
        ix: usize,
        entry: &BranchStatusEntry,
        depth: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_default = self
            .default_branch
            .as_ref()
            .is_some_and(|d| d.as_ref() == entry.name.as_ref());
        let is_favorite = self.is_favorite(entry.name.as_ref());

        // S-SOL-PRT — surface a lock indicator next to protected
        // branches. We key off `delete_branch` because it's the op
        // most users associate with "is this branch protected?" and
        // the policy maps protected branches to `Forbidden` for
        // delete. Cheap glob-match — the snapshot is cached.
        let is_protected = self
            .work_dir
            .as_ref()
            .map(|wd| {
                matches!(
                    solutions::branch_protection::check(
                        wd,
                        entry.name.as_ref(),
                        "delete_branch"
                    ),
                    solutions::branch_protection::Decision::Forbidden { .. }
                )
            })
            .unwrap_or(false);
        let star_icon = if is_favorite {
            IconName::StarFilled
        } else {
            IconName::Star
        };
        let star_color = if is_favorite { Color::Accent } else { Color::Muted };

        let entry_for_click = entry.clone();
        let entry_for_menu = entry.clone();
        let is_head = entry.is_head;
        let entry_label = entry.name.clone();
        let track = entry.upstream_track.clone();
        let subject = entry.subject.clone();
        let date = entry.committer_date_relative.clone();

        let star_branch = entry.name.to_string();
        let star_button = IconButton::new(("branches-popup-star", ix), star_icon)
            .icon_size(IconSize::Small)
            .icon_color(star_color)
            .tooltip(Tooltip::text(if is_favorite {
                "Unfavorite Branch"
            } else {
                "Favorite Branch"
            }))
            .on_click(cx.listener(move |this, _, _window, cx| {
                let Some(work_dir) = this.work_dir.clone() else {
                    return;
                };
                let branch = star_branch.clone();
                cx.spawn(async move |this, cx| {
                    let _ = cx
                        .background_spawn(async move {
                            favorites::toggle_favorite(&work_dir, &branch)
                        })
                        .await;
                    let work_dir = this
                        .read_with(cx, |this, _| this.work_dir.clone())
                        .ok()
                        .flatten();
                    if let Some(work_dir) = work_dir {
                        let snap = cx
                            .background_spawn(async move { favorites::load_for_repo(&work_dir) })
                            .await
                            .ok();
                        this.update(cx, |this, cx| {
                            if let Some(snap) = snap {
                                this.favorites_snapshot = snap;
                            }
                            this.rebuild_rows(cx);
                        })
                        .ok();
                    }
                })
                .detach();
            }));

        let icon_name = if is_head {
            IconName::Check
        } else if entry.is_remote {
            IconName::Screen
        } else {
            IconName::GitBranch
        };
        let icon_color = if is_head { Color::Accent } else { Color::Muted };

        ListItem::new(("branches-popup-branch", ix))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .start_slot(Icon::new(icon_name).color(icon_color).size(IconSize::Small))
            .child(
                h_flex()
                    .w_full()
                    .pl(rems(depth as f32 * 1.0))
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .when(is_protected, |this| {
                                        this.child(
                                            Icon::new(IconName::LockOutlined)
                                                .color(Color::Muted)
                                                .size(IconSize::XSmall),
                                        )
                                    })
                                    .child(Label::new(entry_label))
                                    .when(is_default, |this| {
                                        this.child(
                                            Label::new("default")
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                    })
                                    .when_some(track, |this, t| {
                                        this.child(
                                            Label::new(t)
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                    }),
                            )
                            .when(subject.is_some() || date.is_some(), |this| {
                                this.child(
                                    h_flex()
                                        .gap_1()
                                        .when_some(date, |this, d| {
                                            this.child(
                                                Label::new(d)
                                                    .size(LabelSize::XSmall)
                                                    .color(Color::Muted),
                                            )
                                        })
                                        .when_some(subject, |this, s| {
                                            this.child(
                                                Label::new(s.to_string())
                                                    .size(LabelSize::XSmall)
                                                    .color(Color::Muted)
                                                    .truncate(),
                                            )
                                        }),
                                )
                            }),
                    )
                    .child(star_button),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.standard_click() {
                    this.checkout_branch(entry_for_click.name.clone(), window, cx);
                    cx.emit(DismissEvent);
                }
            }))
            .on_secondary_mouse_down(cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                let workspace = this.workspace.clone();
                let Some(repository) = this.repository.clone() else {
                    return;
                };
                let is_favorite = this.is_favorite(entry_for_menu.name.as_ref());
                let menu = context_menu::build_branch_menu(
                    context_menu::BranchContext {
                        workspace,
                        repository,
                        branch_name: entry_for_menu.name.clone(),
                        is_remote: entry_for_menu.is_remote,
                        is_head,
                        is_favorite,
                    },
                    window,
                    cx,
                );
                window.defer(cx, move |window, cx| {
                    menu.update(cx, |menu, cx| {
                        menu.focus_handle(cx).focus(window, cx);
                    });
                });
            }))
            .into_any_element()
    }
}

impl ModalView for BranchesPopup {}
impl EventEmitter<DismissEvent> for BranchesPopup {}

impl Focusable for BranchesPopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BranchesPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row_count = self.rows.len();
        let head = self.current_head().map(|s| s.to_string());
        let popup = v_flex()
            .key_context("BranchesPopup")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::handle_toggle_favorite))
            .elevation_2(cx)
            .w(rems(48.))
            .max_h(rems(36.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::GitBranch).size(IconSize::XSmall))
                    .child(Headline::new("Branches").size(HeadlineSize::XSmall))
                    .when_some(head, |this, h| {
                        this.child(
                            Label::new(format!("on {}", h))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .child(self.query.clone()),
            )
            .child(self.render_tab_bar(cx))
            .child(div().h_px().bg(cx.theme().colors().border_variant))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        uniform_list("branches-popup-list", row_count, cx.processor(
                            |this, range: std::ops::Range<usize>, _window, cx| {
                                let mut items = Vec::with_capacity(range.len());
                                for ix in range {
                                    if let Some(row) = this.rows.get(ix).cloned() {
                                        items.push(this.render_row(ix, &row, cx));
                                    }
                                }
                                items
                            },
                        ))
                        .h_full(),
                    ),
            );

        popup
    }
}

// ---- modals invoked from the per-branch context menu ----

pub struct SetUpstreamModal {
    repo: Entity<Repository>,
    branch: SharedString,
    editor: Entity<Editor>,
}

impl SetUpstreamModal {
    pub fn new(
        repo: Entity<Repository>,
        branch: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("origin/main", window, cx);
            editor
        });
        Self {
            repo,
            branch,
            editor,
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let upstream = self.editor.read(cx).text(cx);
        let upstream = upstream.trim().to_string();
        if upstream.is_empty() {
            cx.emit(DismissEvent);
            return;
        }
        let repo = self.repo.clone();
        let branch = self.branch.to_string();
        cx.spawn(async move |_, cx| {
            let recv = repo.update(cx, |repo, _| repo.set_upstream(branch, upstream));
            recv.await??;
            anyhow::Ok(())
        })
        .detach_and_prompt_err("Failed to set upstream", window, cx, |_, _, _| None);
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for SetUpstreamModal {}
impl ModalView for SetUpstreamModal {}
impl Focusable for SetUpstreamModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for SetUpstreamModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SetUpstreamModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(34.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::GitBranch).size(IconSize::XSmall))
                    .child(
                        Headline::new(format!("Set Upstream for {}", self.branch))
                            .size(HeadlineSize::XSmall),
                    ),
            )
            .child(div().px_3().pb_3().w_full().child(self.editor.clone()))
    }
}

pub struct RenameBranchPopupModal {
    branch: SharedString,
    work_dir: Arc<Path>,
    editor: Entity<Editor>,
}

impl RenameBranchPopupModal {
    pub fn new(
        _repo: Entity<Repository>,
        branch: SharedString,
        work_dir: Arc<Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(branch.to_string(), window, cx);
            editor
        });
        Self {
            branch,
            work_dir,
            editor,
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let new_name = self.editor.read(cx).text(cx).trim().to_string();
        if new_name.is_empty() || new_name == self.branch.as_ref() {
            cx.emit(DismissEvent);
            return;
        }
        let old = self.branch.to_string();
        let work_dir = self.work_dir.to_path_buf();
        cx.spawn(async move |_, cx| {
            cx.background_spawn(async move {
                git::operations::OpRunner::run(
                    git::operations::RenameBranchOp { old, new: new_name },
                    &work_dir,
                )
            })
            .await
        })
        .detach_and_prompt_err("Failed to rename branch", window, cx, |_, _, _| None);
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for RenameBranchPopupModal {}
impl ModalView for RenameBranchPopupModal {}
impl Focusable for RenameBranchPopupModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for RenameBranchPopupModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("RenameBranchPopupModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(34.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::GitBranch).size(IconSize::XSmall))
                    .child(
                        Headline::new(format!("Rename Branch ({})", self.branch))
                            .size(HeadlineSize::XSmall),
                    ),
            )
            .child(div().px_3().pb_3().w_full().child(self.editor.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use git::repository::{CommitSummary, Remote};
    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use rand::{Rng, rngs::StdRng};
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use workspace::MultiWorkspace;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
    }

    fn create_test_branch(
        name: &str,
        is_head: bool,
        remote_name: Option<&str>,
        timestamp: Option<i64>,
    ) -> Branch {
        let ref_name = match remote_name {
            Some(remote_name) => format!("refs/remotes/{remote_name}/{name}"),
            None => format!("refs/heads/{name}"),
        };

        Branch {
            is_head,
            ref_name: ref_name.into(),
            upstream: None,
            most_recent_commit: timestamp.map(|ts| CommitSummary {
                sha: "abc123".into(),
                commit_timestamp: ts,
                author_name: "Test Author".into(),
                subject: "Test commit".into(),
                has_parent: true,
            }),
        }
    }

    fn create_test_branches() -> Vec<Branch> {
        vec![
            create_test_branch("main", true, None, Some(1000)),
            create_test_branch("feature-auth", false, None, Some(900)),
            create_test_branch("feature-ui", false, None, Some(800)),
            create_test_branch("develop", false, None, Some(700)),
        ]
    }

    async fn init_branch_list_test(
        repository: Option<Entity<Repository>>,
        branches: Vec<Branch>,
        cx: &mut TestAppContext,
    ) -> (Entity<BranchList>, VisualTestContext) {
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;

        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window_handle
            .read_with(cx, |mw, _| mw.workspace().clone())
            .unwrap();

        let branch_list = window_handle
            .update(cx, |_multi_workspace, window, cx| {
                cx.new(|cx| {
                    let mut delegate = BranchListDelegate::new(
                        workspace.downgrade(),
                        repository,
                        BranchListStyle::Modal,
                        cx,
                    );
                    delegate.all_branches = branches;
                    let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
                    let picker_focus_handle = picker.focus_handle(cx);
                    picker.update(cx, |picker, _| {
                        picker.delegate.focus_handle = picker_focus_handle.clone();
                    });

                    let _subscription = cx.subscribe(&picker, |_, _, _, cx| {
                        cx.emit(DismissEvent);
                    });

                    BranchList {
                        picker,
                        picker_focus_handle,
                        width: rems(34.),
                        _subscriptions: vec![_subscription],
                        embedded: false,
                    }
                })
            })
            .unwrap();

        let cx = VisualTestContext::from_window(window_handle.into(), cx);

        (branch_list, cx)
    }

    async fn init_fake_repository(
        cx: &mut TestAppContext,
    ) -> (Entity<Project>, Entity<Repository>) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/dir"),
            json!({
                ".git": {},
                "file.txt": "buffer_text".to_string()
            }),
        )
        .await;
        fs.set_head_for_repo(
            path!("/dir/.git").as_ref(),
            &[("file.txt", "test".to_string())],
            "deadbeef",
        );
        fs.set_index_for_repo(
            path!("/dir/.git").as_ref(),
            &[("file.txt", "index_text".to_string())],
        );

        let project = Project::test(fs.clone(), [path!("/dir").as_ref()], cx).await;
        let repository = cx.read(|cx| project.read(cx).active_repository(cx));

        (project, repository.unwrap())
    }

    #[gpui::test]
    async fn test_update_branch_matches_with_query(cx: &mut TestAppContext) {
        init_test(cx);

        let branches = create_test_branches();
        let (branch_list, mut ctx) = init_branch_list_test(None, branches, cx).await;
        let cx = &mut ctx;

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                let query = "feature".to_string();
                branch_list.picker.update(cx, |picker, cx| {
                    picker.delegate.update_matches(query, window, cx)
                })
            })
            .await;
        cx.run_until_parked();

        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                // Should have 2 existing branches + 1 "create new branch" entry = 3 total
                assert_eq!(picker.delegate.matches.len(), 3);
                assert!(
                    picker
                        .delegate
                        .matches
                        .iter()
                        .any(|m| m.name() == "feature-auth")
                );
                assert!(
                    picker
                        .delegate
                        .matches
                        .iter()
                        .any(|m| m.name() == "feature-ui")
                );
                // Verify the last entry is the "create new branch" option
                let last_match = picker.delegate.matches.last().unwrap();
                assert!(last_match.is_new_branch());
            })
        });
    }

    async fn update_branch_list_matches_with_empty_query(
        branch_list: &Entity<BranchList>,
        cx: &mut VisualTestContext,
    ) {
        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker.delegate.update_matches(String::new(), window, cx)
                })
            })
            .await;
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_delete_branch(cx: &mut TestAppContext) {
        init_test(cx);
        let (_project, repository) = init_fake_repository(cx).await;

        let branches = create_test_branches();

        let branch_names = branches
            .iter()
            .map(|branch| branch.name().to_string())
            .collect::<Vec<String>>();
        let repo = repository.clone();
        cx.spawn(async move |mut cx| {
            for branch in branch_names {
                repo.update(&mut cx, |repo, _| repo.create_branch(branch, None))
                    .await
                    .unwrap()
                    .unwrap();
            }
        })
        .await;
        cx.run_until_parked();

        let (branch_list, mut ctx) = init_branch_list_test(repository.into(), branches, cx).await;
        let cx = &mut ctx;

        update_branch_list_matches_with_empty_query(&branch_list, cx).await;

        let branch_to_delete = branch_list.update_in(cx, |branch_list, window, cx| {
            branch_list.picker.update(cx, |picker, cx| {
                assert_eq!(picker.delegate.matches.len(), 4);
                let branch_to_delete = picker.delegate.matches.get(1).unwrap().name().to_string();
                picker.delegate.delete_at(1, window, cx);
                branch_to_delete
            })
        });
        cx.run_until_parked();

        let expected_branches = ["main", "feature-auth", "feature-ui", "develop"]
            .into_iter()
            .filter(|name| name != &branch_to_delete)
            .collect::<HashSet<_>>();
        let repo_branches = branch_list
            .update(cx, |branch_list, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker
                        .delegate
                        .repo
                        .as_ref()
                        .unwrap()
                        .update(cx, |repo, _cx| repo.branches())
                })
            })
            .await
            .unwrap()
            .unwrap();
        let repo_branches = repo_branches
            .iter()
            .map(|b| b.name())
            .collect::<HashSet<_>>();
        assert_eq!(&repo_branches, &expected_branches);

        branch_list.update(cx, move |branch_list, cx| {
            branch_list.picker.update(cx, move |picker, _cx| {
                assert_eq!(picker.delegate.matches.len(), 3);
                let branches = picker
                    .delegate
                    .matches
                    .iter()
                    .map(|be| be.name())
                    .collect::<HashSet<_>>();
                assert_eq!(branches, expected_branches);
            })
        });
    }

    #[gpui::test]
    async fn test_delete_remote_branch(cx: &mut TestAppContext) {
        init_test(cx);
        let (_project, repository) = init_fake_repository(cx).await;
        let branches = vec![
            create_test_branch("main", true, Some("origin"), Some(1000)),
            create_test_branch("feature-auth", false, Some("origin"), Some(900)),
            create_test_branch("feature-ui", false, Some("fork"), Some(800)),
            create_test_branch("develop", false, Some("private"), Some(700)),
        ];

        let branch_names = branches
            .iter()
            .map(|branch| branch.name().to_string())
            .collect::<Vec<String>>();
        let repo = repository.clone();
        cx.spawn(async move |mut cx| {
            for branch in branch_names {
                repo.update(&mut cx, |repo, _| repo.create_branch(branch, None))
                    .await
                    .unwrap()
                    .unwrap();
            }
        })
        .await;
        cx.run_until_parked();

        let (branch_list, mut ctx) = init_branch_list_test(repository.into(), branches, cx).await;
        let cx = &mut ctx;
        // Enable remote filter
        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                picker.delegate.branch_filter = BranchFilter::Remote;
            });
        });
        update_branch_list_matches_with_empty_query(&branch_list, cx).await;

        // Check matches, it should match all existing branches and no option to create new branch
        let branch_to_delete = branch_list.update_in(cx, |branch_list, window, cx| {
            branch_list.picker.update(cx, |picker, cx| {
                assert_eq!(picker.delegate.matches.len(), 4);
                let branch_to_delete = picker.delegate.matches.get(1).unwrap().name().to_string();
                picker.delegate.delete_at(1, window, cx);
                branch_to_delete
            })
        });
        cx.run_until_parked();

        let expected_branches = [
            "origin/main",
            "origin/feature-auth",
            "fork/feature-ui",
            "private/develop",
        ]
        .into_iter()
        .filter(|name| name != &branch_to_delete)
        .collect::<HashSet<_>>();
        let repo_branches = branch_list
            .update(cx, |branch_list, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker
                        .delegate
                        .repo
                        .as_ref()
                        .unwrap()
                        .update(cx, |repo, _cx| repo.branches())
                })
            })
            .await
            .unwrap()
            .unwrap();
        let repo_branches = repo_branches
            .iter()
            .map(|b| b.name())
            .collect::<HashSet<_>>();
        assert_eq!(&repo_branches, &expected_branches);

        // Check matches, it should match one less branch than before
        branch_list.update(cx, move |branch_list, cx| {
            branch_list.picker.update(cx, move |picker, _cx| {
                assert_eq!(picker.delegate.matches.len(), 3);
                let branches = picker
                    .delegate
                    .matches
                    .iter()
                    .map(|be| be.name())
                    .collect::<HashSet<_>>();
                assert_eq!(branches, expected_branches);
            })
        });
    }

    #[gpui::test]
    async fn test_branch_filter_shows_all_then_remotes_and_applies_query(cx: &mut TestAppContext) {
        init_test(cx);

        let branches = vec![
            create_test_branch("main", true, Some("origin"), Some(1000)),
            create_test_branch("feature-auth", false, Some("fork"), Some(900)),
            create_test_branch("feature-ui", false, None, Some(800)),
            create_test_branch("develop", false, None, Some(700)),
        ];

        let (branch_list, mut ctx) = init_branch_list_test(None, branches, cx).await;
        let cx = &mut ctx;

        update_branch_list_matches_with_empty_query(&branch_list, cx).await;

        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                assert_eq!(picker.delegate.matches.len(), 4);

                let branches = picker
                    .delegate
                    .matches
                    .iter()
                    .map(|be| be.name())
                    .collect::<HashSet<_>>();
                assert_eq!(
                    branches,
                    ["origin/main", "fork/feature-auth", "feature-ui", "develop"]
                        .into_iter()
                        .collect::<HashSet<_>>()
                );

                // Locals should be listed before remotes.
                let ordered = picker
                    .delegate
                    .matches
                    .iter()
                    .map(|be| be.name())
                    .collect::<Vec<_>>();
                assert_eq!(
                    ordered,
                    vec!["feature-ui", "develop", "origin/main", "fork/feature-auth"]
                );

                // Verify the last entry is NOT the "create new branch" option
                let last_match = picker.delegate.matches.last().unwrap();
                assert!(!last_match.is_new_branch());
                assert!(!last_match.is_new_url());
            })
        });

        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                picker.delegate.branch_filter = BranchFilter::Remote;
            })
        });

        update_branch_list_matches_with_empty_query(&branch_list, cx).await;

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    assert_eq!(picker.delegate.matches.len(), 2);
                    let branches = picker
                        .delegate
                        .matches
                        .iter()
                        .map(|be| be.name())
                        .collect::<HashSet<_>>();
                    assert_eq!(
                        branches,
                        ["origin/main", "fork/feature-auth"]
                            .into_iter()
                            .collect::<HashSet<_>>()
                    );

                    // Verify the last entry is NOT the "create new branch" option
                    let last_match = picker.delegate.matches.last().unwrap();
                    assert!(!last_match.is_new_url());
                    picker.delegate.branch_filter = BranchFilter::Remote;
                    picker
                        .delegate
                        .update_matches(String::from("fork"), window, cx)
                })
            })
            .await;
        cx.run_until_parked();

        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                // Should have 1 existing branch + 1 "create new branch" entry = 2 total
                assert_eq!(picker.delegate.matches.len(), 2);
                assert!(
                    picker
                        .delegate
                        .matches
                        .iter()
                        .any(|m| m.name() == "fork/feature-auth")
                );
                // Verify the last entry is the "create new branch" option
                let last_match = picker.delegate.matches.last().unwrap();
                assert!(last_match.is_new_branch());
            })
        });
    }

    #[gpui::test]
    async fn test_new_branch_creation_with_query(test_cx: &mut TestAppContext) {
        const MAIN_BRANCH: &str = "main";
        const FEATURE_BRANCH: &str = "feature";
        const NEW_BRANCH: &str = "new-feature-branch";

        init_test(test_cx);
        let (_project, repository) = init_fake_repository(test_cx).await;

        let branches = vec![
            create_test_branch(MAIN_BRANCH, true, None, Some(1000)),
            create_test_branch(FEATURE_BRANCH, false, None, Some(900)),
        ];

        let (branch_list, mut ctx) =
            init_branch_list_test(repository.into(), branches, test_cx).await;
        let cx = &mut ctx;

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker
                        .delegate
                        .update_matches(NEW_BRANCH.to_string(), window, cx)
                })
            })
            .await;

        cx.run_until_parked();

        branch_list.update_in(cx, |branch_list, window, cx| {
            branch_list.picker.update(cx, |picker, cx| {
                let last_match = picker.delegate.matches.last().unwrap();
                assert!(last_match.is_new_branch());
                assert_eq!(last_match.name(), NEW_BRANCH);
                // State is NewBranch because no existing branches fuzzy-match the query
                assert!(matches!(picker.delegate.state, PickerState::NewBranch));
                picker.delegate.confirm(false, window, cx);
            })
        });
        cx.run_until_parked();

        let branches = branch_list
            .update(cx, |branch_list, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker
                        .delegate
                        .repo
                        .as_ref()
                        .unwrap()
                        .update(cx, |repo, _cx| repo.branches())
                })
            })
            .await
            .unwrap()
            .unwrap();

        let new_branch = branches
            .into_iter()
            .find(|branch| branch.name() == NEW_BRANCH)
            .expect("new-feature-branch should exist");
        assert_eq!(
            new_branch.ref_name.as_ref(),
            &format!("refs/heads/{NEW_BRANCH}"),
            "branch ref_name should not have duplicate refs/heads/ prefix"
        );
    }

    #[gpui::test]
    async fn test_remote_url_detection_https(cx: &mut TestAppContext) {
        init_test(cx);
        let (_project, repository) = init_fake_repository(cx).await;
        let branches = vec![create_test_branch("main", true, None, Some(1000))];

        let (branch_list, mut ctx) = init_branch_list_test(repository.into(), branches, cx).await;
        let cx = &mut ctx;

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    let query = "https://github.com/user/repo.git".to_string();
                    picker.delegate.update_matches(query, window, cx)
                })
            })
            .await;

        cx.run_until_parked();

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    let last_match = picker.delegate.matches.last().unwrap();
                    assert!(last_match.is_new_url());
                    assert!(matches!(picker.delegate.state, PickerState::NewRemote));
                    picker.delegate.confirm(false, window, cx);
                    assert_eq!(picker.delegate.matches.len(), 0);
                    if let PickerState::CreateRemote(remote_url) = &picker.delegate.state
                        && remote_url.as_ref() == "https://github.com/user/repo.git"
                    {
                    } else {
                        panic!("wrong picker state");
                    }
                    picker
                        .delegate
                        .update_matches("my_new_remote".to_string(), window, cx)
                })
            })
            .await;

        cx.run_until_parked();

        branch_list.update_in(cx, |branch_list, window, cx| {
            branch_list.picker.update(cx, |picker, cx| {
                assert_eq!(picker.delegate.matches.len(), 1);
                assert!(matches!(
                    picker.delegate.matches.first(),
                    Some(Entry::NewRemoteName { name, url })
                        if name == "my_new_remote" && url.as_ref() == "https://github.com/user/repo.git"
                ));
                picker.delegate.confirm(false, window, cx);
            })
        });
        cx.run_until_parked();

        // List remotes
        let remotes = branch_list
            .update(cx, |branch_list, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker
                        .delegate
                        .repo
                        .as_ref()
                        .unwrap()
                        .update(cx, |repo, _cx| repo.get_remotes(None, false))
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            remotes,
            vec![Remote {
                name: SharedString::from("my_new_remote")
            }]
        );
    }

    #[gpui::test]
    async fn test_confirm_remote_url_transitions(cx: &mut TestAppContext) {
        init_test(cx);

        let branches = vec![create_test_branch("main_branch", true, None, Some(1000))];
        let (branch_list, mut ctx) = init_branch_list_test(None, branches, cx).await;
        let cx = &mut ctx;

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    let query = "https://github.com/user/repo.git".to_string();
                    picker.delegate.update_matches(query, window, cx)
                })
            })
            .await;
        cx.run_until_parked();

        // Try to create a new remote but cancel in the middle of the process
        branch_list
            .update_in(cx, |branch_list, window, cx| {
                branch_list.picker.update(cx, |picker, cx| {
                    picker.delegate.selected_index = picker.delegate.matches.len() - 1;
                    picker.delegate.confirm(false, window, cx);

                    assert!(matches!(
                        picker.delegate.state,
                        PickerState::CreateRemote(_)
                    ));
                    if let PickerState::CreateRemote(ref url) = picker.delegate.state {
                        assert_eq!(url.as_ref(), "https://github.com/user/repo.git");
                    }
                    assert_eq!(picker.delegate.matches.len(), 0);
                    picker.delegate.dismissed(window, cx);
                    assert!(matches!(picker.delegate.state, PickerState::List));
                    let query = "main".to_string();
                    picker.delegate.update_matches(query, window, cx)
                })
            })
            .await;
        cx.run_until_parked();

        // Try to search a branch again to see if the state is restored properly
        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                // Should have 1 existing branch + 1 "create new branch" entry = 2 total
                assert_eq!(picker.delegate.matches.len(), 2);
                assert!(
                    picker
                        .delegate
                        .matches
                        .iter()
                        .any(|m| m.name() == "main_branch")
                );
                // Verify the last entry is the "create new branch" option
                let last_match = picker.delegate.matches.last().unwrap();
                assert!(last_match.is_new_branch());
            })
        });
    }

    #[gpui::test]
    async fn test_confirm_remote_url_does_not_dismiss(cx: &mut TestAppContext) {
        const REMOTE_URL: &str = "https://github.com/user/repo.git";

        init_test(cx);
        let branches = vec![create_test_branch("main", true, None, Some(1000))];

        let (branch_list, mut ctx) = init_branch_list_test(None, branches, cx).await;
        let cx = &mut ctx;

        let subscription = cx.update(|_, cx| {
            cx.subscribe(&branch_list, |_, _: &DismissEvent, _| {
                panic!("DismissEvent should not be emitted when confirming a remote URL");
            })
        });

        branch_list
            .update_in(cx, |branch_list, window, cx| {
                window.focus(&branch_list.picker_focus_handle, cx);
                assert!(
                    branch_list.picker_focus_handle.is_focused(window),
                    "Branch picker should be focused when selecting an entry"
                );

                branch_list.picker.update(cx, |picker, cx| {
                    picker
                        .delegate
                        .update_matches(REMOTE_URL.to_string(), window, cx)
                })
            })
            .await;

        cx.run_until_parked();

        branch_list.update_in(cx, |branch_list, window, cx| {
            // Re-focus the picker since workspace initialization during run_until_parked
            window.focus(&branch_list.picker_focus_handle, cx);

            branch_list.picker.update(cx, |picker, cx| {
                let last_match = picker.delegate.matches.last().unwrap();
                assert!(last_match.is_new_url());
                assert!(matches!(picker.delegate.state, PickerState::NewRemote));

                picker.delegate.confirm(false, window, cx);

                assert!(
                    matches!(picker.delegate.state, PickerState::CreateRemote(ref url) if url.as_ref() == REMOTE_URL),
                    "State should transition to CreateRemote with the URL"
                );
            });

            assert!(
                branch_list.picker_focus_handle.is_focused(window),
                "Branch list picker should still be focused after confirming remote URL"
            );
        });

        cx.run_until_parked();

        drop(subscription);
    }

    #[gpui::test(iterations = 10)]
    async fn test_empty_query_displays_all_branches(mut rng: StdRng, cx: &mut TestAppContext) {
        init_test(cx);
        let branch_count = rng.random_range(13..540);

        let branches: Vec<Branch> = (0..branch_count)
            .map(|i| create_test_branch(&format!("branch-{:02}", i), i == 0, None, Some(i * 100)))
            .collect();

        let (branch_list, mut ctx) = init_branch_list_test(None, branches, cx).await;
        let cx = &mut ctx;

        update_branch_list_matches_with_empty_query(&branch_list, cx).await;

        branch_list.update(cx, |branch_list, cx| {
            branch_list.picker.update(cx, |picker, _cx| {
                assert_eq!(picker.delegate.matches.len(), branch_count as usize);
            })
        });
    }
}
