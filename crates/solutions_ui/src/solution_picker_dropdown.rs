//! Popover behind the title-bar `+` button.
//!
//! Lists solutions in the catalog that are not currently open in any
//! window (sorted by `last_opened_at` desc, nulls last). Has a leading
//! "Create new solution…" row and a trash icon on each row that opens
//! [`crate::delete_confirm_modal::DeleteConfirmModal`]. The search input
//! is autofocused on open and filters rows case-insensitively as the
//! user types.
//!
//! Wired into the title-bar by Task 7 (`SolutionTabStrip`); kept in its
//! own modal-style entity so the strip can `toggle_modal` it without
//! rebuilding the picker on every rerender.

use editor::{Editor, EditorEvent};
use gpui::{
    DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Subscription, WeakEntity, px,
};
use solutions::{Solution, SolutionId, SolutionStore, SolutionStoreEvent};
use std::path::PathBuf;
use ui::{IconButtonShape, Tooltip, prelude::*};
use workspace::{ModalView, MultiWorkspace, Workspace};

use crate::delete_confirm_modal::{DeleteConfirmItem, open_delete_confirm};
use crate::open::{OpenIntent, open_solution};
use crate::window_helpers::is_solution_open_anywhere;

pub struct SolutionPickerDropdown {
    workspace: WeakEntity<Workspace>,
    multi_workspace: WeakEntity<MultiWorkspace>,
    search_editor: Entity<Editor>,
    closed_solutions: Vec<ClosedSolutionRow>,
    _store_subscription: Subscription,
    _search_subscription: Subscription,
}

#[derive(Clone)]
struct ClosedSolutionRow {
    id: SolutionId,
    name: SharedString,
    root: PathBuf,
}

