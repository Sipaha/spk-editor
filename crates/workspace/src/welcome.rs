use crate::{
    Workspace,
    item::{Item, ItemEvent},
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    InteractiveElement, ParentElement, Render, Styled, Task, Window, actions,
};
use gpui::WeakEntity;
use std::cell::RefCell;
use std::rc::Rc;

use ui::{ContextMenu, Divider, DividerColor, PopoverMenu, Vector, VectorName, prelude::*};
use zed_actions::{Extensions, OpenKeymap, OpenSettings};

actions!(
    zed,
    [
        /// Show the Zed welcome screen
        ShowWelcome
    ]
);

/// Header above a section list (used by registered sections via the public
/// helper `render_section_header`).
#[derive(IntoElement)]
pub struct SectionHeader {
    title: SharedString,
}

impl SectionHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .px_1()
            .mb_2()
            .gap_2()
            .child(
                Label::new(self.title.to_ascii_uppercase())
                    .buffer_font(cx)
                    .color(Color::Muted)
                    .size(LabelSize::XSmall),
            )
            .child(Divider::horizontal().color(DividerColor::BorderVariant))
    }
}

/// Closure that renders an extra section into the welcome page. Returns
/// `None` if the section has nothing to show this frame.
///
/// Lives behind an `Rc` so the registry can hand out clones that outlive
/// the borrow on the registry itself (rendering iterates registered
/// sections one at a time and each call needs `&mut App`).
pub type WelcomeSectionRenderer = Rc<dyn Fn(&mut App) -> Option<AnyElement>>;

#[derive(Default)]
struct WelcomeSectionRegistry {
    sections: RefCell<Vec<WelcomeSectionRenderer>>,
}

impl Global for WelcomeSectionRegistry {}

/// Register an extra section to render on the welcome page (above the
/// "Recent Projects" / static second section). Used by sibling crates such
/// as `solutions_ui` to plug Recent Solutions in without the `workspace`
/// crate having to take a dependency on `solutions` (which would create a
/// cycle: `solutions` already depends on `workspace::open_paths`).
pub fn register_welcome_section(
    cx: &mut App,
    renderer: impl Fn(&mut App) -> Option<AnyElement> + 'static,
) {
    if cx.try_global::<WelcomeSectionRegistry>().is_none() {
        cx.set_global(WelcomeSectionRegistry::default());
    }
    cx.global::<WelcomeSectionRegistry>()
        .sections
        .borrow_mut()
        .push(Rc::new(renderer));
}

fn render_registered_sections(cx: &mut App) -> Vec<AnyElement> {
    let renderers: Vec<WelcomeSectionRenderer> = cx
        .try_global::<WelcomeSectionRegistry>()
        .map(|reg| reg.sections.borrow().iter().cloned().collect())
        .unwrap_or_default();
    renderers
        .into_iter()
        .filter_map(|render| render(cx))
        .collect()
}

pub struct WelcomePage {
    _workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
}

impl WelcomePage {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();

        WelcomePage {
            _workspace: workspace,
            focus_handle,
        }
    }

    fn render_configure_menu(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        PopoverMenu::new("welcome-configure-menu")
            .trigger(
                IconButton::new("welcome-configure-trigger", IconName::Settings)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(ui::Tooltip::text("Configure")),
            )
            .menu(move |window, cx| {
                let focus = focus.clone();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    menu.action("Open Settings", Box::new(OpenSettings))
                        .action("Customize Keymaps", Box::new(OpenKeymap))
                        .action(
                            "Explore Extensions",
                            Box::new(Extensions {
                                category_filter: None,
                                id: None,
                            }),
                        )
                        .context(focus)
                }))
            })
            .anchor(gpui::Anchor::TopRight)
    }
}

impl Render for WelcomePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .key_context("Welcome")
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .justify_start()
            .child(
                v_flex()
                    .id("welcome-content")
                    .pl_16()
                    .pr_8()
                    .py_8()
                    .max_w(rems(56.))
                    .size_full()
                    .gap_6()
                    .overflow_y_scroll()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .mb_4()
                            .gap_4()
                            .child(
                                h_flex()
                                    .gap_4()
                                    .items_center()
                                    .child(Vector::square(VectorName::ZedLogo, rems_from_px(45.)))
                                    .child(
                                        v_flex()
                                            .child(Headline::new("Welcome to SPK Editor"))
                                            .child(
                                                Label::new("The editor for what's next")
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted)
                                                    .italic(),
                                            ),
                                    ),
                            )
                            .child(self.render_configure_menu(cx)),
                    )
                    .children(render_registered_sections(cx)),
            )
    }
}

impl EventEmitter<ItemEvent> for WelcomePage {}

impl Focusable for WelcomePage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for WelcomePage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Welcome".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("New Welcome Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(crate::item::ItemEvent)) {
        f(*event)
    }
}

impl crate::SerializableItem for WelcomePage {
    fn serialized_item_kind() -> &'static str {
        "WelcomePage"
    }

    fn cleanup(
        workspace_id: crate::WorkspaceId,
        alive_items: Vec<crate::ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<gpui::Result<()>> {
        crate::delete_unloaded_items(
            alive_items,
            workspace_id,
            "welcome_pages",
            &persistence::WelcomePagesDb::global(cx),
            cx,
        )
    }

    fn deserialize(
        _project: Entity<project::Project>,
        workspace: gpui::WeakEntity<Workspace>,
        workspace_id: crate::WorkspaceId,
        item_id: crate::ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<gpui::Result<Entity<Self>>> {
        if persistence::WelcomePagesDb::global(cx)
            .get_welcome_page(item_id, workspace_id)
            .ok()
            .is_some_and(|is_open| is_open)
        {
            Task::ready(Ok(cx.new(|cx| WelcomePage::new(workspace, window, cx))))
        } else {
            Task::ready(Err(anyhow::anyhow!("No welcome page to deserialize")))
        }
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: crate::ItemId,
        _closing: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<gpui::Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let db = persistence::WelcomePagesDb::global(cx);
        Some(cx.background_spawn(
            async move { db.save_welcome_page(item_id, workspace_id, true).await },
        ))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        event == &ItemEvent::UpdateTab
    }
}

mod persistence {
    use crate::WorkspaceDb;
    use db::{
        query,
        sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
        sqlez_macros::sql,
    };

    pub struct WelcomePagesDb(ThreadSafeConnection);

    impl Domain for WelcomePagesDb {
        const NAME: &str = stringify!(WelcomePagesDb);

        const MIGRATIONS: &[&str] = (&[sql!(
                    CREATE TABLE welcome_pages (
                        workspace_id INTEGER,
                        item_id INTEGER UNIQUE,
                        is_open INTEGER DEFAULT FALSE,

                        PRIMARY KEY(workspace_id, item_id),
                        FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                        ON DELETE CASCADE
                    ) STRICT;
        )]);
    }

    db::static_connection!(WelcomePagesDb, [WorkspaceDb]);

    impl WelcomePagesDb {
        query! {
            pub async fn save_welcome_page(
                item_id: crate::ItemId,
                workspace_id: crate::WorkspaceId,
                is_open: bool
            ) -> Result<()> {
                INSERT OR REPLACE INTO welcome_pages(item_id, workspace_id, is_open)
                VALUES (?, ?, ?)
            }
        }

        query! {
            pub fn get_welcome_page(
                item_id: crate::ItemId,
                workspace_id: crate::WorkspaceId
            ) -> Result<bool> {
                SELECT is_open
                FROM welcome_pages
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}

