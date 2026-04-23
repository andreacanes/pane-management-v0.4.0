use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct TmuxSession {
    pub name: String,
    pub windows: usize,
    pub attached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub panes: u32,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TmuxPane {
    pub pane_id: String,
    pub pane_index: u32,
    pub width: u32,
    pub height: u32,
    pub top: u32,
    pub left: u32,
    pub active: bool,
    pub current_command: String,
    pub current_path: String,
    /// Original command the pane was started with (e.g. "claude -r <uuid>").
    /// Survives even while Claude spawns child processes that briefly
    /// override `current_command`. Empty if tmux doesn't report one.
    #[serde(default)]
    pub start_command: String,
    /// The pane's top-level process PID (usually the shell), used for
    /// /proc/<pid>/environ walks to detect which Claude profile is
    /// running under it. Empty if tmux didn't report one.
    #[serde(default)]
    pub pane_pid: String,
    /// `"andrea"` or `"bravura"` — detected from the child process's
    /// `CLAUDE_CONFIG_DIR` env var. `None` if the pane isn't running
    /// Claude or detection hasn't completed yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_account: Option<String>,
    /// Window index this pane belongs to — stamped by the caller since
    /// the per-window list_tmux_panes format string omits it.
    #[serde(default)]
    pub window_index: u32,
    /// Short git branch name at current_path. Populated via
    /// services::git::probe_many; None when the cwd isn't a git repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// True when current_path is a linked (non-primary) git worktree.
    #[serde(default)]
    pub is_worktree: bool,
    /// Host the pane lives on: `"local"` for WSL tmux, `"mac"` (or any
    /// SSH alias) for a remote tmux server. Stamped by the list-panes
    /// caller. The host + session + window + pane_index tuple uniquely
    /// identifies the pane across all reachable tmux servers; this is
    /// the coordinate used by `pane_assignments` keys and by every
    /// operation that routes through `HostTarget`.
    #[serde(default = "default_local_host")]
    pub host: String,
    /// tmux session this pane belongs to — stamped by the caller since
    /// the per-session `list_tmux_panes` format string omits it. Empty
    /// when a parser without context produces the pane (callers should
    /// always stamp before returning to the wire).
    #[serde(default)]
    pub session_name: String,
}

// Used by `#[serde(default = "...")]` on `TmuxPane::host`. Only fires
// on deserialize, which TmuxPane doesn't currently derive — kept for
// the day we add Deserialize (the migration plan leaves that door open
// without needing another model tweak).
#[allow(dead_code)]
fn default_local_host() -> String {
    "local".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct TmuxState {
    pub sessions: Vec<TmuxSession>,
    pub windows: Vec<TmuxWindow>,
    pub panes: Vec<TmuxPane>,
}

/// Per-window status: which panes are running Claude, their paths,
/// and which panes are waiting for user approval.
#[derive(Debug, Clone, Serialize)]
pub struct WindowPaneStatus {
    pub has_active: bool,
    pub active_panes: Vec<u32>,
    pub active_paths: Vec<String>,
    pub waiting_panes: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tmux_session_serialize() {
        let session = TmuxSession {
            name: "workspace".to_string(),
            windows: 3,
            attached: true,
        };
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("\"name\":\"workspace\""));
        assert!(json.contains("\"windows\":3"));
        assert!(json.contains("\"attached\":true"));

        // Round-trip via Value
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "workspace");
        assert_eq!(value["windows"], 3);
        assert_eq!(value["attached"], true);
    }

    #[test]
    fn test_tmux_window_serialize() {
        let window = TmuxWindow {
            index: 0,
            name: "editor".to_string(),
            panes: 2,
            active: true,
        };
        let json = serde_json::to_string(&window).unwrap();
        assert!(json.contains("\"index\":0"));
        assert!(json.contains("\"name\":\"editor\""));
        assert!(json.contains("\"panes\":2"));
        assert!(json.contains("\"active\":true"));
    }

    #[test]
    fn test_tmux_pane_serialize() {
        let pane = TmuxPane {
            pane_id: "%5".to_string(),
            pane_index: 0,
            width: 80,
            height: 24,
            top: 0,
            left: 0,
            active: true,
            current_command: "bash".to_string(),
            current_path: "/home/user".to_string(),
            start_command: String::new(),
            pane_pid: String::new(),
            claude_account: None,
            window_index: 0,
            git_branch: None,
            is_worktree: false,
            host: "local".to_string(),
            session_name: "main".to_string(),
        };
        let json = serde_json::to_string(&pane).unwrap();
        assert!(json.contains("\"pane_id\":\"%5\""));
        assert!(json.contains("\"pane_index\":0"));
        assert!(json.contains("\"width\":80"));
        assert!(json.contains("\"height\":24"));
        assert!(json.contains("\"current_command\":\"bash\""));
    }

    #[test]
    fn test_tmux_state_serialize() {
        let state = TmuxState {
            sessions: vec![TmuxSession {
                name: "test".to_string(),
                windows: 1,
                attached: false,
            }],
            windows: vec![TmuxWindow {
                index: 0,
                name: "main".to_string(),
                panes: 1,
                active: true,
            }],
            panes: vec![TmuxPane {
                pane_id: "%0".to_string(),
                pane_index: 0,
                width: 120,
                height: 40,
                top: 0,
                left: 0,
                active: true,
                current_command: "zsh".to_string(),
                current_path: "/tmp".to_string(),
                start_command: String::new(),
                pane_pid: String::new(),
                claude_account: None,
                window_index: 0,
                git_branch: None,
                is_worktree: false,
                host: "local".to_string(),
                session_name: "test".to_string(),
            }],
        };
        let json = serde_json::to_string(&state).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(value["windows"].as_array().unwrap().len(), 1);
        assert_eq!(value["panes"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_tmux_state_empty() {
        let state = TmuxState {
            sessions: vec![],
            windows: vec![],
            panes: vec![],
        };
        let json = serde_json::to_string(&state).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["sessions"].as_array().unwrap().is_empty());
        assert!(value["windows"].as_array().unwrap().is_empty());
        assert!(value["panes"].as_array().unwrap().is_empty());
    }
}
