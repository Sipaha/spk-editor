//! Welcome page integration: renders a "Recent Solutions" section by
//! plugging into `workspace::register_welcome_section`.
//!
//! Lives here (not in `workspace`) so the dependency graph stays one-way:
//! `solutions_ui → workspace`, never the reverse. Earlier we tried to read
//! `SolutionStore` directly from `workspace::welcome` and that introduced
//! the `workspace ↔ solutions` cycle that had to be reverted.

use chrono::{DateTime, Utc};
use gpui::{Action, AnyElement, App, IntoElement};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use solutions::{SolutionId, SolutionStore};
use ui::prelude::*;
use ui::{ButtonLike, Divider, DividerColor};
use util::ResultExt as _;
use workspace::{AppState, OpenOptions, welcome::register_welcome_section};

const MAX_RECENT: usize = 5;

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
}

fn render_section(cx: &mut App) -> Option<AnyElement> {
    let entries = recent_solutions(cx);
    if entries.is_empty() {
        return None;
    }
    let mut list = ui::v_flex().w_full().gap_1();
    list = list.child(
        ui::h_flex()
            .px_1()
            .mb_2()
            .gap_2()
            .child(
                Label::new("RECENT SOLUTIONS")
                    .buffer_font(cx)
                    .color(Color::Muted)
                    .size(LabelSize::XSmall),
            )
            .child(Divider::horizontal().color(DividerColor::BorderVariant)),
    );
    for (index, entry) in entries.into_iter().enumerate() {
        list = list.child(render_row(index, entry));
    }
    Some(list.into_any_element())
}

fn render_row(index: usize, entry: RecentSolution) -> impl IntoElement {
    ButtonLike::new(("recent-solution", index))
        .full_width()
        .size(ui::ButtonSize::Medium)
        .child(
            ui::h_flex()
                .gap_2()
                .child(
                    Icon::new(IconName::Folder)
                        .color(Color::Muted)
                        .size(IconSize::Small),
                )
                .child(Label::new(entry.label)),
        )
        .on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(OpenRecentSolution { index }), cx);
        })
}

fn open_recent_solution(action: &OpenRecentSolution, cx: &mut App) {
    let entries = recent_solutions(cx);
    let Some(entry) = entries.get(action.index) else {
        return;
    };
    open_solution(entry.id.clone(), cx);
}

fn open_solution(sol_id: SolutionId, cx: &mut App) {
    let Some(store) = SolutionStore::try_global(cx) else {
        return;
    };
    let paths = match store.read_with(cx, |s, _| s.paths_for_open(&sol_id)) {
        Ok(paths) => paths,
        Err(err) => {
            log::error!("solutions_ui: paths_for_open failed: {err}");
            return;
        }
    };
    if paths.is_empty() {
        return;
    }
    store
        .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
        .log_err();
    let app_state = AppState::global(cx);
    let task = workspace::open_paths(&paths, app_state, OpenOptions::default(), cx);
    cx.spawn(async move |_| {
        task.await.log_err();
    })
    .detach();
}

#[cfg_attr(test, derive(Debug))]
struct RecentSolution {
    id: SolutionId,
    label: String,
}

#[cfg(test)]
fn recent_solutions_for_test(cx: &App) -> Vec<RecentSolution> {
    recent_solutions(cx)
}

fn recent_solutions(cx: &App) -> Vec<RecentSolution> {
    let Some(store) = SolutionStore::try_global(cx) else {
        return Vec::new();
    };
    let mut sols: Vec<(SolutionId, String, Option<DateTime<Utc>>)> = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .filter(|sol| sol.last_opened_at.is_some())
            .map(|sol| (sol.id.clone(), sol.name.clone(), sol.last_opened_at))
            .collect()
    });
    sols.sort_by(|a, b| b.2.cmp(&a.2));
    sols.truncate(MAX_RECENT);
    sols.into_iter()
        .map(|(id, name, _)| RecentSolution { id, label: name })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use solutions::SolutionStore;
    use tempfile::tempdir;

    #[gpui::test]
    async fn empty_store_yields_no_recent(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            solutions::install_global_for_test(store, cx);
            assert!(recent_solutions_for_test(cx).is_empty());
        });
    }

    #[gpui::test]
    async fn unopened_solutions_are_excluded(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            store
                .update(cx, |s, cx| s.create_solution("Alpha", dir.path().to_path_buf(), cx))
                .expect("create");
            solutions::install_global_for_test(store, cx);
            // last_opened_at is None for a freshly-created solution.
            assert!(recent_solutions_for_test(cx).is_empty());
        });
    }

    #[gpui::test]
    async fn most_recent_first_capped_to_max(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            // Create MAX_RECENT + 2 solutions and stamp them in order.
            for i in 0..MAX_RECENT + 2 {
                let sol_id = store
                    .update(cx, |s, cx| {
                        s.create_solution(&format!("Sol{i}"), dir.path().join(format!("r{i}")), cx)
                    })
                    .expect("create");
                store
                    .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
                    .expect("touch");
            }
            solutions::install_global_for_test(store, cx);

            let recent = recent_solutions_for_test(cx);
            assert_eq!(recent.len(), MAX_RECENT);
            // Highest index was stamped last → comes first.
            let last_index = MAX_RECENT + 1;
            assert_eq!(recent[0].label, format!("Sol{last_index}"));
            // Just-before-last is second.
            assert_eq!(recent[1].label, format!("Sol{}", last_index - 1));
        });
    }
}
