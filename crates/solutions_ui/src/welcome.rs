//! Welcome page integration: renders a Solutions section (full list + Create
//! button) by plugging into `workspace::register_welcome_section`.
//!
//! Lives here (not in `workspace`) so the dependency graph stays one-way:
//! `solutions_ui → workspace`, never the reverse. Earlier we tried to read
//! `SolutionStore` directly from `workspace::welcome` and that introduced
//! the `workspace ↔ solutions` cycle that had to be reverted.
//!
//! Combined with `restore_on_startup: "none"` in `assets/settings/default.json`,
//! this section is what the user sees on every fresh launch — it's the
//! Solutions launcher for the whole editor. See FORK.md §11.

use anyhow::anyhow;
use chrono::{DateTime, Utc};
use gpui::{Action, AnyElement, AnyWindowHandle, App, IntoElement};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use solutions::{SolutionId, SolutionStore, SolutionStoreEvent};
use ui::prelude::*;
use ui::{ButtonLike, Divider, DividerColor};
use util::ResultExt as _;
use workspace::{
    AppState, OpenOptions,
    welcome::{WelcomePage, register_welcome_section},
};

use crate::actions::NewSolution;

#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = solutions)]
#[serde(transparent)]
pub struct OpenRecentSolution {
    pub index: usize,
}

/// Wires the Recent Solutions section onto the welcome page and the
/// `OpenRecentSolution` action handler. Called once from `solutions_ui::init`.
pub fn init(cx: &mut App) {
    register_welcome_section(cx, render_section);
    cx.on_action(open_recent_solution);

    // WelcomePage doesn't know about SolutionStore on its own, so without
    // this hook the Recent Solutions section would render once at page
    // construction and then stay frozen. We subscribe each new WelcomePage
    // to SolutionStoreEvent::Changed and call `cx.notify` to re-run the
    // section renderer after `solutions.open` / `delete` / `touch_last_opened`.
    cx.observe_new::<WelcomePage>(|_page, _window, cx| {
        let Some(store) = SolutionStore::try_global(cx) else {
            return;
        };
        cx.subscribe(&store, |_page, _store, _event: &SolutionStoreEvent, cx| {
            cx.notify();
        })
        .detach();
    })
    .detach();
}

