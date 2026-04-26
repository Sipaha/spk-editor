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

struct RecentSolution {
    id: SolutionId,
    label: String,
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
