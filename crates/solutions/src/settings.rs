use settings::{RegisterSetting, Settings};
use std::path::PathBuf;

#[derive(Clone, Debug, RegisterSetting)]
pub struct SolutionsSettings {
    pub root: PathBuf,
}

impl Default for SolutionsSettings {
    fn default() -> Self {
        Self {
            root: default_root(),
        }
    }
}

/// Default solutions storage root: `<base_dir>/solutions`. The base
/// directory comes from `paths::base_dir()` (single-folder profile —
/// `~/spk-editor` for release, `~/spk-editor-dev` for debug, or any
/// `set_custom_data_dir` override) so all per-profile state lives in
/// one place.
fn default_root() -> PathBuf {
    paths::base_dir().join("solutions")
}

impl Settings for SolutionsSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let root = match content.solutions.as_ref().and_then(|s| s.root.clone()) {
            Some(raw) => PathBuf::from(shellexpand::tilde(&raw).into_owned()),
            None => default_root(),
        };
        Self { root }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_root_expands_tilde() {
        let s = SolutionsSettings::default();
        assert!(
            !s.root.starts_with("~"),
            "tilde was not expanded: {}",
            s.root.display()
        );
        // Root is `~/spk-editor/solutions` in release and
        // `~/spk-editor-dev/solutions` in debug. Either way the last
        // segment is `solutions` and the parent matches the active
        // base directory name.
        assert!(s.root.ends_with("solutions"));
    }
}
