use std::collections::HashMap;

use agent_client_protocol::schema as acp;
use gpui::SharedString;

use super::recall::unpack_recalled_bundle;
use super::SolutionSessionView;
use crate::model::SubagentTab;
use crate::store::SubagentView;

fn text_block(s: &str) -> acp::ContentBlock {
    acp::ContentBlock::Text(acp::TextContent::new(s.to_string()))
}

fn image_block(data: &str, mime: &str) -> acp::ContentBlock {
    acp::ContentBlock::Image(acp::ImageContent::new(data.to_string(), mime.to_string()))
}

#[test]
fn unpack_recalled_bundle_strips_marker_and_concatenates_text() {
    let bundle = vec![
        text_block(
            "[The user typed the following at 14:23:01 (local time) while you were still on \
             the previous turn — this is NOT a direct reply to your last question or tool \
             result, it was queued in advance.]\n\nfirst part",
        ),
        text_block("\n\n"),
        text_block("second part"),
    ];
    let (text, images) = unpack_recalled_bundle(bundle);
    assert_eq!(text, "first part\n\nsecond part");
    assert!(images.is_empty());
}

#[test]
fn unpack_recalled_bundle_passes_through_unmarked_text() {
    // Bundles built before the marker shipped (e.g. older persisted state)
    // shouldn't get mangled — leading text is returned untouched.
    let bundle = vec![text_block("plain user input")];
    let (text, images) = unpack_recalled_bundle(bundle);
    assert_eq!(text, "plain user input");
    assert!(images.is_empty());
}

#[test]
fn unpack_recalled_bundle_recovers_images_with_labels_from_text() {
    let bundle = vec![
        text_block("look at [image #5] and [image #7]"),
        image_block("aGVsbG8=", "image/png"),
        image_block("d29ybGQ=", "image/jpeg"),
    ];
    let (text, images) = unpack_recalled_bundle(bundle);
    assert_eq!(text, "look at [image #5] and [image #7]");
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].data_base64, "aGVsbG8=");
    assert_eq!(images[0].mime_type, "image/png");
    assert_eq!(images[0].label.as_ref(), "image #5");
    assert_eq!(images[1].data_base64, "d29ybGQ=");
    assert_eq!(images[1].mime_type, "image/jpeg");
    assert_eq!(images[1].label.as_ref(), "image #7");
}

fn make_tab(label: &str) -> SubagentTab {
    SubagentTab {
        label: SharedString::from(label.to_string()),
        started_at: chrono::Utc::now(),
    }
}

#[test]
fn next_selection_after_change_keeps_still_active_selection() {
    let id_a = SharedString::from("toolu_a");
    let id_b = SharedString::from("toolu_b");
    let mut active: HashMap<SharedString, SubagentTab> = HashMap::new();
    active.insert(id_a.clone(), make_tab("A"));
    active.insert(id_b.clone(), make_tab("B"));
    let order = vec![id_a.clone(), id_b.clone()];
    let next = SolutionSessionView::next_selection_after_change(
        &SubagentView::Task(id_a.clone()),
        &active,
        &order,
    );
    assert_eq!(
        next,
        SubagentView::Task(id_a),
        "still-active selection must be preserved"
    );
}

#[test]
fn next_selection_after_change_snaps_to_next_when_current_removed() {
    let id_a = SharedString::from("toolu_a");
    let id_b = SharedString::from("toolu_b");
    let mut active: HashMap<SharedString, SubagentTab> = HashMap::new();
    active.insert(id_b.clone(), make_tab("B"));
    // `id_a` is gone but still asked-for; `id_b` remains, first in order.
    let order = vec![id_b.clone()];
    let next = SolutionSessionView::next_selection_after_change(
        &SubagentView::Task(id_a),
        &active,
        &order,
    );
    assert_eq!(next, SubagentView::Task(id_b));
}

#[test]
fn next_selection_after_change_falls_back_to_main_when_all_gone() {
    let id_a = SharedString::from("toolu_a");
    let active: HashMap<SharedString, SubagentTab> = HashMap::new();
    let order: Vec<SharedString> = Vec::new();
    let next = SolutionSessionView::next_selection_after_change(
        &SubagentView::Task(id_a),
        &active,
        &order,
    );
    assert_eq!(
        next,
        SubagentView::Main,
        "empty active set must collapse to Main"
    );
}

