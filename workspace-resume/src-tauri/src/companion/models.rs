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
    /// Reserved. Currently never emitted by the poller — `Idle` covers
    /// "Claude exited cleanly". Kept on the wire because the Tauri
    /// frontend (`src/components/ui/StatusChip.tsx`) and Android client
    /// (`Dtos.kt::PaneState.Done`, `StatusColors.Done`, `StatusChip.kt`)
    /// already render a "Done" state. Removing requires a coordinated
    /// 3-language change for no functional gain.
    Done,
}

/// Why a pane is in `PaneState::Waiting`. Carried alongside the state so
/// the apk Waiting tab can render approval-style prompts distinctly from
/// Claude having stopped mid-flow and wanting a follow-up nudge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    /// Claude explicitly asked something of the user: permission prompt,
    /// elicitation dialog, AskUserQuestion, idle_prompt hook, etc.
    Request,
    /// Claude finished a turn and stopped (Stop hook). The user should
    /// decide what to ask next — Claude is not asking anything.
    Continue,
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
    /// Sub-category of `state == Waiting`. Absent for every other state.
    /// `Request` = Claude asked something (approval, elicitation, ...);
    /// `Continue` = Claude stopped (Stop hook) and wants a user nudge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    /// Up to 5 tail lines, ANSI-stripped, from the last capture-pane.
    pub last_output_preview: Vec<String>,
    /// `-home-andrea-pane-management` etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_encoded_name: Option<String>,
    /// Friendly name derived from the matched project's actual_path
    /// basename. Mobile/desktop render this as the card title instead
    /// of falling back to the tmux session_name (which is just "main").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_display_name: Option<String>,
    /// Claude Code JSONL UUID if we've bound one to this pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// Which Claude account the pane is running under, detected from
    /// the child process's `CLAUDE_CONFIG_DIR` env var. `"andrea"` or
    /// `"bravura"` — `None` when the pane isn't a Claude session or we
    /// haven't detected yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_account: Option<String>,
    /// Current `/effort` level detected from the pane's terminal output.
    /// `"low"`, `"medium"`, `"high"`, or `"max"`. Sticky-cached in the
    /// poller: set when `detect_effort` finds a banner (`"with max effort"`)
    /// or echoed `/effort <level>`, cleared on pane renumber / session
    /// change. `None` for non-Claude panes, or Claude panes where neither
    /// signal has been seen yet (e.g. the desktop started after the
    /// banner scrolled off and the user hasn't interacted with the chip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_effort: Option<String>,
    /// Epoch milliseconds of last state/output change.
    pub updated_at: i64,
    /// Epoch milliseconds of the last conversation message — derived from
    /// the bound JSONL file's mtime, so it advances on real Claude turns
    /// (assistant tokens, tool results, user input) but NOT on phone
    /// capture views or state-only transitions. `None` for non-Claude
    /// panes or when the JSONL doesn't exist yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<i64>,
    /// Operator-visible warning set by the companion when it detects an
    /// abnormal state for this pane — e.g., session_id collision with
    /// another pane. UI renders this as a yellow chip with the message
    /// in a tooltip. Cleared the next time the pane's detection completes
    /// cleanly. None for healthy panes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_account: Option<String>,
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

