//! Member-picker popover hosted by ActiveProjectSelector.
//!
//! Lists the active solution's members. Click a row → set the panel's
//! `panel_member_selections` entry and emit DismissEvent. Search input
//! filters by display name, case-insensitive substring.
//!
//! Each row has a trash icon that dispatches `RemoveMember` (Task 8)
//! and a change-count badge slot (Task 10).

use collections::HashMap;
use editor::Editor;
use gpui::{
    AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};
use solutions::{CatalogId, SolutionId, SolutionMember, SolutionStore, db::PanelKind};
use ui::{ListItem, ListItemSpacing, prelude::*};
use util::ResultExt as _;

use super::member_display_name;

pub struct MemberPicker {
    panel_kind: PanelKind,
    solution_id: SolutionId,
    members: Vec<SolutionMember>,
    selected_catalog_id: Option<CatalogId>,
    change_counts: HashMap<CatalogId, usize>,
    search_editor: Entity<Editor>,
    query: String,
    focus_handle: FocusHandle,
    _editor_subscription: Subscription,
}

impl MemberPicker {
    pub fn new(
        panel_kind: PanelKind,
        solution_id: SolutionId,
        members: Vec<SolutionMember>,
        selected_catalog_id: Option<CatalogId>,
        change_counts: HashMap<CatalogId, usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search members…", window, cx);
            editor
        });
        let editor_subscription = cx.subscribe(&search_editor, |this, _, event, cx| {
            if matches!(
                event,
                editor::EditorEvent::BufferEdited | editor::EditorEvent::Edited { .. }
            ) {
                this.query = this.search_editor.read(cx).text(cx).trim().to_lowercase();
                cx.notify();
            }
        });
        let focus_handle = search_editor.focus_handle(cx);
        Self {
            panel_kind,
            solution_id,
            members,
            selected_catalog_id,
            change_counts,
            search_editor,
            query: String::new(),
            focus_handle,
            _editor_subscription: editor_subscription,
        }
    }

    pub fn confirm_member(
        &mut self,
        catalog_id: CatalogId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = SolutionStore::global(cx);
        let solution_id = self.solution_id.clone();
        let panel_kind = self.panel_kind;
        store
            .update(cx, |store, cx| {
                store.set_panel_member_selection(solution_id, panel_kind, catalog_id, cx)
            })
            .log_err();
        cx.emit(DismissEvent);
    }

    fn filtered<'a>(&'a self, store: &'a SolutionStore) -> Vec<&'a SolutionMember> {
        self.members
            .iter()
            .filter(|member| {
                if self.query.is_empty() {
                    return true;
                }
                member_display_name(member, store)
                    .to_lowercase()
                    .contains(&self.query)
            })
            .collect()
    }
}

impl EventEmitter<DismissEvent> for MemberPicker {}

impl Focusable for MemberPicker {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MemberPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = SolutionStore::global(cx);
        let store_ref = store.read(cx);
        let members: Vec<(CatalogId, SharedString, bool, Option<usize>)> = self
            .filtered(store_ref)
            .into_iter()
            .map(|member| {
                let catalog_id = member.catalog_id.clone();
                let label: SharedString = member_display_name(member, store_ref).into();
                let active = self.selected_catalog_id.as_ref() == Some(&member.catalog_id);
                let count = self.change_counts.get(&member.catalog_id).copied();
                (catalog_id, label, active, count)
            })
            .collect();

        let mut list = v_flex().gap_0p5();
        for (catalog_id, label, active, count) in members {
            let sol = self.solution_id.clone();
            let cat = catalog_id.clone();
            let row = ListItem::new(SharedString::from(catalog_id.0.clone()))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(active)
                .child(Label::new(label))
                .end_slot(
                    h_flex()
                        .gap_2()
                        .when_some(count.filter(|c| *c > 0), |this, c| {
                            this.child(
                                Label::new(format!("● {c}"))
                                    .color(Color::Accent)
                                    .size(LabelSize::Small),
                            )
                        })
                        .child(
                            ui::IconButton::new(
                                SharedString::from(format!("delete-{}", cat.0)),
                                ui::IconName::Trash,
                            )
                            .size(ui::ButtonSize::Compact)
                            .icon_color(Color::Muted)
                            .on_click(cx.listener(move |_this, _, window, cx| {
                                cx.emit(DismissEvent);
                                window.dispatch_action(
                                    Box::new(crate::actions::RemoveMember {
                                        solution_id: sol.0.clone(),
                                        catalog_id: cat.0.clone(),
                                    }),
                                    cx,
                                );
                            })),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_member(catalog_id.clone(), window, cx);
                }));
            list = list.child(row);
        }

        v_flex()
            .key_context("ActiveProjectMemberPicker")
            .track_focus(&self.focus_handle)
            .w(rems(28.))
            .p_2()
            .gap_2()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .child(self.search_editor.clone())
            .child(list)
    }
}
