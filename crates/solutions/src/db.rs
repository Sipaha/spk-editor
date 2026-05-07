//! SQLite persistence for the Solutions registry. Replaces the previous
//! solutions.json file. Schema is owned by the Domain impl on
//! SolutionsDb; queries live in impl SolutionsDb blocks.

use db::sqlez::domain::Domain;
use db::sqlez::thread_safe_connection::ThreadSafeConnection;
use db::sqlez_macros::sql;

pub struct SolutionsDb(ThreadSafeConnection);

impl Domain for SolutionsDb {
    const NAME: &str = stringify!(SolutionsDb);

    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE catalog_projects (
            id             TEXT PRIMARY KEY,
            name           TEXT NOT NULL,
            remote_url     TEXT NOT NULL,
            default_branch TEXT
        );

        CREATE TABLE solutions (
            id             TEXT PRIMARY KEY,
            name           TEXT NOT NULL,
            root           TEXT NOT NULL,
            last_opened_at INTEGER
        );

        CREATE TABLE solution_members (
            solution_id  TEXT    NOT NULL REFERENCES solutions(id) ON DELETE CASCADE,
            catalog_id   TEXT    NOT NULL,
            local_path   TEXT    NOT NULL,
            position     INTEGER NOT NULL,
            PRIMARY KEY (solution_id, catalog_id)
        );

        CREATE INDEX idx_solution_members_position
            ON solution_members(solution_id, position);

        CREATE TABLE panel_member_selections (
            solution_id  TEXT NOT NULL REFERENCES solutions(id) ON DELETE CASCADE,
            panel_kind   TEXT NOT NULL,
            catalog_id   TEXT NOT NULL,
            PRIMARY KEY (solution_id, panel_kind)
        );
    )];
}

db::static_connection!(SolutionsDb, []);

/// Identifies which panel a panel_member_selections row belongs to.
/// Stored as the literal string in the SQL panel_kind column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Tree,
    Git,
}

impl PanelKind {
    pub fn as_sql_str(self) -> &'static str {
        match self {
            PanelKind::Tree => "tree",
            PanelKind::Git => "git",
        }
    }

    pub fn from_sql_str(s: &str) -> Option<PanelKind> {
        match s {
            "tree" => Some(PanelKind::Tree),
            "git" => Some(PanelKind::Git),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn open_test_db_applies_migration() {
        let db = SolutionsDb::open_test_db("solutions_db_open_test").await;
        db.write(|conn| {
            conn.exec("INSERT INTO catalog_projects (id, name, remote_url) VALUES ('x', 'X', 'u')")
                .unwrap()()
                .unwrap();
            conn.exec("DELETE FROM catalog_projects WHERE id = 'x'")
                .unwrap()()
                .unwrap();
        })
        .await;
    }

    #[test]
    fn panel_kind_round_trips_sql_str() {
        assert_eq!(PanelKind::from_sql_str("tree"), Some(PanelKind::Tree));
        assert_eq!(PanelKind::from_sql_str("git"), Some(PanelKind::Git));
        assert_eq!(PanelKind::from_sql_str("xxx"), None);
        assert_eq!(PanelKind::Tree.as_sql_str(), "tree");
        assert_eq!(PanelKind::Git.as_sql_str(), "git");
    }
}
