//! Unified bottom-dock panel hosting terminal + AI-chat tabs.

mod actions;
mod chat_provider;
mod console_panel_settings;
mod panel;
mod terminal_provider;

pub use actions::{NewChat, NewTerminal, ToggleFocus};
pub use chat_provider::{ChatProvider, ChatProviderEvent};
pub use console_panel_settings::ConsolePanelSettings;
pub use panel::{ConsolePanel, ConsoleTab};
pub use terminal_provider::TerminalProvider;

pub fn init(cx: &mut gpui::App) {
    use settings::Settings;
    ConsolePanelSettings::register(cx);
}
