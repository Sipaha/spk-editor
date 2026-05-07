//! Per-panel "active member" picker for the project_panel and git_panel.
//!
//! Each panel owns one Entity<ActiveProjectSelector>. The selector
//! subscribes to SolutionStore + observes the workspace's project so
//! it knows when the active solution flips, when membership changes,
//! and when the panel's persisted selection moves (cross-window
//! sync).
//!
//! Public surface (current task):
//!  - struct ActiveProjectSelector { ... }
//!  - pub fn new(panel_kind, workspace, cx) -> Self
//!  - pub fn selected_catalog_id(&self) -> Option<&CatalogId>
//!  - pub fn set_change_counts(&mut self, ..., cx) — for git_panel
//!  - impl Render — renders a trigger button with the selected
//!    member's display name and a caret icon.
//!
//! Subsequent tasks add the member-picker popover, the [+] add-project
//! dropdown, the trash icon, and the change-count badges.

use collections::HashMap;
use gpui::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    WeakEntity, Window,
};
use solutions::{
    CatalogId, SolutionId, SolutionMember, SolutionStore, SolutionStoreEvent, db::PanelKind,
};
use ui::{Color, Icon, IconName, Label, prelude::*};
use workspace::Workspace;

use crate::window_helpers::active_solution_in_workspace;

pub struct ActiveProjectSelector {
    panel_kind: PanelKind,
    workspace: WeakEntity<Workspace>,
    /// `None` when no solution is hosted by the workspace's worktrees
    /// — shown as "No solution" disabled trigger.
    solution_id: Option<SolutionId>,
    /// Cached members of `solution_id` at the time of the last
    /// rebuild. Rebuilt whenever SolutionStore emits Changed.
    members: Vec<SolutionMember>,
    /// `None` if the solution has no members; otherwise points at the
    /// catalog id stored in `panel_member_selections` (or the first
    /// member as fallback after the initial-selection rules apply).
    selected_catalog_id: Option<CatalogId>,
    /// git_panel-only: per-member changed-file count, set by the host
    /// panel via `set_change_counts`. Empty by default.
    change_counts: HashMap<CatalogId, usize>,
    _subscriptions: Vec<Subscription>,
}

impl ActiveProjectSelector {
    pub fn new(
        panel_kind: PanelKind,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SolutionStore::global(cx);
        let store_subscription = cx.subscribe(&store, |this, _, event, cx| match event {
            SolutionStoreEvent::Changed => this.rebuild(cx),
            SolutionStoreEvent::ActiveSolutionChanged(_) => this.rebuild(cx),
            SolutionStoreEvent::PanelMemberSelectionChanged {
                solution,
                panel,
                catalog,
            } => {
                if Some(solution) == this.solution_id.as_ref() && *panel == this.panel_kind {
                    this.selected_catalog_id = Some(catalog.clone());
                    cx.notify();
                }
            }
            _ => {}
        });
        let mut this = Self {
            panel_kind,
            workspace,
            solution_id: None,
            members: Vec::new(),
            selected_catalog_id: None,
            change_counts: HashMap::default(),
            _subscriptions: vec![store_subscription],
        };
        this.rebuild(cx);
        this
    }

    pub fn selected_catalog_id(&self) -> Option<&CatalogId> {
        self.selected_catalog_id.as_ref()
    }

    pub fn selected_member(&self) -> Option<&SolutionMember> {
        let cat = self.selected_catalog_id.as_ref()?;
        self.members.iter().find(|m| m.catalog_id == *cat)
    }

    pub fn set_change_counts(
        &mut self,
        counts: HashMap<CatalogId, usize>,
        cx: &mut Context<Self>,
    ) {
        if self.change_counts != counts {
            self.change_counts = counts;
            cx.notify();
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.upgrade();
        let new_solution_id = workspace
            .as_ref()
            .and_then(|ws| active_solution_in_workspace(ws.read(cx), cx));
        self.solution_id = new_solution_id.clone();

        let store = SolutionStore::global(cx);
        let (members, persisted): (Vec<SolutionMember>, Option<CatalogId>) =
            if let Some(sol_id) = &new_solution_id {
                store.read_with(cx, |s, _| {
                    let members = s
                        .solutions()
                        .iter()
                        .find(|sol| sol.id == *sol_id)
                        .map(|sol| sol.members.clone())
                        .unwrap_or_default();
                    let persisted = s
                        .panel_member_selection(sol_id, self.panel_kind)
                        .cloned();
                    (members, persisted)
                })
            } else {
                (Vec::new(), None)
            };

        self.members = members;
        self.selected_catalog_id = persisted
            .or_else(|| self.members.first().map(|m| m.catalog_id.clone()));

        // Persist initial-selection default: per spec, "first time a
        // solution is loaded for a given panel … select the first
        // member; persist immediately." The DB write reconciles the
        // cache so cross-window sync works on first load.
        if let (Some(sol_id), Some(cat_id)) =
            (self.solution_id.clone(), self.selected_catalog_id.clone())
        {
            // Only persist if the cache has no row for this pair, so we
            // don't churn writes on every rebuild.
            let needs_persist = SolutionStore::global(cx).read_with(cx, |s, _| {
                s.panel_member_selection(&sol_id, self.panel_kind).is_none()
            });
            if needs_persist {
                let panel = self.panel_kind;
                SolutionStore::global(cx).update(cx, |s, cx| {
                    let _ = s.set_panel_member_selection(sol_id, panel, cat_id, cx);
                });
            }
        }

        cx.notify();
    }
}

impl Render for ActiveProjectSelector {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = SolutionStore::global(cx);
        let label_text: SharedString = match self.selected_member() {
            Some(m) => {
                let name = store.read_with(cx, |s, _| member_display_name(m, s));
                SharedString::from(name)
            }
            None if self.solution_id.is_none() => "No solution".into(),
            None => "No project".into(),
        };
        h_flex()
            .id("active-project-selector")
            .w_full()
            .gap_1()
            .px_2()
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
            )
            .child(Label::new(label_text).size(LabelSize::Default))
    }
}

/// Display name for a member: catalog name when the catalog row exists,
/// otherwise the path's last segment (the spec's orphan rendering rule,
/// also applied to fresh empty members which never have a catalog row).
pub(crate) fn member_display_name(m: &SolutionMember, store: &SolutionStore) -> String {
    if let Some(c) = store.catalog().iter().find(|c| c.id == m.catalog_id) {
        return c.name.clone();
    }
    m.local_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| m.catalog_id.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::path::PathBuf;

    #[gpui::test]
    async fn member_display_name_uses_catalog_when_present(cx: &mut TestAppContext) {
        let store = cx.update(|cx| SolutionStore::for_test(PathBuf::new(), cx));
        let cat_id = store
            .update(cx, |s, cx| {
                s.add_catalog_project("Frontend", "git@x:f.git", None, cx)
            })
            .expect("add catalog");
        let member = SolutionMember {
            catalog_id: cat_id,
            local_path: PathBuf::from("/tmp/sol/some-slug"),
        };
        let name = store.read_with(cx, |s, _| member_display_name(&member, s));
        assert_eq!(name, "Frontend");
    }

    #[gpui::test]
    async fn member_display_name_falls_back_to_path_segment(cx: &mut TestAppContext) {
        let store = cx.update(|cx| SolutionStore::for_test(PathBuf::new(), cx));
        let m = SolutionMember {
            catalog_id: CatalogId("orphan".into()),
            local_path: PathBuf::from("/tmp/sol/backend"),
        };
        let name = store.read_with(cx, |s, _| member_display_name(&m, s));
        assert_eq!(name, "backend");
    }
}
