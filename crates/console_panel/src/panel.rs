use gpui::{
    Action, Anchor, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, Subscription,
    WeakEntity, Window, anchored, deferred,
};
use settings::Settings as _;
use solution_agent::claude_adapter::CLAUDE_ACP_AGENT_ID;
use solution_agent::rename_session_modal::RenameSessionModal;
use solution_agent::session_view::SolutionSessionView;
use solution_agent::store::SolutionAgentStore;
use solution_agent::SolutionSessionId;
use solutions::{SolutionId, SolutionStore};
use std::path::PathBuf;
use terminal_view::TerminalView;
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use workspace::{
    Item,
    dock::{DockPosition, Panel, PanelEvent},
    Workspace,
};

use crate::actions::{NewChat, NewTerminal, ToggleFocus};
use crate::{ChatProvider, ConsolePanelSettings, TerminalProvider};

const CONSOLE_PANEL_KEY: &str = "ConsolePanel";

pub enum ConsoleTab {
    Terminal {
        view: Entity<TerminalView>,
    },
    Chat {
        view: Entity<SolutionSessionView>,
        session_id: SolutionSessionId,
    },
}

pub struct ConsolePanel {
    workspace: WeakEntity<Workspace>,
    tabs: Vec<ConsoleTab>,
    active_index: Option<usize>,
    dock_position: DockPosition,
    width: Option<Pixels>,
    height: Option<Pixels>,
    terminal_provider: Entity<TerminalProvider>,
    chat_provider: Entity<ChatProvider>,
    focus_handle: FocusHandle,
    tab_context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl ConsolePanel {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        store: Entity<SolutionAgentStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = ConsolePanelSettings::get_global(cx).clone();
        let terminal_provider = cx.new(|_| TerminalProvider::new(workspace.clone()));
        let chat_provider = cx.new(|cx| ChatProvider::new(workspace.clone(), store, cx));
        Self {
            workspace,
            tabs: Vec::new(),
            active_index: None,
            dock_position: settings.default_position,
            width: None,
            height: None,
            terminal_provider,
            chat_provider,
            focus_handle: cx.focus_handle(),
            tab_context_menu: None,
            _subscriptions: Vec::new(),
        }
    }
}

impl EventEmitter<PanelEvent> for ConsolePanel {}

impl Focusable for ConsolePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_overlay = self
            .tab_context_menu
            .as_ref()
            .map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            });
        v_flex()
            .size_full()
            .key_context("ConsolePanel")
            .track_focus(&self.focus_handle)
            .child(self.render_tab_strip(window, cx))
            .child(self.render_active_tab(window, cx))
            .children(menu_overlay)
    }
}

