//! S-BRP tab enum. The tab bar header is rendered inline in
//! [`super::BranchesPopup::render_tab_bar`] so each pill can dispatch
//! through the popup's `cx.listener` without juggling per-pill `'static`
//! closures.

use std::fmt;

use ui::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Recent,
    Local,
    Remote,
    Tags,
    Favorites,
    Backups,
}

impl Tab {
    pub fn all() -> [Tab; 6] {
        [
            Tab::Recent,
            Tab::Local,
            Tab::Remote,
            Tab::Tags,
            Tab::Favorites,
            Tab::Backups,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Recent => "Recent",
            Tab::Local => "Local",
            Tab::Remote => "Remote",
            Tab::Tags => "Tags",
            Tab::Favorites => "Favorites",
            Tab::Backups => "Backups",
        }
    }

    pub fn icon(&self) -> IconName {
        match self {
            Tab::Recent => IconName::HistoryRerun,
            Tab::Local => IconName::GitBranch,
            Tab::Remote => IconName::Screen,
            Tab::Tags => IconName::Hash,
            Tab::Favorites => IconName::Star,
            Tab::Backups => IconName::CountdownTimer,
        }
    }
}

impl fmt::Display for Tab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}
