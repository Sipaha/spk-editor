//! Unified bottom-dock panel hosting terminal + AI-chat tabs.

mod console_panel_settings;
pub use console_panel_settings::ConsolePanelSettings;

mod chat_provider;
pub use chat_provider::{ChatProvider, ChatProviderEvent};

mod terminal_provider;
pub use terminal_provider::TerminalProvider;

pub fn init(cx: &mut gpui::App) {
    use settings::Settings;
    ConsolePanelSettings::register(cx);
}
