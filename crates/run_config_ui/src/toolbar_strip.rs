use gpui::{Context, Window};
use workspace::Workspace;

/// Installs the Run Configurations toolbar strip (+ the per-workspace
/// `RunController`) on the given workspace. Filled in by Task 15; for now it's
/// a no-op so `run_config_ui::init` compiles.
pub fn install(_workspace: &mut Workspace, _window: &mut Window, _cx: &mut Context<Workspace>) {}
