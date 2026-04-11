use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub encoded_name: String,
    pub actual_path: String,
    pub session_count: usize,
    pub path_exists: bool,
    /// Short git branch name, e.g. `main`, `feat/add-voice`. None if not a git repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// True if this is a linked worktree (not the primary one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_linked_worktree: bool,
    /// Total number of worktrees (1 for a plain repo).
    #[serde(default, skip_serializing_if = "is_default_worktree_count")]
    pub worktree_count: u32,
}

fn is_default_worktree_count(c: &u32) -> bool {
    *c == 0 || *c == 1
}