fn render_section(cx: &mut App) -> Option<AnyElement> {
    let entries = all_solutions(cx);
    let mut list = ui::v_flex().w_full().gap_1();
    list = list.child(
        ui::h_flex()
            .px_1()
            .mb_2()
            .gap_2()
            .child(
                Label::new("SOLUTIONS")
                    .buffer_font(cx)
                    .color(Color::Muted)
                    .size(LabelSize::XSmall),
            )
            .child(Divider::horizontal().color(DividerColor::BorderVariant)),
    );
    if entries.is_empty() {
        list = list.child(
            ui::v_flex()
                .px_1()
                .py_2()
                .child(
                    Label::new("No solutions yet.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                ),
        );
    } else {
        for (index, entry) in entries.into_iter().enumerate() {
            list = list.child(render_row(index, entry));
        }
    }
    list = list.child(render_create_button());
    Some(list.into_any_element())
}

fn render_create_button() -> impl IntoElement {
    ButtonLike::new("create-solution-from-welcome")
        .full_width()
        .size(ui::ButtonSize::Medium)
        .child(
            ui::h_flex()
                .gap_2()
                .child(
                    Icon::new(IconName::Plus)
                        .color(Color::Muted)
                        .size(IconSize::Small),
                )
                .child(Label::new("Create new solution")),
        )
        .on_click(|_, window, cx| {
            window.dispatch_action(Box::new(NewSolution), cx);
        })
}

fn render_row(index: usize, entry: RecentSolution) -> impl IntoElement {
    let entry_id = entry.id.clone();
    let mut row = ui::h_flex()
        .gap_2()
        .child(
            Icon::new(IconName::Folder)
                .color(Color::Muted)
                .size(IconSize::Small),
        )
        .child(Label::new(entry.label));
    if entry.is_empty {
        row = row.child(
            Label::new("(empty)")
                .color(Color::Muted)
                .size(LabelSize::XSmall),
        );
    }
    ButtonLike::new(("recent-solution", index))
        .full_width()
        .size(ui::ButtonSize::Medium)
        .child(row)
        .on_click(move |_, window, cx| {
            // Pass the welcome window as the source so it gets closed once the
            // new solution window is up — otherwise the launcher stays around
            // as an orphan tab the user has to dismiss manually.
            let source = window.window_handle();
            open_solution(entry_id.clone(), Some(source), cx);
        })
}

fn open_recent_solution(action: &OpenRecentSolution, cx: &mut App) {
    let entries = all_solutions(cx);
    let Some(entry) = entries.get(action.index) else {
        return;
    };
    // Keybind path: no source window. We can't tell whether the trigger came
    // from a welcome window or some other workspace, and closing the wrong one
    // would be very surprising.
    open_solution(entry.id.clone(), None, cx);
}

fn open_solution(sol_id: SolutionId, source_window: Option<AnyWindowHandle>, cx: &mut App) {
    let Some(store) = SolutionStore::try_global(cx) else {
        return;
    };
    // For an empty solution we still want to open a window — just one with
    // `solution.root` as the only worktree. The dock panel detects the
    // solution from that path and lets the user add members from inside.
    let paths = match store.read_with(cx, |s, _| -> anyhow::Result<Vec<std::path::PathBuf>> {
        let paths = s.paths_for_open(&sol_id)?;
        if !paths.is_empty() {
            return Ok(paths);
        }
        let root = s
            .solutions()
            .iter()
            .find(|sol| sol.id == sol_id)
            .map(|sol| sol.root.clone())
            .ok_or_else(|| anyhow!("solution not found: {}", sol_id.0))?;
        Ok(vec![root])
    }) {
        Ok(paths) => paths,
        Err(err) => {
            log::error!("solutions_ui: resolving paths for {} failed: {err}", sol_id.0);
            return;
        }
    };
    store
        .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
        .log_err();
    let app_state = AppState::global(cx);
    let mut options = OpenOptions::default();
    options.open_mode = workspace::OpenMode::NewWindow;
    let task = workspace::open_paths(&paths, app_state, options, cx);
    cx.spawn(async move |cx| {
        let Some(opened) = task.await.log_err() else {
            return;
        };
        let Some(source) = source_window else {
            return;
        };
        // Defensive: only close if it's not the same window we just opened.
        // open_paths with NewWindow always creates a new one today, but a
        // future change that reuses a window would otherwise close the
        // window the user just opened.
        if source.window_id() == opened.window.window_id() {
            return;
        }
        cx.update(|cx| {
            source
                .update(cx, |_, window, _| window.remove_window())
                .log_err();
        });
    })
    .detach();
}

#[cfg_attr(test, derive(Debug))]
struct RecentSolution {
    id: SolutionId,
    label: String,
    is_empty: bool,
}

#[cfg(test)]
fn all_solutions_for_test(cx: &App) -> Vec<RecentSolution> {
    all_solutions(cx)
}

/// Returns every solution in the store, sorted by `last_opened_at` desc with
/// never-opened solutions placed last (kept in their natural store order).
/// No truncation — the Welcome page is the launcher for the whole editor and
/// the user expects to see all of their solutions, not just five.
fn all_solutions(cx: &App) -> Vec<RecentSolution> {
    let Some(store) = SolutionStore::try_global(cx) else {
        return Vec::new();
    };
    let mut sols: Vec<(SolutionId, String, Option<DateTime<Utc>>, bool)> = store
        .read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .map(|sol| {
                    (
                        sol.id.clone(),
                        sol.name.clone(),
                        sol.last_opened_at,
                        sol.members.is_empty(),
                    )
                })
                .collect()
        });
    // Opened solutions first, ordered by last_opened_at desc (newest first).
    // Never-opened solutions follow, kept in their store insertion order.
    sols.sort_by(|a, b| match (a.2, b.2) {
        (Some(ts_a), Some(ts_b)) => ts_b.cmp(&ts_a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    sols.into_iter()
        .map(|(id, name, _, is_empty)| RecentSolution {
            id,
            label: name,
            is_empty,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use solutions::SolutionStore;
    use tempfile::tempdir;

    #[gpui::test]
    async fn empty_store_yields_empty_list(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            solutions::install_global_for_test(store, cx);
            assert!(all_solutions_for_test(cx).is_empty());
        });
    }

    #[gpui::test]
    async fn unopened_solutions_are_included(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            store
                .update(cx, |s, cx| {
                    s.create_solution("Alpha", dir.path().to_path_buf(), cx)
                })
                .expect("create");
            solutions::install_global_for_test(store, cx);
            // Welcome shows ALL solutions — never-opened ones included.
            let entries = all_solutions_for_test(cx);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].label, "Alpha");
        });
    }

    #[gpui::test]
    async fn opened_solutions_first_then_unopened_in_store_order(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            // Three never-opened, then two opened (so the latter sort first).
            for i in 0..3 {
                store
                    .update(cx, |s, cx| {
                        s.create_solution(&format!("Unopen{i}"), dir.path().join(format!("u{i}")), cx)
                    })
                    .expect("create");
            }
            for i in 0..2 {
                let sol_id = store
                    .update(cx, |s, cx| {
                        s.create_solution(&format!("Open{i}"), dir.path().join(format!("o{i}")), cx)
                    })
                    .expect("create");
                store
                    .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
                    .expect("touch");
            }
            solutions::install_global_for_test(store, cx);

            let entries = all_solutions_for_test(cx);
            assert_eq!(entries.len(), 5);
            // Most recently opened first.
            assert_eq!(entries[0].label, "Open1");
            assert_eq!(entries[1].label, "Open0");
            // Then never-opened in store insertion order.
            assert_eq!(entries[2].label, "Unopen0");
            assert_eq!(entries[3].label, "Unopen1");
            assert_eq!(entries[4].label, "Unopen2");
        });
    }
}
