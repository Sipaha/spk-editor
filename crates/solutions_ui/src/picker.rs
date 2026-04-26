use std::sync::Arc;

use gpui::{
    AppContext as _, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Task, WeakEntity,
};
use picker::{Picker, PickerDelegate};
use solutions::{SolutionId, SolutionStore};
use ui::{ListItem, ListItemSpacing, prelude::*};
use util::ResultExt as _;
use workspace::{AppState, ModalView, OpenOptions, Workspace};

use crate::actions::OpenSolution;

pub struct OpenSolutionModal {
    picker: Entity<Picker<OpenSolutionDelegate>>,
}

impl OpenSolutionModal {
    pub fn register(workspace: &mut Workspace, _: Option<&mut Window>, _: &mut Context<Workspace>) {
        workspace.register_action(|workspace, _: &OpenSolution, window, cx| {
            let weak = cx.weak_entity();
            workspace.toggle_modal(window, cx, move |window, cx| {
                OpenSolutionModal::new(weak, window, cx)
            });
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = OpenSolutionDelegate::new(workspace, cx.entity().downgrade(), cx);
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for OpenSolutionModal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for OpenSolutionModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for OpenSolutionModal {}
impl ModalView for OpenSolutionModal {}

struct SolutionEntry {
    id: SolutionId,
    name: String,
    member_count: usize,
}

pub struct OpenSolutionDelegate {
    workspace: WeakEntity<Workspace>,
    modal: WeakEntity<OpenSolutionModal>,
    all: Vec<SolutionEntry>,
    matches: Vec<usize>,
    selected_index: usize,
}

impl OpenSolutionDelegate {
    fn new(
        workspace: WeakEntity<Workspace>,
        modal: WeakEntity<OpenSolutionModal>,
        cx: &mut App,
    ) -> Self {
        let store = SolutionStore::global(cx);
        let mut all: Vec<SolutionEntry> = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .map(|sol| SolutionEntry {
                    id: sol.id.clone(),
                    name: sol.name.clone(),
                    member_count: sol.members.len(),
                })
                .collect()
        });
        all.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        let matches = (0..all.len()).collect();
        Self {
            workspace,
            modal,
            all,
            matches,
            selected_index: 0,
        }
    }
}

impl PickerDelegate for OpenSolutionDelegate {
    type ListItem = ListItem;

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search Solutions…".into()
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
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_lowercase();
        self.matches = if query.is_empty() {
            (0..self.all.len()).collect()
        } else {
            self.all
                .iter()
                .enumerate()
                .filter(|(_, e)| e.name.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect()
        };
        if self.selected_index >= self.matches.len() {
            self.selected_index = 0;
        }
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(&idx) = self.matches.get(self.selected_index) else {
            return;
        };
        let Some(entry) = self.all.get(idx) else {
            return;
        };
        let sol_id = entry.id.clone();

        let store = SolutionStore::global(cx);
        let paths = match store.read_with(cx, |s, _| s.paths_for_open(&sol_id)) {
            Ok(paths) => paths,
            Err(err) => {
                log::error!("solutions_ui: paths_for_open failed: {err}");
                self.dismissed(window, cx);
                return;
            }
        };
        if paths.is_empty() {
            self.dismissed(window, cx);
            return;
        }
        let app_state = AppState::global(cx);

        store
            .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
            .log_err();

        cx.spawn(async move |_, cx| {
            let task = cx.update(|cx| {
                let mut options = OpenOptions::default();
                options.open_mode = workspace::OpenMode::NewWindow;
                workspace::open_paths(&paths, app_state, options, cx)
            });
            task.await?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);

        self.dismissed(window, cx);
        let _ = self.workspace.upgrade();
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.modal
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry_idx = *self.matches.get(ix)?;
        let entry = self.all.get(entry_idx)?;
        let item = ListItem::new(ix)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(Label::new(entry.name.clone()))
            .end_slot(
                Label::new(format!("{} project(s)", entry.member_count))
                    .color(Color::Muted)
                    .size(LabelSize::Small),
            );
        Some(item)
    }
}
