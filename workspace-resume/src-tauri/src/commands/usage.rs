//! Tauri commands exposing Claude Code usage data to the frontend.

use std::path::PathBuf;

use crate::services::usage::{self, ProjectUsage};
use crate::services::wsl;

/// Resolve the project directory the same way discovery does — WSL first, Windows home as fallback.
fn resolve_project_dir(encoded: &str) -> Option<PathBuf> {
    if let Some(info) = wsl::wsl_info() {
        let wsl_dir = info.claude_projects_unc().join(encoded);
        if wsl_dir.exists() {
            return Some(wsl_dir);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let win_dir = home.join(".claude").join("projects").join(encoded);
        if win_dir.exists() {
            return Some(win_dir);
        }
    }
    None
}

/// Aggregate usage for a single project by encoded name.
#[tauri::command]
pub async fn get_project_usage(encoded_name: String) -> Result<ProjectUsage, String> {
    let dir = resolve_project_dir(&encoded_name)
        .ok_or_else(|| format!("Project directory not found: {}", encoded_name))?;
    Ok(usage::parse_project_usage(&dir, &encoded_name))
}

/// Aggregate usage for every project on disk, keyed by encoded name.
/// Heavier than `get_project_usage`; cache the result on the frontend.
#[tauri::command]
pub async fn get_all_usage() -> Result<std::collections::HashMap<String, ProjectUsage>, String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(info) = wsl::wsl_info() {
        let p = info.claude_projects_unc();
        if p.exists() {
            roots.push(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("projects");
        if p.exists() {
            roots.push(p);
        }
    }
    let root_refs: Vec<&std::path::Path> = roots.iter().map(|p| p.as_path()).collect();
    Ok(usage::parse_all_projects_usage(&root_refs))
}

/// Summary row for the global dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSummary {
    pub projects: u32,
    pub sessions: u32,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_write: u64,
    pub total_cache_read: u64,
    pub total_cost_usd: f64,
}

#[tauri::command]
pub async fn get_usage_summary() -> Result<UsageSummary, String> {
    let all = get_all_usage().await?;
    let mut sum = UsageSummary {
        projects: 0,
        sessions: 0,
        total_input: 0,
        total_output: 0,
        total_cache_write: 0,
        total_cache_read: 0,
        total_cost_usd: 0.0,
    };
    for (_, proj) in all.iter() {
        sum.projects += 1;
        sum.sessions += proj.sessions.len() as u32;
        sum.total_input += proj.total_input;
        sum.total_output += proj.total_output;
        sum.total_cache_write += proj.total_cache_write;
        sum.total_cache_read += proj.total_cache_read;
        sum.total_cost_usd += proj.total_cost_usd;
    }
    Ok(sum)
}
