use anyhow::Result;
use gpui::{
    App, AppContext as _, AsyncWindowContext, Entity, EventEmitter, FocusHandle, Focusable,
    MouseButton, Pixels, Render, WeakEntity, Window, px,
};
use solutions::{CatalogProject, Solution, SolutionId, SolutionStore, SolutionStoreEvent};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{
    AppState, OpenOptions, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::actions::{NewSolution, ToggleSolutionsPanel};

pub struct SolutionsPanel {
    focus_handle: FocusHandle,
    _workspace: WeakEntity<Workspace>,
    width: Option<Pixels>,
    _store_subscription: gpui::Subscription,
}

impl SolutionsPanel {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let store = SolutionStore::global(cx);
        let store_subscription =
            cx.subscribe(&store, |_, _, _event: &SolutionStoreEvent, cx| {
                cx.notify();
            });
        Self {
            focus_handle: cx.focus_handle(),
            _workspace: workspace,
            width: None,
            _store_subscription: store_subscription,
        }
    }

    fn render_section_header(label: &'static str) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
    }

    fn render_catalog_row(project: &CatalogProject) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .child(Icon::new(IconName::GitBranch).size(IconSize::Small))
            .child(Label::new(project.name.clone()))
    }

    fn render_solution_row(
        &self,
        s: &Solution,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let sol_id = s.id.clone();
        h_flex()
            .id(SharedString::from(format!("sol-{}", s.id.as_str())))
            .px_2()
            .py_1()
            .gap_2()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().colors().element_hover))
            .child(Icon::new(IconName::Folder).size(IconSize::Small))
            .child(Label::new(s.name.clone()))
            .child(
                Label::new(format!("({} projects)", s.members.len()))
                    .color(Color::Muted)
                    .size(LabelSize::Small),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, window, cx| {
                    this.open_solution(sol_id.clone(), window, cx);
                }),
            )
    }

    fn open_solution(
        &self,
        sol_id: SolutionId,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let store = SolutionStore::global(cx);
        let paths = match store.read_with(cx, |s, _| s.paths_for_open(&sol_id)) {
            Ok(paths) => paths,
            Err(err) => {
                log::error!("solutions_ui: paths_for_open failed: {err}");
                return;
            }
        };
        if paths.is_empty() {
            log::info!("solutions_ui: solution {} has no members", sol_id.as_str());
            return;
        }
        let app_state = AppState::global(cx);

        store
            .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
            .log_err();

        cx.spawn(async move |_, cx| {
            let task = cx.update(|cx| {
                workspace::open_paths(&paths, app_state, OpenOptions::default(), cx)
            });
            task.await?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }
}

impl EventEmitter<PanelEvent> for SolutionsPanel {}

impl Focusable for SolutionsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for SolutionsPanel {
    fn persistent_name() -> &'static str {
        "SolutionsPanel"
    }

    fn panel_key() -> &'static str {
        "SolutionsPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _: &mut Window,
        _: &mut gpui::Context<Self>,
    ) {
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        self.width.unwrap_or(px(280.))
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Folder)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Solutions")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleSolutionsPanel)
    }

    fn activation_priority(&self) -> u32 {
        9
    }
}

impl Render for SolutionsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let store = SolutionStore::global(cx);
        let (catalog, solutions) =
            store.read_with(cx, |s, _| (s.catalog().to_vec(), s.solutions().to_vec()));

        v_flex()
            .key_context("SolutionsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(Self::render_section_header("Solutions"))
            .children(
                solutions
                    .iter()
                    .map(|s| self.render_solution_row(s, cx).into_any_element())
                    .collect::<Vec<_>>(),
            )
            .child(
                div().px_2().py_1().child(
                    Button::new("new-solution", "+ New Solution")
                        .on_click(cx.listener(|_, _, window, cx| {
                            window.dispatch_action(Box::new(NewSolution), cx);
                        })),
                ),
            )
            .child(div().h(px(8.)))
            .child(Self::render_section_header("Catalog"))
            .children(catalog.iter().map(Self::render_catalog_row))
            .child(
                div().px_2().py_1().child(
                    Button::new("add-project", "+ Add Project").on_click(cx.listener(
                        |_, _, window, cx| {
                            window.dispatch_action(
                                Box::new(crate::modals::AddCatalogProject),
                                cx,
                            );
                        },
                    )),
                ),
            )
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleSolutionsPanel, window, cx| {
            workspace.toggle_panel_focus::<SolutionsPanel>(window, cx);
        });
    })
    .detach();
}

pub async fn load(
    workspace: WeakEntity<Workspace>,
    mut cx: AsyncWindowContext,
) -> Result<Entity<SolutionsPanel>> {
    workspace.update_in(&mut cx, |_, window, cx| {
        cx.new(|cx2| SolutionsPanel::new(workspace.clone(), window, cx2))
    })
}
