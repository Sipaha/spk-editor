use std::sync::Arc;

use agent_client_protocol::schema as acp;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use futures::{FutureExt, future::Shared};
use gpui::{App, BackgroundExecutor, Global, SharedString, Task};
use indoc::indoc;
use parking_lot::Mutex;
use solutions::SolutionId;
use sqlez::connection::Connection;

use crate::model::{SolutionSessionId, SolutionSessionMetadata};

pub struct SolutionAgentDb {
    executor: BackgroundExecutor,
    connection: Arc<Mutex<Connection>>,
}

struct GlobalSolutionAgentDb(Shared<Task<Result<Arc<SolutionAgentDb>, Arc<anyhow::Error>>>>);

impl Global for GlobalSolutionAgentDb {}

impl SolutionAgentDb {
    pub fn connect(cx: &mut App) -> Shared<Task<Result<Arc<SolutionAgentDb>, Arc<anyhow::Error>>>> {
        if cx.has_global::<GlobalSolutionAgentDb>() {
            return cx.global::<GlobalSolutionAgentDb>().0.clone();
        }
        let executor = cx.background_executor().clone();
        let task = executor
            .spawn({
                let executor = executor.clone();
                async move {
                    match Self::open(executor) {
                        Ok(db) => Ok(Arc::new(db)),
                        Err(err) => Err(Arc::new(err)),
                    }
                }
            })
            .shared();
        cx.set_global(GlobalSolutionAgentDb(task.clone()));
        task
    }

    pub fn open(executor: BackgroundExecutor) -> Result<Self> {
        let connection = if cfg!(any(feature = "test-support", test)) {
            let thread = std::thread::current();
            Connection::open_memory(Some(&format!(
                "SOLUTION_AGENT_TEST_{}",
                thread.name().unwrap_or_default()
            )))
        } else {
            let dir = paths::data_dir().join("solution_agent");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("solution_agent.db");
            Connection::open_file(&path.to_string_lossy())
        };

        connection.exec(indoc! {"
            CREATE TABLE IF NOT EXISTS solution_sessions (
                id                TEXT PRIMARY KEY,
                solution_id       TEXT NOT NULL,
                agent_id          TEXT NOT NULL,
                acp_session_id    TEXT NOT NULL,
                title             TEXT NOT NULL,
                created_at        INTEGER NOT NULL,
                last_activity_at  INTEGER NOT NULL,
                acp_thread_blob   BLOB
            )
        "})?()
        .map_err(|e| anyhow!("Failed to create solution_sessions table: {}", e))?;

        connection.exec(indoc! {"
            CREATE INDEX IF NOT EXISTS idx_session_by_solution
                ON solution_sessions (solution_id, last_activity_at DESC)
        "})?()
        .map_err(|e| anyhow!("Failed to create idx_session_by_solution: {}", e))?;

        Ok(Self {
            executor,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn save_metadata(&self, meta: SolutionSessionMetadata) -> Task<Result<()>> {
        let connection = self.connection.clone();
        self.executor.spawn(async move {
            let connection = connection.lock();
            insert_or_update_metadata(&connection, &meta)
        })
    }

    pub fn list_for_solution(
        &self,
        solution_id: SolutionId,
    ) -> Task<Result<Vec<SolutionSessionMetadata>>> {
        let connection = self.connection.clone();
        self.executor.spawn(async move {
            let connection = connection.lock();
            select_metadata_for_solution(&connection, &solution_id)
        })
    }
}

fn insert_or_update_metadata(
    connection: &Connection,
    meta: &SolutionSessionMetadata,
) -> Result<()> {
    let mut insert = connection.exec_bound::<(String, String, String, Arc<str>, String, i64, i64)>(indoc! {"
        INSERT INTO solution_sessions (
            id, solution_id, agent_id, acp_session_id, title, created_at, last_activity_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(id) DO UPDATE SET
            solution_id      = excluded.solution_id,
            agent_id         = excluded.agent_id,
            acp_session_id   = excluded.acp_session_id,
            title            = excluded.title,
            created_at       = excluded.created_at,
            last_activity_at = excluded.last_activity_at
    "})?;

    insert((
        meta.id.to_string(),
        meta.solution_id.0.clone(),
        meta.agent_id.to_string(),
        meta.acp_session_id.0.clone(),
        meta.title.to_string(),
        meta.created_at.timestamp_millis(),
        meta.last_activity_at.timestamp_millis(),
    ))?;

    Ok(())
}

fn select_metadata_for_solution(
    connection: &Connection,
    solution_id: &SolutionId,
) -> Result<Vec<SolutionSessionMetadata>> {
    let mut select = connection
        .select_bound::<String, (String, String, String, Arc<str>, String, i64, i64)>(indoc! {"
            SELECT id, solution_id, agent_id, acp_session_id, title, created_at, last_activity_at
            FROM solution_sessions
            WHERE solution_id = ?
            ORDER BY last_activity_at DESC
        "})?;

    let rows = select(solution_id.0.clone())?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, solution_id, agent_id, acp_session_id, title, created_at, last_activity_at) in rows {
        let id = SolutionSessionId::parse(&id)
            .map_err(|e| anyhow!("invalid SolutionSessionId in db: {e}"))?;
        let created_at = DateTime::<Utc>::from_timestamp_millis(created_at)
            .ok_or_else(|| anyhow!("invalid created_at timestamp: {created_at}"))?;
        let last_activity_at = DateTime::<Utc>::from_timestamp_millis(last_activity_at)
            .ok_or_else(|| anyhow!("invalid last_activity_at timestamp: {last_activity_at}"))?;

        out.push(SolutionSessionMetadata {
            id,
            solution_id: SolutionId(solution_id),
            agent_id: SharedString::from(agent_id),
            acp_session_id: acp::SessionId::new(acp_session_id),
            title: SharedString::from(title),
            created_at,
            last_activity_at,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_meta(seq: u32, sol: &str) -> SolutionSessionMetadata {
        SolutionSessionMetadata {
            id: SolutionSessionId::new(),
            solution_id: SolutionId(sol.into()),
            agent_id: SharedString::from("claude-acp"),
            acp_session_id: acp::SessionId::new(format!("acp-{seq}")),
            title: SharedString::from(format!("session {seq}")),
            created_at: Utc
                .timestamp_millis_opt(1_700_000_000_000 + seq as i64 * 1000)
                .unwrap(),
            last_activity_at: Utc
                .timestamp_millis_opt(1_700_000_000_000 + seq as i64 * 1000)
                .unwrap(),
        }
    }

    #[gpui::test]
    async fn save_then_list_returns_inserted_rows(cx: &mut gpui::TestAppContext) {
        let executor = cx.executor();
        let db = SolutionAgentDb::open(executor).unwrap();

        db.save_metadata(make_meta(1, "sol-a")).await.unwrap();
        db.save_metadata(make_meta(2, "sol-a")).await.unwrap();
        db.save_metadata(make_meta(3, "sol-b")).await.unwrap();

        let in_a = db
            .list_for_solution(SolutionId("sol-a".into()))
            .await
            .unwrap();
        assert_eq!(in_a.len(), 2);
        let in_b = db
            .list_for_solution(SolutionId("sol-b".into()))
            .await
            .unwrap();
        assert_eq!(in_b.len(), 1);
        let in_c = db
            .list_for_solution(SolutionId("sol-c".into()))
            .await
            .unwrap();
        assert_eq!(in_c.len(), 0);
    }
}
