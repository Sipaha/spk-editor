mod actions;
mod edit_modal;
mod run_controller;
mod schema_form;
mod status_item;
mod toolbar_strip;

pub use run_controller::{ActiveRun, RunController, RunControllerEvent};

use gpui::App;
use workspace::Workspace;

pub fn init(cx: &mut App) {
    actions::init(cx);
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };

        let project = workspace.project().clone();
        let fs = project.read(cx).fs().clone();
        if let Some(store) = run_config::RunConfigStore::try_global(cx) {
            store.update(cx, |store, cx| store.watch_project(project, fs, cx));
        }

        toolbar_strip::install(workspace, window, cx);
    })
    .detach();
}
