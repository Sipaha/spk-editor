//! Welcome page integration: renders a Solutions section (full list +
//! Create button) by plugging into `workspace::register_welcome_section`.
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
use gpui::{AnyElement, AnyWindowHandle, App, IntoElement};
use solutions::{Solution, SolutionId, SolutionStore, SolutionStoreEvent};
use std::path::PathBuf;
use ui::{ButtonLike, Divider, DividerColor, IconButtonShape, prelude::*};
use util::ResultExt as _;
use workspace::{
    AppState, OpenOptions,
    welcome::{WelcomePage, register_welcome_section},
};

use crate::actions::{DeleteSolution, NewSolution};

/// Wires the Recent Solutions section onto the welcome page. Called once
/// from `solutions_ui::init`.
pub fn init(cx: &mut App) {
    register_welcome_section(cx, render_section);

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
    let mut list = ui::v_flex().w_full().gap_2();
    list = list.child(
        ui::h_flex()
            .px_1()
            .mb_1()
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
                    Label::new("No solutions yet — create one to get started.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                ),
        );
    } else {
        for (index, entry) in entries.into_iter().enumerate() {
            list = list.child(render_card(index, entry, cx));
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
                .px_1()
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

/// IDEA-style card: colored avatar, name, path, last-opened ago.
/// Trash button on hover.
fn render_card(index: usize, entry: RecentSolution, cx: &App) -> impl IntoElement {
    let entry_id = entry.id.clone();
    let entry_id_for_delete = entry.id.clone();

    let avatar_color = avatar_color_for(&entry.label, cx);
    let initials = initials_of(&entry.label);

    let path_display = display_path(&entry.root);
    let meta = entry
        .last_opened_at
        .map(|ts| relative_time_label(ts, Utc::now()))
        .unwrap_or_else(|| "never opened".to_string());

    // We can't put `on_mouse_down` on the parent row because then a click
    // anywhere on the row fires "open" — including on the trash button,
    // since GPUI's parent listeners run before child `stop_propagation`
    // takes effect. So the row is a non-clickable layout container; the
    // "open" handler lives on an inner `clickable_body` div that sits
    // alongside the trash button as siblings.
    ui::h_flex()
        .id(("solution-card-row", index))
        .w_full()
        .gap_2()
        .px_2()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .bg(cx.theme().colors().elevated_surface_background)
        .hover(|s| s.bg(cx.theme().colors().element_hover))
        .child(
            ui::h_flex()
                .id(("solution-card-body", index))
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .cursor_pointer()
                .on_click(move |_, window, cx| {
                    let source = window.window_handle();
                    open_solution(entry_id.clone(), Some(source), cx);
                })
                .child(
                    ui::h_flex()
                        .flex_none()
                        .size_8()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .bg(avatar_color)
                        .child(
                            Label::new(initials)
                                .size(LabelSize::Default)
                                .color(Color::Custom(gpui::white())),
                        ),
                )
                .child(
                    ui::v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            ui::h_flex()
                                .gap_2()
                                .child(Label::new(entry.label.clone()).size(LabelSize::Default))
                                .when(entry.is_empty, |this| {
                                    this.child(
                                        Label::new("(empty)")
                                            .color(Color::Muted)
                                            .size(LabelSize::XSmall),
                                    )
                                }),
                        )
                        .child(
                            Label::new(path_display)
                                .color(Color::Muted)
                                .size(LabelSize::XSmall)
                                .truncate(),
                        ),
                )
                .child(
                    Label::new(meta)
                        .color(Color::Muted)
                        .size(LabelSize::XSmall),
                ),
        )
        .child(
            IconButton::new(("delete-solution", index), IconName::Trash)
                .shape(IconButtonShape::Square)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(ui::Tooltip::text("Delete solution"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(DeleteSolution {
                            id: entry_id_for_delete.0.clone(),
                        }),
                        cx,
                    );
                }),
        )
}

fn open_solution(sol_id: SolutionId, source_window: Option<AnyWindowHandle>, cx: &mut App) {
    let Some(store) = SolutionStore::try_global(cx) else {
        return;
    };
    // For an empty solution we still want to open a window — just one with
    // `solution.root` as the only worktree, plus an EmptySolutionPage item
    // so the user has a CTA instead of a blank workspace.
    struct OpenInfo {
        paths: Vec<std::path::PathBuf>,
        name: String,
        is_empty: bool,
    }
    let info = match store.read_with(cx, |s, _| -> anyhow::Result<OpenInfo> {
        let solution = s
            .solutions()
            .iter()
            .find(|sol| sol.id == sol_id)
            .ok_or_else(|| anyhow!("solution not found: {}", sol_id.0))?;
        let is_empty = solution.members.is_empty();
        let name = solution.name.clone();
        let paths = if is_empty {
            vec![solution.root.clone()]
        } else {
            s.paths_for_open(&sol_id)?
        };
        Ok(OpenInfo {
            paths,
            name,
            is_empty,
        })
    }) {
        Ok(info) => info,
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
    let task = workspace::open_paths(&info.paths, app_state, options, cx);
    cx.spawn(async move |cx| {
        let Some(opened) = task.await.log_err() else {
            return;
        };
        if info.is_empty {
            let sol_id_for_page = sol_id.clone();
            let name_for_page = info.name.clone();
            cx.update(|cx| {
                opened
                    .window
                    .update(cx, |multi_workspace, window, cx| {
                        let workspace = multi_workspace.workspace().clone();
                        let weak_workspace = workspace.downgrade();
                        workspace.update(cx, |ws, cx| {
                            let page = cx.new(|cx| {
                                crate::empty_solution_page::EmptySolutionPage::new(
                                    sol_id_for_page,
                                    name_for_page,
                                    weak_workspace,
                                    cx,
                                )
                            });
                            ws.add_item_to_active_pane(
                                Box::new(page),
                                None,
                                true,
                                window,
                                cx,
                            );
                        });
                    })
                    .log_err();
            });
        }
        // Defensive: don't close the source window if `open_paths` reused it
        // (today NewWindow always creates a fresh one, but a future change
        // could reuse — closing it would kill the window the user just
        // opened). When the keybind path supplies no source, nothing to do.
        let Some(source) = source_window else { return };
        if source.window_id() == opened.window.window_id() {
            return;
        }
        cx.update(|cx| {
            if let Err(err) = source.update(cx, |_, window, _| window.remove_window()) {
                log::warn!("solutions_ui: failed to close welcome window: {err}");
            }
        });
    })
    .detach();
}

#[cfg_attr(test, derive(Debug))]
struct RecentSolution {
    id: SolutionId,
    label: String,
    root: PathBuf,
    is_empty: bool,
    last_opened_at: Option<DateTime<Utc>>,
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
    let mut sols: Vec<RecentSolution> = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .map(|sol: &Solution| RecentSolution {
                id: sol.id.clone(),
                label: sol.name.clone(),
                root: sol.root.clone(),
                is_empty: sol.members.is_empty(),
                last_opened_at: sol.last_opened_at,
            })
            .collect()
    });
    // Opened solutions first, ordered by last_opened_at desc (newest first).
    // Never-opened solutions follow, kept in their store insertion order.
    sols.sort_by(|a, b| match (a.last_opened_at, b.last_opened_at) {
        (Some(ts_a), Some(ts_b)) => ts_b.cmp(&ts_a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    sols
}

fn initials_of(name: &str) -> String {
    let parts: Vec<&str> = name.split_whitespace().collect();
    if parts.is_empty() {
        return "?".into();
    }
    if parts.len() == 1 {
        let s = parts[0];
        return s.chars().take(2).collect::<String>().to_uppercase();
    }
    let mut s = String::new();
    for p in parts.iter().take(2) {
        if let Some(c) = p.chars().next() {
            s.push(c.to_ascii_uppercase());
        }
    }
    if s.is_empty() {
        "?".into()
    } else {
        s
    }
}

/// Pick a stable accent color from the theme palette by hashing the name.
/// Same name → same color across launches.
fn avatar_color_for(name: &str, cx: &App) -> gpui::Hsla {
    let palette = &cx.theme().colors();
    let candidates = [
        palette.terminal_ansi_red,
        palette.terminal_ansi_green,
        palette.terminal_ansi_yellow,
        palette.terminal_ansi_blue,
        palette.terminal_ansi_magenta,
        palette.terminal_ansi_cyan,
    ];
    let h = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    candidates[(h as usize) % candidates.len()]
}

fn display_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    s
}

fn relative_time_label(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now.signed_duration_since(ts);
    let secs = delta.num_seconds();
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 7 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else if secs < 30 * 86_400 {
        format!("{}w ago", secs / (7 * 86_400))
    } else if secs < 365 * 86_400 {
        format!("{}mo ago", secs / (30 * 86_400))
    } else {
        format!("{}y ago", secs / (365 * 86_400))
    }
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

    #[test]
    fn initials_basic() {
        assert_eq!(initials_of("Alpha"), "AL");
        assert_eq!(initials_of("Alpha Bravo"), "AB");
        assert_eq!(initials_of("alpha bravo charlie"), "AB");
        assert_eq!(initials_of(""), "?");
        assert_eq!(initials_of("   "), "?");
    }

    #[test]
    fn relative_time_buckets() {
        let now = Utc::now();
        let m5 = now - chrono::Duration::minutes(5);
        let h2 = now - chrono::Duration::hours(2);
        let d3 = now - chrono::Duration::days(3);
        assert_eq!(relative_time_label(m5, now), "5m ago");
        assert_eq!(relative_time_label(h2, now), "2h ago");
        assert_eq!(relative_time_label(d3, now), "3d ago");
    }
}