/// Send a single named key (or sequence) to a pane via `tmux send-keys`
/// without the `-l` literal flag, so tmux interprets symbolic names like
/// `S-Tab`, `Enter`, `Up`, `C-c`, etc. Used for mode switching (`S-Tab`),
/// arrow navigation, and any other special-key need that the literal
/// `/input` endpoint can't express.
#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureResponse {
    pub lines: Vec<String>,
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// Attention snapshot (for WebSocket reconnect replay)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionSnapshotDto {
    pub pane_id: String,
    pub title: String,
    pub message: String,
    pub kind: String,
    pub at: i64,
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
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attention: Vec<AttentionSnapshotDto>,
    },
    /// Synthetic event sent on WebSocket connect after the snapshot.
    /// Carries a timestamp so clients can estimate clock skew / latency.
    Hello {
        at: i64,
    },
    PaneStateChanged {
        pane_id: String,
        old: PaneState,
        new: PaneState,
        at: i64,
    },
    /// Fired when a pane's metadata changes out-of-band from state
    /// transitions — e.g. the poller just detected the pane's Claude
    /// account via `/proc/<pid>/environ`. Carries the full updated DTO
    /// so clients can drop their cached copy wholesale.
    PaneUpdated {
        pane: PaneDto,
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
    /// Claude went idle or needs user input — no allow/deny, just a
    /// notification so clients can surface the right level of urgency.
    AttentionNeeded {
        pane_id: String,
        title: String,
        message: String,
        at: i64,
        /// `"input"` when Claude is waiting for user input,
        /// `"info"` for status updates like "Claude finished".
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_display_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        claude_account: Option<String>,
    },
    /// A tmux pane disappeared (window killed, session ended, etc.).
    /// Emitted once per vanished pane so clients can remove cards.
    PaneRemoved {
        pane_id: String,
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
// Conversation (JSONL session transcript)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ConversationMessage {
    pub uuid: String,
    /// `"user"`, `"assistant"`, or `"tool_result"`
    pub role: String,
    /// Concatenated text content (tool_use blocks excluded).
    pub text: String,
    /// ISO 8601 timestamp from the JSONL record.
    pub timestamp: String,
    /// Tool name when this assistant turn contains a tool_use block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool input JSON — e.g. Bash `command`, Edit `file_path`/`old_string`/
    /// `new_string`, Read `file_path`. The client formats a human-readable
    /// summary per tool. Only set when `tool_name` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    /// Thinking block text from the assistant turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Tool result content — the actual output of a tool call.
    /// Truncated server-side to `MAX_TOOL_RESULT_CHARS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    /// True when `tool_result` was truncated to fit the wire budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_truncated: Option<bool>,
    /// True when the tool call errored (`is_error` in the JSONL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationResponse {
    pub session_id: String,
    pub messages: Vec<ConversationMessage>,
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

// ---------------------------------------------------------------------------
// Project list (for mobile launcher sheet)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDto {
    pub encoded_name: String,
    pub display_name: String,
    pub actual_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub session_count: u32,
    pub active_pane_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

// ---------------------------------------------------------------------------
// Window lifecycle (create + kill)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateWindowRequest {
    pub session_name: String,
    pub project_path: String,
    pub project_display_name: String,
    pub account: String, // "andrea" | "bravura"
}

#[derive(Debug, Serialize)]
pub struct CreateWindowResponse {
    pub window_index: u32,
    pub pane_id: String,
}

// ---------------------------------------------------------------------------
// Pane lifecycle (split-window)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePaneRequest {
    /// Target pane id to split from, e.g. "main:3.1"
    pub target_pane_id: String,
    pub account: String, // "andrea" | "bravura"
    /// Split direction: "horizontal" (default, side-by-side) or "vertical" (stacked below).
    /// Optional for wire compatibility with older Android builds that omit the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreatePaneResponse {
    pub pane_id: String,
}

/// Body for `POST /api/v1/panes/{id}/fork`. The target pane id is in the
/// URL path; the source session_id is read server-side from AppState.
/// Response reuses `CreatePaneResponse` — caller only needs the new
/// pane's id to navigate there.
#[derive(Debug, Deserialize)]
pub struct ForkPaneRequest {
    pub account: String, // "andrea" | "bravura"
}

// ---------------------------------------------------------------------------
// Rate limits (per-account Anthropic API utilization)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AccountRateLimit {
    pub account: String,
    pub label: String,
    pub five_hour_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day_resets_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Image upload
// ---------------------------------------------------------------------------

fn default_media_type() -> String {
    "image/png".to_string()
}

#[derive(Debug, Deserialize)]
pub struct ImageItem {
    pub image_base64: String,
    #[serde(default = "default_media_type")]
    pub media_type: String,
}

#[derive(Debug, Deserialize)]
pub struct ImageRequest {
    /// One or more images to write to WSL and reference in the message.
    pub images: Vec<ImageItem>,
    /// Optional user prompt to pair with the images.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// Helper: current epoch milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
