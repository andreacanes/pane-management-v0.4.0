//! DTOs shared across companion modules.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Pane state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PaneState {
    Idle,
    Running,
    Waiting,
    Done,
}

impl Default for PaneState {
    fn default() -> Self {
        PaneState::Idle
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneDto {
    /// Stable id: `<session>:<window_index>.<pane_index>`
    pub id: String,
    pub session_name: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_index: u32,
    /// `claude`, `bash`, `nvim`, etc.
    pub current_command: String,
    /// POSIX path inside WSL
    pub current_path: String,
    pub state: PaneState,
    /// Up to 5 tail lines, ANSI-stripped, from the last capture-pane.
    pub last_output_preview: Vec<String>,
    /// `-home-andrea-pane-management` etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_encoded_name: Option<String>,
    /// Claude Code JSONL UUID if we've bound one to this pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// Epoch milliseconds of last state/output change.
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDto {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
}

// ---------------------------------------------------------------------------
// Approvals (Claude Code permission prompts)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDto {
    pub id: Uuid,
    pub pane_id: String,
    pub title: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Debug, Deserialize)]
pub struct ApprovalResponse {
    pub decision: Decision,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Pane I/O requests
// ---------------------------------------------------------------------------

fn default_submit() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct InputRequest {
    pub text: String,
    #[serde(default = "default_submit")]
    pub submit: bool,
}

#[derive(Debug, Deserialize)]
pub struct VoiceRequest {
    pub transcript: String,
    #[serde(default = "default_submit")]
    pub submit: bool,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureResponse {
    pub lines: Vec<String>,
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// WebSocket / SSE events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventDto {
    Snapshot {
        panes: Vec<PaneDto>,
        approvals: Vec<ApprovalDto>,
    },
    PaneStateChanged {
        pane_id: String,
        old: PaneState,
        new: PaneState,
        at: i64,
    },
    PaneOutputChanged {
        pane_id: String,
        tail: Vec<String>,
        seq: u64,
        at: i64,
    },
    ApprovalCreated {
        approval: ApprovalDto,
    },
    ApprovalResolved {
        id: Uuid,
        decision: Decision,
        at: i64,
    },
    SessionStarted {
        name: String,
        at: i64,
    },
    SessionEnded {
        name: String,
        at: i64,
    },
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HealthDto {
    pub version: &'static str,
    pub bind: String,
    pub uptime_s: u64,
}

/// Helper: current epoch milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
