//! Tauri commands for git worktree management.

use crate::services::git::{self, GitInfo};

#[tauri::command]
pub async fn get_git_info(path: String) -> GitInfo {
    git::probe(&path)
}

#[tauri::command]
pub async fn create_worktree(project_path: String, slug: String) -> Result<String, String> {
    git::create_worktree(&project_path, &slug)
}