impl SolutionPickerDropdown {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        multi_workspace: WeakEntity<MultiWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search…", window, cx);
            editor
        });

        // Re-render whenever the search query changes — the filter is
        // applied in-place on the `closed_solutions` snapshot so the
        // store subscription doesn't have to rerun for every keystroke.
        let search_subscription = cx.subscribe(
            &search_editor,
            |_, _, event: &EditorEvent, cx| {
                if matches!(
                    event,
                    EditorEvent::Edited { .. } | EditorEvent::BufferEdited
                ) {
                    cx.notify();
                }
            },
        );

        // Refresh the closed-solutions list whenever the store mutates
        // (solutions added / removed / renamed, or members changing in a
        // way that flips a solution's open-anywhere status).
        let store = SolutionStore::global(cx);
        let store_subscription = cx.subscribe(
            &store,
            |this, _, _event: &SolutionStoreEvent, cx| {
                this.refresh(cx);
            },
        );

        let mut this = Self {
            workspace,
            multi_workspace,
            search_editor,
            closed_solutions: Vec::new(),
            _store_subscription: store_subscription,
            _search_subscription: search_subscription,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        // `is_solution_open_anywhere` skips the window currently on the
        // stack, so solutions only-open-in-our-window slip through. Build
        // an explicit "open in this window's MW" set from the source MW
        // handle and exclude those too.
        let open_in_this_window: std::collections::HashSet<SolutionId> = self
            .multi_workspace
            .upgrade()
            .map(|mw| {
                mw.read(cx)
                    .workspaces()
                    .filter_map(|ws| {
                        let store = SolutionStore::try_global(cx)?;
                        let store = store.read(cx);
                        ws.read(cx)
                            .project()
                            .read(cx)
                            .worktrees(cx)
                            .find_map(|tree| {
                                store
                                    .solution_for_path(&tree.read(cx).abs_path())
                                    .map(|sol| sol.id.clone())
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let store = SolutionStore::global(cx);
        let mut rows: Vec<(Option<chrono::DateTime<chrono::Utc>>, ClosedSolutionRow)> = store
            .read_with(cx, |s, _| {
                s.solutions()
                    .iter()
                    .filter(|sol: &&Solution| {
                        !is_solution_open_anywhere(&sol.id, cx)
                            && !open_in_this_window.contains(&sol.id)
                    })
                    .map(|sol| {
                        (
                            sol.last_opened_at,
                            ClosedSolutionRow {
                                id: sol.id.clone(),
                                name: SharedString::from(sol.name.clone()),
                                root: sol.root.clone(),
                            },
                        )
                    })
                    .collect()
            });
        // Most-recently-opened first; never-opened solutions go last in
        // their natural store order. Mirrors `welcome::all_solutions`
        // so the dropdown's row order matches what the user already
        // sees on the launcher.
        rows.sort_by(|a, b| match (a.0, b.0) {
            (Some(ts_a), Some(ts_b)) => ts_b.cmp(&ts_a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        self.closed_solutions = rows.into_iter().map(|(_, row)| row).collect();
        cx.notify();
    }

    fn filter_query(&self, cx: &App) -> String {
        self.search_editor
            .read(cx)
            .text(cx)
            .trim()
            .to_lowercase()
    }

    fn filtered_rows<'a>(&'a self, cx: &App) -> Vec<&'a ClosedSolutionRow> {
        let query = self.filter_query(cx);
        if query.is_empty() {
            self.closed_solutions.iter().collect()
        } else {
            self.closed_solutions
                .iter()
                .filter(|row| row.name.to_lowercase().contains(&query))
                .collect()
        }
    }

    fn open_create(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        cx.dispatch_action(&crate::actions::NewSolution);
    }

    fn open_row(&mut self, id: SolutionId, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        let source = window.window_handle().downcast();
        open_solution(id, source, OpenIntent::SameWindow, cx);
    }

    fn ask_delete(
        &mut self,
        row: ClosedSolutionRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dismiss the dropdown first — the confirm modal toggles on the
        // workspace's modal layer, and leaving this picker mounted while
        // a confirm modal opens above it stacks two modals on the same
        // layer. Dispatching through the `DeleteSolutionFromTabBar`
        // action handler would do the same modal but force us to keep
        // the dropdown around long enough for the action to fire; calling
        // `open_delete_confirm` directly lets us emit `DismissEvent`
        // first.
        cx.emit(DismissEvent);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let ClosedSolutionRow { id, name, root } = row;
        workspace.update(cx, |workspace, cx| {
            let folder_label = SharedString::from(format!("Folder {}", root.display()));
            open_delete_confirm(
                workspace,
                SharedString::from(format!("Delete solution \"{name}\"?")),
                "This will permanently delete:",
                vec![
                    DeleteConfirmItem {
                        label: "Registry entry".into(),
                        path: None,
                    },
                    DeleteConfirmItem {
                        label: folder_label,
                        path: Some(root),
                    },
                ],
                move |_window, cx| {
                    cx.dispatch_action(&crate::actions::DeleteSolution { id: id.0 });
                },
                window,
                cx,
            );
        });
    }
}

impl ModalView for SolutionPickerDropdown {
    fn debug_kind(&self) -> &'static str {
        "SolutionPickerDropdown"
    }
}

impl EventEmitter<DismissEvent> for SolutionPickerDropdown {}

impl Focusable for SolutionPickerDropdown {
    // Hand the search editor's focus handle out so the modal layer can
    // park focus on it on open — that's the autofocus contract.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search_editor.focus_handle(cx)
    }
}

impl Render for SolutionPickerDropdown {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<ClosedSolutionRow> =
            self.filtered_rows(cx).into_iter().cloned().collect();
        let row_count = rows.len();

        let row_elements: Vec<gpui::AnyElement> = rows
            .into_iter()
            .map(|row| {
                let row_id = SharedString::from(format!(
                    "solution-picker-row-{}",
                    row.id.as_str()
                ));
                let group_id = SharedString::from(format!(
                    "solution-picker-group-{}",
                    row.id.as_str()
                ));
                let trash_id = SharedString::from(format!(
                    "solution-picker-delete-{}",
                    row.id.as_str()
                ));
                let id_for_open = row.id.clone();
                let label = row.name.clone();
                h_flex()
                    .id(row_id)
                    .group(group_id.clone())
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_row(id_for_open.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(label).truncate()),
                    )
                    .child(
                        IconButton::new(trash_id, IconName::Trash)
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .visible_on_hover(group_id)
                            .tooltip(Tooltip::text("Delete solution"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask_delete(row.clone(), window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .chain((row_count == 0).then(|| {
                div()
                    .px_2()
                    .py_2()
                    .child(
                        Label::new(if self.closed_solutions.is_empty() {
                            "No closed solutions"
                        } else {
                            "No matches"
                        })
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                    )
                    .into_any_element()
            }))
            .collect();

        v_flex()
            .key_context("SolutionPickerDropdown")
            .track_focus(&self.search_editor.focus_handle(cx))
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| {
                cx.emit(DismissEvent);
            }))
            .min_w(px(280.0))
            .max_h(px(360.0))
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .child(div().p_2().child(self.search_editor.clone()))
            .child(
                div()
                    .h_px()
                    .bg(cx.theme().colors().border),
            )
            .child(
                h_flex()
                    .id("solution-picker-create")
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                    .child(
                        Icon::new(IconName::Plus)
                            .size(IconSize::Small)
                            .color(Color::Accent),
                    )
                    .child(
                        Label::new("Create new solution…").color(Color::Accent),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create(window, cx);
                    })),
            )
            .child(
                div()
                    .h_px()
                    .bg(cx.theme().colors().border),
            )
            .child(
                div()
                    .id("solution-picker-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(row_elements),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    /// Mirrors the sort logic inside `refresh()` so we can validate it in
    /// isolation. `(last_opened, name)` pairs in / `name`s in expected
    /// order out.
    fn sort_rows(mut rows: Vec<(Option<chrono::DateTime<chrono::Utc>>, &'static str)>) -> Vec<&'static str> {
        rows.sort_by(|a, b| match (a.0, b.0) {
            (Some(ts_a), Some(ts_b)) => ts_b.cmp(&ts_a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        rows.into_iter().map(|(_, name)| name).collect()
    }

    #[test]
    fn closed_solutions_sort_by_last_opened_desc_with_nulls_last() {
        let now = Utc.with_ymd_and_hms(2024, 1, 10, 12, 0, 0).unwrap();
        let earlier = now - Duration::hours(1);
        let earliest = now - Duration::days(1);
        let order = sort_rows(vec![
            (Some(earlier), "b-middle"),
            (None, "d-never-1"),
            (Some(now), "a-newest"),
            (None, "e-never-2"),
            (Some(earliest), "c-oldest"),
        ]);
        assert_eq!(
            order,
            vec!["a-newest", "b-middle", "c-oldest", "d-never-1", "e-never-2"]
        );
    }

    #[test]
    fn filter_matches_substring_case_insensitive() {
        let rows = [
            ClosedSolutionRow {
                id: SolutionId("1".into()),
                name: "Citeck Core".into(),
                root: PathBuf::from("/x/1"),
            },
            ClosedSolutionRow {
                id: SolutionId("2".into()),
                name: "ECOS Records".into(),
                root: PathBuf::from("/x/2"),
            },
            ClosedSolutionRow {
                id: SolutionId("3".into()),
                name: "spk-editor".into(),
                root: PathBuf::from("/x/3"),
            },
        ];
        let query = "ECOS".to_lowercase();
        let matched: Vec<&str> = rows
            .iter()
            .filter(|r| r.name.to_lowercase().contains(&query))
            .map(|r| r.name.as_ref())
            .collect();
        assert_eq!(matched, vec!["ECOS Records"]);

        let query = "ed".to_lowercase();
        let matched: Vec<&str> = rows
            .iter()
            .filter(|r| r.name.to_lowercase().contains(&query))
            .map(|r| r.name.as_ref())
            .collect();
        assert_eq!(matched, vec!["spk-editor"]);
    }
}
