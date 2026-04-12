//! Shared companion state: pane store, approvals registry, broadcast channel,
//! bearer token, hook secret, and the embedded ntfy message buffer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use base64::Engine;
use rand::RngCore;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;
use tokio::sync::{broadcast, oneshot, RwLock};
use uuid::Uuid;

use super::models::{ApprovalDto, ApprovalResponse, EventDto, PaneDto, PaneState};

/// Stored per pane: the public DTO plus internal bookkeeping.
#[derive(Debug, Clone)]
pub struct PaneRecord {
    pub dto: PaneDto,
    /// SHA-256 of the last capture-pane output. Used to detect changes.
    pub output_hash: [u8; 32],
    /// When the pane last emitted new output (for Running → Idle decay).
    pub last_output_change: Option<Instant>,
    /// How confident we are in the claude_session_id binding.
    pub binding_confidence: BindingConfidence,
    /// Cached Claude account detection: `None` means "not yet detected
    /// or not a Claude pane", `Some("andrea")` / `Some("bravura")` once
    /// we've read `/proc/<child_pid>/environ`. Caches forever per pane
    /// record — re-detection only happens on a fresh pane record.
    pub claude_account: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindingConfidence {
    /// No Claude session bound.
    None,
    /// Bound via cwd match + recent JSONL activity.
    Heuristic,
    /// Bound via `claude --resume <uuid>` found in pane_start_command.
    Explicit,
}

/// A Claude Code notification-hook request parked on a oneshot until
/// the user decides on their phone or the timeout expires.
pub struct PendingApproval {
    pub dto: ApprovalDto,
    pub responder: oneshot::Sender<ApprovalResponse>,
}

/// An ntfy message we've published to the embedded endpoint. Retained
/// briefly so late subscribers can catch up.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NtfyMessage {
    pub id: String,
    pub time: i64,
    pub topic: String,
    pub event: String, // "message"
    pub title: Option<String>,
    pub message: String,
    pub priority: u8,
    pub tags: Vec<String>,
    /// Raw X-Actions header value, forwarded to clients verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<String>,
}

/// Cached project lookup table used by the tmux poller to attach a
/// `project_encoded_name` / `project_display_name` to each pane DTO.
/// `list_projects` scans WSL via `wsl.exe` and is too expensive to run
/// every 2 s, so the poller refreshes this at most every 30 s.
#[derive(Default)]
pub struct ProjectCache {
    pub fetched_at: Option<Instant>,
    /// Map from a normalized lowercase POSIX path to (encoded_name, display_name).
    pub by_path: HashMap<String, (String, String)>,
}

#[derive(Clone)]
pub struct AppState {
    pub bearer: Arc<RwLock<String>>,
    pub hook_secret: Arc<String>,
    pub ntfy_topic: Arc<String>,
    pub panes: Arc<RwLock<HashMap<String, PaneRecord>>>,
    pub approvals: Arc<RwLock<HashMap<Uuid, PendingApproval>>>,
    pub events: broadcast::Sender<EventDto>,
    /// Small ring buffer of the last N ntfy messages for late subscribers.
    pub ntfy_backlog: Arc<RwLock<Vec<NtfyMessage>>>,
    /// Broadcast of new ntfy messages to live SSE subscribers.
    pub ntfy_events: broadcast::Sender<NtfyMessage>,
    pub started_at: Instant,
    pub bind_addr: String,
    pub project_cache: Arc<RwLock<ProjectCache>>,
    /// Panes flagged as needing user attention via a Claude Code
    /// `idle_prompt` hook. Unlike approvals there is nothing to allow or
    /// deny — the flag sticks until the pane produces fresh output (user
    /// responded) or Claude exits. Cleared by the tmux poller.
    pub attention_panes: Arc<RwLock<HashSet<String>>>,
}

impl AppState {
    /// Load persisted secrets (bearer, hook secret, ntfy topic) from
    /// Tauri store or generate fresh ones on first run.
    pub async fn load_or_init(app: &AppHandle) -> anyhow::Result<Self> {
        let store = app.store("settings.json")?;

        let bearer: String = match store.get("companion.bearer_token") {
            Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
            _ => {
                let tok = generate_token();
                store.set("companion.bearer_token", serde_json::json!(tok));
                tok
            }
        };
        if bearer.is_empty() {
            let tok = generate_token();
            store.set("companion.bearer_token", serde_json::json!(tok));
        }

        let hook_secret: String = match store.get("companion.hook_secret") {
            Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
            _ => {
                let tok = generate_token();
                store.set("companion.hook_secret", serde_json::json!(tok));
                tok
            }
        };

        let ntfy_topic: String = match store.get("companion.ntfy_topic") {
            Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
            _ => {
                let t = format!("pmgmt-{}", short_id());
                store.set("companion.ntfy_topic", serde_json::json!(t));
                t
            }
        };

        let _ = store.save();

        let (events_tx, _) = broadcast::channel::<EventDto>(1024);
        let (ntfy_tx, _) = broadcast::channel::<NtfyMessage>(256);

        Ok(Self {
            bearer: Arc::new(RwLock::new(bearer)),
            hook_secret: Arc::new(hook_secret),
            ntfy_topic: Arc::new(ntfy_topic),
            panes: Arc::new(RwLock::new(HashMap::new())),
            approvals: Arc::new(RwLock::new(HashMap::new())),
            events: events_tx,
            ntfy_backlog: Arc::new(RwLock::new(Vec::with_capacity(64))),
            ntfy_events: ntfy_tx,
            started_at: Instant::now(),
            bind_addr: format!("{}:{}", super::BIND_ADDR, super::COMPANION_PORT),
            project_cache: Arc::new(RwLock::new(ProjectCache::default())),
            attention_panes: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    /// Apply a state transition, emit an event, update `updated_at`.
    pub async fn transition(&self, pane_id: &str, new_state: PaneState) {
        let mut panes = self.panes.write().await;
        if let Some(rec) = panes.get_mut(pane_id) {
            if rec.dto.state != new_state {
                let old = rec.dto.state;
                rec.dto.state = new_state;
                rec.dto.updated_at = super::models::now_ms();
                let _ = self.events.send(EventDto::PaneStateChanged {
                    pane_id: pane_id.to_string(),
                    old,
                    new: new_state,
                    at: rec.dto.updated_at,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn short_id() -> String {
    let mut buf = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}