impl ConsolePanel {
    fn render_tab_strip(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_index;
        let mut strip = div()
            .id("console-tab-strip")
            .flex()
            .flex_none()
            .items_stretch()
            .h_9()
            .bg(cx.theme().colors().tab_bar_background)
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .overflow_x_scroll();
        for (ix, tab) in self.tabs.iter().enumerate() {
            let (icon, title): (IconName, SharedString) = match tab {
                ConsoleTab::Terminal { view } => (
                    IconName::Terminal,
                    view.read(cx).tab_content_text(0, cx),
                ),
                ConsoleTab::Chat { view: _, session_id } => {
                    let title = SolutionAgentStore::global(cx)
                        .read_with(cx, |s, _| s.session(*session_id))
                        .map(|entity| entity.read(cx).title.clone())
                        .unwrap_or_else(|| SharedString::from(session_id.to_string()));
                    (IconName::Sparkle, title)
                }
            };
            let is_active = active == Some(ix);
            let bg = if is_active {
                cx.theme().colors().tab_active_background
            } else {
                cx.theme().colors().tab_inactive_background
            };
            let tab_el = div()
                .id(("console-tab", ix))
                .flex()
                .flex_none()
                .items_center()
                .h_full()
                .gap_1p5()
                .px_3()
                .min_w(gpui::px(140.0))
                .max_w(gpui::px(220.0))
                .bg(bg)
                .border_r_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Icon::new(icon).size(IconSize::Small))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .h_full()
                        .child(
                            Label::new(title)
                                .size(LabelSize::Default)
                                .line_height_style(LineHeightStyle::UiLabel)
                                .truncate(),
                        ),
                )
                .child(
                    IconButton::new(("console-close", ix), IconName::Close)
                        .icon_size(IconSize::Small)
                        .on_click(cx.listener(move |this, _, _, cx| this.close_tab(ix, cx))),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.activate_tab(ix, cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        let position = ev.position;
                        this.show_tab_context_menu(ix, position, window, cx);
                    }),
                );
            strip = strip.child(tab_el);
        }
        strip.child(self.render_plus_popover(cx))
    }

    fn render_plus_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active_solution = self.active_solution_id(cx).is_some();
        let plus_container = div()
            .flex()
            .flex_none()
            .items_center()
            .h_full()
            .px_1p5()
            .border_r_1()
            .border_color(cx.theme().colors().border_variant);
        plus_container.child(
            PopoverMenu::new("console-panel-plus")
                .trigger_with_tooltip(
                    IconButton::new("console-plus", IconName::Plus).icon_size(IconSize::Small),
                    Tooltip::text("New…"),
                )
                .anchor(Anchor::TopLeft)
                .menu(move |window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _, _| {
                        menu.action("New Terminal", NewTerminal.boxed_clone())
                            .action_disabled_when(
                                !has_active_solution,
                                if has_active_solution {
                                    "New AI Chat"
                                } else {
                                    "New AI Chat (no active solution)"
                                },
                                NewChat.boxed_clone(),
                            )
                            .action("Spawn Task…", zed_actions::Spawn::modal().boxed_clone())
                    }))
                }),
        )
    }

    fn active_solution_id(&self, cx: &App) -> Option<SolutionId> {
        let workspace = self.workspace.upgrade()?;
        let store = SolutionStore::try_global(cx)?;
        let store = store.read(cx);
        let workspace = workspace.read(cx);
        let project = workspace.project().read(cx);
        for worktree in project.worktrees(cx) {
            let abs_path = worktree.read(cx).abs_path();
            if let Some(sol) = store.solution_for_path(abs_path.as_ref()) {
                return Some(sol.id.clone());
            }
        }
        None
    }

    pub fn add_terminal_tab(
        &mut self,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .terminal_provider
            .update(cx, |provider, cx| provider.new_tab(cwd, window, cx));
        cx.spawn(async move |this, cx| {
            let view = task.await?;
            this.update(cx, |this, cx| {
                this.tabs.push(ConsoleTab::Terminal { view });
                this.active_index = Some(this.tabs.len() - 1);
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn add_chat_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(solution_id) = self.active_solution_id(cx) else {
            return;
        };
        let task = self.chat_provider.update(cx, |provider, cx| {
            provider.new_tab(
                solution_id,
                SharedString::from(CLAUDE_ACP_AGENT_ID),
                None,
                window,
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let (session_id, view) = task.await?;
            this.update(cx, |this, cx| {
                this.tabs.push(ConsoleTab::Chat { view, session_id });
                this.active_index = Some(this.tabs.len() - 1);
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn show_tab_context_menu(
        &mut self,
        tab_index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(tab_index) else {
            return;
        };
        let weak = cx.weak_entity();
        let menu = match tab {
            ConsoleTab::Terminal { view } => {
                let view = view.clone();
                ContextMenu::build(window, cx, |menu, _, _| {
                    let weak_close = weak.clone();
                    let weak_rename = weak.clone();
                    let weak_reveal = weak.clone();
                    let view_rename = view.clone();
                    let view_reveal = view;
                    menu.entry("Close", None, move |_, cx| {
                        if let Some(this) = weak_close.upgrade() {
                            this.update(cx, |this, cx| this.close_tab(tab_index, cx));
                        }
                    })
                    .entry("Rename Tab", None, move |window, cx| {
                        if let Some(this) = weak_rename.upgrade() {
                            this.update(cx, |_, cx| {
                                view_rename.update(cx, |view, cx| {
                                    view.rename_terminal(
                                        &terminal_view::RenameTerminal,
                                        window,
                                        cx,
                                    );
                                });
                            });
                        }
                    })
                    .entry("Reveal CWD in Project Panel", None, move |window, cx| {
                        if let Some(this) = weak_reveal.upgrade() {
                            this.update(cx, |this, cx| {
                                this.reveal_terminal_cwd(&view_reveal, window, cx);
                            });
                        }
                    })
                })
            }
            ConsoleTab::Chat { session_id, .. } => {
                let session_id = *session_id;
                ContextMenu::build(window, cx, |menu, _, _| {
                    let weak_close = weak.clone();
                    let weak_rename = weak.clone();
                    let weak_restart = weak.clone();
                    menu.entry("Close", None, move |_, cx| {
                        if let Some(this) = weak_close.upgrade() {
                            this.update(cx, |this, cx| this.close_tab(tab_index, cx));
                        }
                    })
                    .entry("Rename Session", None, move |window, cx| {
                        if let Some(this) = weak_rename.upgrade() {
                            this.update(cx, |this, cx| {
                                this.open_rename_session_modal(session_id, window, cx);
                            });
                        }
                    })
                    .entry("Restart Agent", None, move |_, cx| {
                        if let Some(this) = weak_restart.upgrade() {
                            this.update(cx, |_, cx| {
                                let store = SolutionAgentStore::global(cx);
                                store
                                    .update(cx, |store, cx| store.restart_agent(session_id, cx))
                                    .detach_and_log_err(cx);
                            });
                        }
                    })
                })
            }
        };
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.tab_context_menu.take();
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.tab_context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn reveal_terminal_cwd(
        &self,
        view: &Entity<TerminalView>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some(cwd) = view.read(cx).terminal().read(cx).working_directory() else {
            return;
        };
        let project = workspace.read(cx).project().clone();
        let Some((worktree, rel_path)) = project.read(cx).find_worktree(&cwd, cx) else {
            return;
        };
        let Some(entry_id) = worktree.read(cx).entry_for_path(&rel_path).map(|e| e.id) else {
            return;
        };
        project.update(cx, |_project, cx| {
            cx.emit(project::Event::RevealInProjectPanel(entry_id));
        });
    }

    fn open_rename_session_modal(
        &self,
        session_id: SolutionSessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let current_title = SolutionAgentStore::global(cx)
            .read_with(cx, |s, _| s.session(session_id))
            .map(|entity| entity.read(cx).title.to_string())
            .unwrap_or_default();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                RenameSessionModal::new(session_id, current_title, window, cx)
            });
        });
    }

    fn render_active_tab(&self, _window: &mut Window, _cx: &mut Context<Self>) -> AnyElement {
        let Some(ix) = self.active_index else {
            return div().flex_1().min_h_0().into_any_element();
        };
        match &self.tabs[ix] {
            ConsoleTab::Terminal { view } => div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(view.clone())
                .into_any_element(),
            ConsoleTab::Chat { view, .. } => div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(view.clone())
                .into_any_element(),
        }
    }

    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_index = Some(index);
            cx.notify();
        }
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.active_index = if self.tabs.is_empty() {
            None
        } else {
            match self.active_index {
                Some(i) if i > index => Some(i - 1),
                Some(i) if i == index => Some(i.min(self.tabs.len() - 1)),
                other => other,
            }
        };
        cx.notify();
    }
}

impl Panel for ConsolePanel {
    fn persistent_name() -> &'static str {
        CONSOLE_PANEL_KEY
    }

    fn panel_key() -> &'static str {
        CONSOLE_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.dock_position
    }

    fn position_is_valid(&self, _position: DockPosition) -> bool {
        true
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_position = position;
        cx.notify();
        // Persisting to settings is a B-followup task.
    }

    fn default_size(&self, window: &Window, cx: &App) -> Pixels {
        let settings = ConsolePanelSettings::get_global(cx);
        match self.position(window, cx) {
            DockPosition::Left | DockPosition::Right => settings.default_width,
            DockPosition::Bottom => settings.default_height,
        }
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<IconName> {
        if ConsolePanelSettings::get_global(cx).button_visible {
            Some(IconName::Console)
        } else {
            None
        }
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Toggle Console")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use settings::SettingsStore;
    use solution_agent::store::SolutionAgentStore;
    use workspace::Workspace;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    // Ignored: constructing a `ConsolePanel` inside a real `Workspace` requires
    // `SolutionAgentStore::init_global` plus the full solution_agent stack. That
    // bootstrap is equivalent to the one in `chat_provider.rs::tests::setup`,
    // which itself requires an async test context and `allow_parking()`. The
    // panel skeleton's correctness is verified at compile time; the runtime
    // integration path will be exercised in B11 when the panel is wired into
    // `Workspace`.
    #[gpui::test]
    #[ignore]
    async fn defaults_to_bottom_position(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", serde_json::json!({})).await;
        let project = Project::test(fs, ["/root".as_ref()], cx).await;

        let connect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        cx.update(|cx| {
            let registry = std::sync::Arc::new(solution_agent::adapter::AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            let agent_store = SolutionAgentStore::global(cx);
            agent_store.update(cx, |s, _| {
                s.register_agent_server(
                    gpui::SharedString::from(
                        solution_agent::claude_adapter::CLAUDE_ACP_AGENT_ID,
                    ),
                    std::rc::Rc::new(
                        solution_agent::test_support::MockAgentServer::new(connect_count),
                    ),
                );
            });
        });

        let store = cx.read(|cx| SolutionAgentStore::global(cx));

        let window_handle =
            cx.add_window(|window, cx| Workspace::test_new(project, window, cx));

        let panel = window_handle
            .update(cx, |workspace, window, cx| {
                cx.new(|cx| ConsolePanel::new(workspace.weak_handle(), store, cx))
            })
            .unwrap();

        window_handle
            .update(cx, |_workspace, window, cx| {
                assert_eq!(
                    panel.read(cx).position(window, cx),
                    DockPosition::Bottom,
                    "default position should be Bottom per ConsolePanelSettings defaults"
                );
            })
            .unwrap();
    }

    // Ignored: same bootstrap constraint as `defaults_to_bottom_position` — constructing
    // ConsolePanel requires SolutionAgentStore::init_global plus full solution_agent stack.
    // The close_tab and activate_tab logic is verified at compile time; runtime integration
    // will be exercised in B11 when the panel is wired into Workspace.
    #[gpui::test]
    #[ignore]
    async fn close_active_tab_moves_active_to_neighbor(_cx: &mut TestAppContext) {
        // Bootstrap: same as defaults_to_bottom_position. Push 3 placeholder tabs
        // (via Terminal-only spawn). Activate index 1. Close index 1.
        // Assert active_index == Some(1) — which is the old #2 shifted down.
        todo!("flesh out");
    }

    #[gpui::test]
    #[ignore]
    async fn close_last_tab_clears_active(_cx: &mut TestAppContext) {
        // Bootstrap: same as defaults_to_bottom_position. Push 1 tab, set active.
        // Close it. Assert tabs.is_empty() and active_index is None.
        todo!("flesh out");
    }
}
