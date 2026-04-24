//! Notification audit log — structured NDJSON log of every notification
//! decision (fired, suppressed, cancelled, resolved) with timestamps
//! correlatable to Claude Code JSONL sessions.
//!
//! The writer runs in a dedicated tokio task. Callers use `AuditLog::log()`
//! which is fire-and-forget via an unbounded mpsc channel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use super::models::now_ms;

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub ts: String,
    pub ts_ms: i64,
    #[serde(flatten)]
    pub event: AuditEvent,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[allow(dead_code)] // `HookReceived` is reserved for a future per-hook
                    // ingress audit trail that isn't wired yet (we only
                    // log Fired/Suppressed today). Keep the variant so
                    // the wire format is forward-compatible when we add
                    // it — deleting would require a wire-breaking change.
pub enum AuditEvent {
    HookReceived {
        pane_id: Option<String>,
        session_id: Option<String>,
        hook_event: String,
        matcher: String,
        resolved: bool,
    },
    Fired {
        pane_id: String,
        session_id: Option<String>,
        hook_event: String,
        matcher: String,
        channel: String,
        title: String,
        body: String,
    },
    Suppressed {
        pane_id: String,
        session_id: Option<String>,
        hook_event: String,
        matcher: String,
        reason: String,
    },
    Cancelled {
        pane_id: String,
        notification_type: String,
        reason: String,
    },
    ApprovalResolved {
        pane_id: String,
        approval_id: String,
        decision: String,
    },
}

pub struct AuditLog {
    seq: AtomicU64,
    tx: mpsc::UnboundedSender<AuditEntry>,
}

impl AuditLog {
    pub fn spawn(data_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(writer_task(data_dir, rx));
        Self {
            seq: AtomicU64::new(1),
            tx,
        }
    }

    pub fn log(&self, event: AuditEvent) {
        let now = now_ms();
        let entry = AuditEntry {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ts_ms: now,
            event,
        };
        let _ = self.tx.send(entry);
    }
}

async fn writer_task(data_dir: PathBuf, mut rx: mpsc::UnboundedReceiver<AuditEntry>) {
    let audit_dir = data_dir.join("audit");
    if let Err(e) = tokio::fs::create_dir_all(&audit_dir).await {
        tracing::warn!(?e, "failed to create audit log directory");
        return;
    }

    let mut current_date = String::new();
    let mut file: Option<tokio::fs::File> = None;

    while let Some(entry) = rx.recv().await {
        let date = &entry.ts[..10]; // "2026-04-15"
        if date != current_date {
            current_date = date.to_string();
            let path = audit_dir.join(format!("notifications-{}.ndjson", current_date));
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
            {
                Ok(f) => file = Some(f),
                Err(e) => {
                    tracing::warn!(?e, path = %path.display(), "failed to open audit log");
                    file = None;
                    continue;
                }
            }
        }

        if let Some(f) = file.as_mut() {
            if let Ok(mut line) = serde_json::to_string(&entry) {
                line.push('\n');
                if let Err(e) = f.write_all(line.as_bytes()).await {
                    tracing::warn!(?e, "failed to write audit log entry");
                }
            }
        }
    }
}

/// Read audit entries from today's log file. Returns up to `limit` entries,
/// optionally filtered to those with `ts_ms >= since_ms`.
pub async fn read_recent(
    data_dir: &std::path::Path,
    limit: usize,
    since_ms: Option<i64>,
) -> Vec<serde_json::Value> {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let path = data_dir.join("audit").join(format!("notifications-{}.ndjson", date));
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut entries: Vec<serde_json::Value> = content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|v: &serde_json::Value| {
            if let Some(since) = since_ms {
                v.get("ts_ms")
                    .and_then(|t| t.as_i64())
                    .map(|t| t >= since)
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .collect();

    // Return the last `limit` entries (most recent).
    let start = entries.len().saturating_sub(limit);
    entries.drain(..start);
    entries
}
