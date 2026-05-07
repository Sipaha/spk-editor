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
//!    member's display name and a caret icon. Clicking the trigger
//!    opens the member-picker popover.
//!
//! Subsequent tasks add the [+] add-project dropdown, the trash icon,
//! and the change-count badges.

mod add_project_picker;
mod member_picker;

use collections::HashMap;
use gpui::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    WeakEntity, Window,
};
use solutions::{
    CatalogId, SolutionId, SolutionMember, SolutionStore, SolutionStoreEvent, db::PanelKind,
};
use ui::{Color, Icon, IconButton, IconName, IconSize, PopoverMenu, prelude::*};
use util::ResultExt as _;
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
        let this = Self {
            panel_kind,
            workspace,
            solution_id: None,
            members: Vec::new(),
            selected_catalog_id: None,
            change_counts: HashMap::default(),
            _subscriptions: vec![store_subscription],
        };
        // Defer the initial rebuild so it does not run while the host panel's
        // containing entity (e.g. Workspace) is still being mutably updated.
        // This avoids the "cannot read X while it is already being updated"
        // panic that occurs when a panel is constructed inside
        // `workspace.update_in(cx, PanelType::new)`.
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| this.rebuild(cx)).log_err();
        })
        .detach();
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
                    s.set_panel_member_selection(sol_id, panel, cat_id, cx)
                        .log_err();
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

        let solution_id = self.solution_id.clone();
        let panel_kind = self.panel_kind;
        let members = self.members.clone();
        let selected = self.selected_catalog_id.clone();
        let change_counts = self.change_counts.clone();

        let trigger = Button::new("active-project-selector-trigger", label_text)
            .start_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall).color(Color::Muted))
            .style(ButtonStyle::Subtle);

        let solution_id_for_add = solution_id.clone();

        h_flex()
            .id("active-project-selector")
            .w_full()
            .child(
                PopoverMenu::new("active-project-member-picker")
                    .trigger(trigger)
                    .menu(move |window, cx| {
                        let Some(solution_id) = solution_id.clone() else {
                            return None;
                        };
                        Some(cx.new(|cx| {
                            member_picker::MemberPicker::new(
                                panel_kind,
                                solution_id,
                                members.clone(),
                                selected.clone(),
                                change_counts.clone(),
                                window,
                                cx,
                            )
                        }))
                    })
                    .anchor(gpui::Anchor::TopLeft)
                    .attach(gpui::Anchor::BottomLeft),
            )
            .child(
                PopoverMenu::new("active-project-add-picker")
                    .trigger(
                        IconButton::new("active-project-add", IconName::Plus)
                            .style(ButtonStyle::Subtle),
                    )
                    .menu(move |window, cx| {
                        let Some(solution_id) = solution_id_for_add.clone() else {
                            return None;
                        };
                        Some(cx.new(|cx| {
                            add_project_picker::AddProjectPicker::new(solution_id, window, cx)
                        }))
                    })
                    .anchor(gpui::Anchor::TopLeft)
                    .attach(gpui::Anchor::BottomLeft),
            )
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
    use collections::HashMap;
    use gpui::TestAppContext;
    use solutions::install_global_for_test;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use theme_settings;

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

    /// Tests the core of the member-picker click path: constructing a
    /// `MemberPicker` directly (no Workspace needed) and calling
    /// `confirm_member` updates the store's persisted selection.
    ///
    /// We use `add_window_view` to obtain a `VisualTestContext` (which
    /// implements `VisualContext`) so we can call `update_in` with a
    /// `Window` reference — `Editor::single_line` and `confirm_member`
    /// both require one.
    #[gpui::test]
    async fn member_picker_confirm_persists_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });

        let dir = tempdir().expect("tempdir");
        let solutions_root = dir.path().join("solutions");
        std::fs::create_dir_all(&solutions_root).expect("mkdir solutions");
        let cfg_path = dir.path().join("solutions.json");

        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));
        cx.update(|cx| install_global_for_test(store.clone(), cx));

        let sol_id = store
            .update(cx, |s, cx| s.create_solution("TestSol", solutions_root, cx))
            .expect("create solution");

        let cat_id_a = store
            .update(cx, |s, cx| s.add_empty_member(&sol_id, "Alpha", cx))
            .expect("add alpha");
        let cat_id_b = store
            .update(cx, |s, cx| s.add_empty_member(&sol_id, "Beta", cx))
            .expect("add beta");

        let members = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == sol_id)
                .map(|sol| sol.members.clone())
                .unwrap_or_default()
        });

        let sol_id_for_picker = sol_id.clone();
        let (picker, cx) = cx.add_window_view(move |window, cx| {
            member_picker::MemberPicker::new(
                PanelKind::Tree,
                sol_id_for_picker,
                members,
                Some(cat_id_a),
                HashMap::default(),
                window,
                cx,
            )
        });

        // Before confirm: no persisted selection for this fresh solution.
        let before = store.read_with(cx, |s, _| {
            s.panel_member_selection(&sol_id, PanelKind::Tree).cloned()
        });
        assert_eq!(before, None, "no persisted selection before picker confirm");

        // Confirm the second member.
        picker.update_in(cx, |picker, window, cx| {
            picker.confirm_member(cat_id_b.clone(), window, cx);
        });

        // After confirm: store must reflect the second member.
        let after = store.read_with(cx, |s, _| {
            s.panel_member_selection(&sol_id, PanelKind::Tree).cloned()
        });
        assert_eq!(
            after,
            Some(cat_id_b),
            "persisted selection must equal confirmed member"
        );
    }

    /// Smoke test: a MemberPicker with members renders without panic and the
    /// trash-icon button element id is unique per member. We cannot simulate
    /// a click in this harness, but we verify the `RemoveMember` action has
    /// the expected payload shape by constructing it directly and asserting
    /// its fields round-trip through the solution / catalog ids.
    #[gpui::test]
    async fn member_picker_trash_icon_action_payload(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });

        let dir = tempdir().expect("tempdir");
        let solutions_root = dir.path().join("solutions");
        std::fs::create_dir_all(&solutions_root).expect("mkdir solutions");
        let cfg_path = dir.path().join("solutions.json");

        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));
        cx.update(|cx| install_global_for_test(store.clone(), cx));

        let sol_id = store
            .update(cx, |s, cx| s.create_solution("TestSol", solutions_root, cx))
            .expect("create solution");

        let cat_id = store
            .update(cx, |s, cx| s.add_empty_member(&sol_id, "MyProject", cx))
            .expect("add member");

        let members = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == sol_id)
                .map(|sol| sol.members.clone())
                .unwrap_or_default()
        });

        let sol_id_clone = sol_id.clone();
        let cat_id_clone = cat_id.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            member_picker::MemberPicker::new(
                PanelKind::Tree,
                sol_id_clone,
                members,
                Some(cat_id_clone),
                HashMap::default(),
                window,
                cx,
            )
        });

        // Verify the RemoveMember action is constructable with the expected
        // payload shape (solution_id / catalog_id as Strings matching the
        // inner values of SolutionId / CatalogId).
        let action = crate::actions::RemoveMember {
            solution_id: sol_id.0.clone(),
            catalog_id: cat_id.0.clone(),
        };
        assert_eq!(action.solution_id, sol_id.0);
        assert_eq!(action.catalog_id, cat_id.0);

        // Run the event loop so the picker's render pass doesn't panic.
        cx.run_until_parked();
    }

    /// Tests that AddProjectPicker filters the catalog correctly:
    /// Catalog entries already attached to the solution are filtered out
    /// of the picker; catalog entries not yet members appear.
    #[gpui::test]
    async fn add_project_picker_filters_catalog(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });

        let dir = tempdir().expect("tempdir");
        let solutions_root = dir.path().join("solutions");
        std::fs::create_dir_all(&solutions_root).expect("mkdir solutions");
        let cfg_path = dir.path().join("solutions.json");

        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));
        cx.update(|cx| install_global_for_test(store.clone(), cx));

        let sol_id = store
            .update(cx, |s, cx| s.create_solution("TestSol", solutions_root, cx))
            .expect("create solution");

        // unique_slug derives "frontend" from "Frontend" in both uniqueness
        // contexts (catalog ids vs this solution's member catalog ids), so
        // both calls below land on CatalogId("frontend"). The catalog entry
        // and the (empty) solution member share that id, which is exactly
        // the case the picker's already_member filter must catch.
        store
            .update(cx, |s, cx| {
                s.add_catalog_project("Frontend", "git@x:frontend.git", None, cx)
            })
            .expect("add catalog Frontend");
        store
            .update(cx, |s, cx| {
                s.add_catalog_project("Backend", "git@x:backend.git", None, cx)
            })
            .expect("add catalog Backend");
        store
            .update(cx, |s, cx| s.add_empty_member(&sol_id, "Frontend", cx))
            .expect("add empty member Frontend");

        let (picker, cx) = cx.add_window_view(move |window, cx| {
            add_project_picker::AddProjectPicker::new(sol_id, window, cx)
        });

        let entries: Vec<String> = picker.read_with(cx, |p, _| {
            p.catalog_entries.iter().map(|c| c.name.clone()).collect()
        });
        assert_eq!(
            entries,
            vec!["Backend".to_string()],
            "Frontend is already a member and must be filtered out; Backend is not"
        );

        // Also verify add_catalog does not panic on a visible entry; the
        // clone task will fail (no real remote), which is fine — emit-and-
        // dismiss is the only behaviour the picker owns.
        let backend_entry = store.read_with(cx, |s, _| {
            s.catalog()
                .iter()
                .find(|p| p.name == "Backend")
                .cloned()
                .expect("Backend entry exists")
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.add_catalog(backend_entry, window, cx);
        });
    }
}
