mod debug;
mod shell;
mod task_ref;

pub(crate) fn register_builtin(cx: &mut gpui::App) {
    crate::store::register_provider(cx, shell::ShellProvider);
}
