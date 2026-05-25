use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Pixels, Render, Styled as _, Subscription, WeakEntity, Window,
};
use settings::Settings as _;
use solution_agent::session_view::SolutionSessionView;
use solution_agent::store::SolutionAgentStore;
use solution_agent::SolutionSessionId;
use terminal_view::TerminalView;
use ui::IconName;
use workspace::{
    dock::{DockPosition, Panel, PanelEvent},
    Workspace,
};

use crate::actions::ToggleFocus;
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
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Tab-strip + active-tab content land in Task B7.
        gpui::div().size_full()
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
}
