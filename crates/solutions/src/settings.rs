use settings::{RegisterSetting, Settings};
use std::path::PathBuf;

#[derive(Clone, Debug, RegisterSetting)]
pub struct SolutionsSettings {
    pub root: PathBuf,
    /// S-SOL-LOG aggregated-log configuration.
    pub aggregated_log: AggregatedLogSettings,
}

#[derive(Clone, Debug)]
pub struct AggregatedLogSettings {
    /// Pre-warm member buffers when a Solution is opened.
    pub background_load: bool,
    /// Hard cap on commits served per aggregated-log session.
    pub max_total_commits: u32,
}

impl Default for AggregatedLogSettings {
    fn default() -> Self {
        Self {
            background_load: true,
            max_total_commits: 50_000,
        }
    }
}

impl Default for SolutionsSettings {
    fn default() -> Self {
        Self {
            root: default_root(),
            aggregated_log: AggregatedLogSettings::default(),
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
        let solutions = content.solutions.as_ref();
        let root = match solutions.and_then(|s| s.root.clone()) {
            Some(raw) => PathBuf::from(shellexpand::tilde(&raw).into_owned()),
            None => default_root(),
        };
        let defaults = AggregatedLogSettings::default();
        let aggregated_log = solutions
            .and_then(|s| s.git.as_ref())
            .and_then(|g| g.aggregated_log.as_ref())
            .map(|a| AggregatedLogSettings {
                background_load: a.background_load.unwrap_or(defaults.background_load),
                max_total_commits: a
                    .max_total_commits
                    .unwrap_or(defaults.max_total_commits),
            })
            .unwrap_or(defaults);
        Self {
            root,
            aggregated_log,
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
        // Root is `~/spk-editor/solutions` in release and
        // `~/spk-editor-dev/solutions` in debug. Either way the last
        // segment is `solutions` and the parent matches the active
        // base directory name.
        assert!(s.root.ends_with("solutions"));
    }

    #[test]
    fn aggregated_log_defaults() {
        let s = SolutionsSettings::default();
        assert!(s.aggregated_log.background_load);
        assert_eq!(s.aggregated_log.max_total_commits, 50_000);
    }
}