#[test]
fn next_selection_after_change_main_stays_main() {
    let id_a = SharedString::from("toolu_a");
    let mut active: HashMap<SharedString, SubagentTab> = HashMap::new();
    active.insert(id_a.clone(), make_tab("A"));
    let order = vec![id_a];
    // Main was already selected — a strip change should not yank us into a tab.
    let next = SolutionSessionView::next_selection_after_change(&SubagentView::Main, &active, &order);
    assert_eq!(next, SubagentView::Main);
}

fn make_background_agent(id: &str) -> crate::background_agent::BackgroundAgent {
    crate::background_agent::BackgroundAgent {
        id: crate::background_agent::BackgroundAgentId::new(id),
        jsonl_path: std::path::PathBuf::from("/dev/null"),
        registered_at: chrono::Utc::now(),
        latest: None,
    }
}

#[test]
fn next_selection_after_background_change_snaps_stale_background_to_main() {
    // Stale Background id: the user × closed the pill (or healthcheck
    // reaped it), the agent is no longer in the session map. Renderer
    // would paint empty; this snap fires off the
    // `SessionBackgroundAgentsChanged` handler and restores Main.
    let stale = crate::background_agent::BackgroundAgentId::new("a30f92");
    let agents = HashMap::new();
    let next = SolutionSessionView::next_selection_after_background_change(
        &SubagentView::Background(stale),
        &agents,
    );
    assert_eq!(next, SubagentView::Main);
}

#[test]
fn next_selection_after_background_change_keeps_live_background() {
    let id = crate::background_agent::BackgroundAgentId::new("a30f92");
    let mut agents = HashMap::new();
    agents.insert(id.clone(), make_background_agent("a30f92"));
    let next = SolutionSessionView::next_selection_after_background_change(
        &SubagentView::Background(id.clone()),
        &agents,
    );
    assert_eq!(next, SubagentView::Background(id));
}

#[test]
fn next_selection_after_background_change_passes_through_main_and_task() {
    let agents = HashMap::new();
    assert_eq!(
        SolutionSessionView::next_selection_after_background_change(
            &SubagentView::Main,
            &agents,
        ),
        SubagentView::Main,
    );
    let task_id = SharedString::from("toolu_a");
    assert_eq!(
        SolutionSessionView::next_selection_after_background_change(
            &SubagentView::Task(task_id.clone()),
            &agents,
        ),
        SubagentView::Task(task_id),
    );
}

#[test]
fn next_selection_after_change_preserves_background_view() {
    // Background views render a Managed Agent's standalone JSONL transcript,
    // so a change in the Task subagent set must not perturb them.
    let bg_id = crate::background_agent::BackgroundAgentId::new("a30f92");
    let id_a = SharedString::from("toolu_a");
    let mut active: HashMap<SharedString, SubagentTab> = HashMap::new();
    active.insert(id_a.clone(), make_tab("A"));
    let order = vec![id_a];
    let next = SolutionSessionView::next_selection_after_change(
        &SubagentView::Background(bg_id.clone()),
        &active,
        &order,
    );
    assert_eq!(next, SubagentView::Background(bg_id));
}

#[test]
fn compose_disabled_predicate_returns_false_for_main() {
    assert!(!super::compose_disabled_for(&SubagentView::Main));
}

#[test]
fn compose_disabled_predicate_returns_false_for_task() {
    assert!(!super::compose_disabled_for(&SubagentView::Task(
        SharedString::from("toolu_a")
    )));
}

#[test]
fn compose_disabled_predicate_returns_true_for_background() {
    let id = crate::background_agent::BackgroundAgentId::new("a30f92");
    assert!(super::compose_disabled_for(&SubagentView::Background(id)));
}

#[test]
fn unpack_recalled_bundle_handles_more_images_than_placeholders() {
    // Defensive: if the text somehow lost its `[image #N]` placeholders
    // (e.g. user manually edited them out before submission), images
    // still come back with safe placeholder labels and never panic.
    let bundle = vec![
        text_block("no placeholders here"),
        image_block("aGVsbG8=", "image/png"),
    ];
    let (text, images) = unpack_recalled_bundle(bundle);
    assert_eq!(text, "no placeholders here");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].label.as_ref(), "image #?");
}
