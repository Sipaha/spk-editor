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

fn default_root() -> PathBuf {
    PathBuf::from(shellexpand::tilde("~/spk-editor/solutions").into_owned())
}

impl Settings for SolutionsSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let raw = content
            .solutions
            .as_ref()
            .and_then(|s| s.root.clone())
            .unwrap_or_else(|| "~/spk-editor/solutions".to_string());
        Self {
            root: PathBuf::from(shellexpand::tilde(&raw).into_owned()),
        }
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
        assert!(s.root.ends_with("spk-editor/solutions"));
    }
}
