use gpui::SharedString;
use solutions::Solution;
use ui::IconName;

use crate::adapter::SolutionAgentAdapter;
use crate::model::AgentServerId;

pub const CLAUDE_ACP_AGENT_ID: &str = "claude-acp";

pub struct ClaudeAcpAdapter;

impl SolutionAgentAdapter for ClaudeAcpAdapter {
    fn agent_id(&self) -> AgentServerId {
        SharedString::from(CLAUDE_ACP_AGENT_ID)
    }

    fn display_name(&self) -> SharedString {
        SharedString::from("Claude")
    }

    fn icon(&self) -> IconName {
        IconName::AiClaude
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn build_initial_system_prompt(&self, solution: &Solution) -> String {
        let mut buf = String::new();
        buf.push_str("You are working inside a Solution — a multi-project workspace.\n\n");
        buf.push_str(&format!("Solution root: {}\n", solution.root.display()));
        buf.push_str("Member projects (subdirectories you can navigate freely):\n");
        if solution.members.is_empty() {
            buf.push_str("  (none yet — solution is empty)\n");
        } else {
            for member in &solution.members {
                let label = member
                    .local_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| member.local_path.display().to_string());
                buf.push_str(&format!("  - {label}\n"));
            }
        }
        buf.push_str(
            "\nEach member may contain its own CLAUDE.md with project-specific guidance — \
             read them on demand when working in that subdirectory.\n\n",
        );
        buf.push_str(
            "Build / test / git commands must be run from within a member subdirectory \
             (the solution root has no .git, no Cargo.toml, etc.).\n",
        );
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use solutions::{CatalogId, Solution, SolutionId, SolutionMember};
    use std::path::PathBuf;

    fn solution(members: Vec<&str>) -> Solution {
        Solution {
            id: SolutionId("sol-x".into()),
            name: "test".into(),
            root: PathBuf::from("/tmp/sol-x"),
            members: members
                .into_iter()
                .map(|m| SolutionMember {
                    catalog_id: CatalogId(format!("cat-{m}")),
                    local_path: PathBuf::from(format!("/tmp/sol-x/{m}")),
                })
                .collect(),
            last_opened_at: Some(Utc::now()),
        }
    }

    #[test]
    fn prompt_lists_members_and_includes_root_path() {
        let sol = solution(vec!["ecos-records", "ecos-app"]);
        let prompt = ClaudeAcpAdapter.build_initial_system_prompt(&sol);
        assert!(prompt.contains("Solution root: /tmp/sol-x"));
        assert!(prompt.contains("- ecos-records"));
        assert!(prompt.contains("- ecos-app"));
        assert!(prompt.contains("CLAUDE.md"));
    }

    #[test]
    fn prompt_handles_empty_solution() {
        let sol = solution(vec![]);
        let prompt = ClaudeAcpAdapter.build_initial_system_prompt(&sol);
        assert!(prompt.contains("(none yet — solution is empty)"));
    }
}
