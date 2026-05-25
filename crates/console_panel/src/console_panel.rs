//! Unified bottom-dock panel hosting terminal + AI-chat tabs.

mod console_panel_settings;
pub use console_panel_settings::ConsolePanelSettings;

pub fn init(cx: &mut gpui::App) {
    use settings::Settings;
    ConsolePanelSettings::register(cx);
}
