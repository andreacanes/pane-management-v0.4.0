//! Claude Code Notification hook sink.
//!
//! The hook script POSTs the standard Claude Code hook payload here.
//! We create a pending approval, publish to ntfy (for lock-screen push)
//! + broadcast via WS (for foregrounded companion apps), then park the
//! request on a `oneshot` until the user decides or we time out.
//!
//! The hook expects a JSON response:
//!   {"decision": "approve" | "block", "reason": "..."}
//! exit 0 = allow, exit 2 = block (handled by the shell wrapper).

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{
    error::AppError,
    models::{now_ms, ApprovalDto, Decision, EventDto},
    state::{AppState, NtfyMessage, PendingApproval},
};

const APPROVAL_TTL_MS: i64 = 120_000;

#[derive(Debug, Deserialize)]
pub struct HookPayload {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HookReply {
    pub decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub async fn notification(
    State(state): State<AppState>,
    Json(payload): Json<HookPayload>,
) -> Result<Json<HookReply>, AppError> {
    // Only intercept permission prompts; pass everything else through.
    let is_permission = payload
        .matcher
        .as_deref()
        .map(|m| m == "permission_prompt")
        .unwrap_or(false);
    if !is_permission {
        return Ok(Json(HookReply {
            decision: "approve",
            reason: None,
        }));
    }

    // Try to resolve a pane from cwd / session_id.
    let pane_id = resolve_pane(&state, &payload)
        .await
        .unwrap_or_else(|| "unknown".to_string());

    let id = Uuid::new_v4();
    let now = now_ms();
    let dto = ApprovalDto {
        id,
        pane_id: pane_id.clone(),
        title: payload
            .tool_name
            .clone()
            .unwrap_or_else(|| "Claude request".into()),
        message: payload
            .message
            .clone()
            .or_else(|| payload.tool_input.as_ref().map(|v| v.to_string()))
            .unwrap_or_default(),
        tool_name: payload.tool_name.clone(),
        tool_input: payload.tool_input.clone(),
        created_at: now,
        expires_at: now + APPROVAL_TTL_MS,
    };

    let (tx, rx) = oneshot::channel::<super::models::ApprovalResponse>();

    // Register pending approval
    state.approvals.write().await.insert(
        id,
        PendingApproval {
            dto: dto.clone(),
            responder: tx,
        },
    );

    // Broadcast to WebSocket clients (the pane-management app)
    let _ = state.events.send(EventDto::ApprovalCreated {
        approval: dto.clone(),
    });

    // Fire ntfy push (the F-Droid ntfy app on lock screen)
    publish_ntfy_approval(&state, &dto).await;

    // Park until the user responds or the ttl expires.
    let reply = match tokio::time::timeout(Duration::from_millis(APPROVAL_TTL_MS as u64), rx).await
    {
        Ok(Ok(resp)) => match resp.decision {
            Decision::Allow => HookReply {
                decision: "approve",
                reason: resp.reason,
            },
            Decision::Deny => HookReply {
                decision: "block",
                reason: resp.reason,
            },
        },
        Ok(Err(_)) => HookReply {
            decision: "block",
            reason: Some("approval channel closed".into()),
        },
        Err(_) => HookReply {
            decision: "block",
            reason: Some("timeout".into()),
        },
    };

    // Clean up + broadcast resolution
    state.approvals.write().await.remove(&id);
    let decision = if reply.decision == "approve" {
        Decision::Allow
    } else {
        Decision::Deny
    };
    let _ = state.events.send(EventDto::ApprovalResolved {
        id,
        decision,
        at: now_ms(),
    });

    Ok(Json(reply))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn resolve_pane(state: &AppState, payload: &HookPayload) -> Option<String> {
    let panes = state.panes.read().await;
    // Prefer cwd match
    if let Some(cwd) = payload.cwd.as_ref() {
        for rec in panes.values() {
            if rec.dto.current_path == *cwd {
                return Some(rec.dto.id.clone());
            }
        }
    }
    // Fall back to claude_session_id match
    if let Some(sid) = payload.session_id.as_ref() {
        for rec in panes.values() {
            if rec.dto.claude_session_id.as_deref() == Some(sid.as_str()) {
                return Some(rec.dto.id.clone());
            }
        }
    }
    None
}

async fn publish_ntfy_approval(state: &AppState, dto: &ApprovalDto) {
    let host = &state.bind_addr;
    let bearer = state.bearer.read().await.clone();
    // ntfy X-Actions format: semicolon-separated action entries.
    let actions = format!(
        "http, Allow, http://{host}/api/v1/approvals/{id}, method=POST, headers.Authorization=Bearer {b}, headers.Content-Type=application/json, body={allow_body}; \
         http, Deny, http://{host}/api/v1/approvals/{id}, method=POST, headers.Authorization=Bearer {b}, headers.Content-Type=application/json, body={deny_body}",
        host = host,
        id = dto.id,
        b = bearer,
        allow_body = r#"{"decision":"allow"}"#,
        deny_body = r#"{"decision":"deny"}"#,
    );

    let msg = NtfyMessage {
        id: dto.id.to_string(),
        time: chrono::Utc::now().timestamp(),
        topic: state.ntfy_topic.to_string(),
        event: "message".to_string(),
        title: Some(format!("Claude: {}", dto.title)),
        message: if dto.message.is_empty() {
            "Approval requested".to_string()
        } else {
            dto.message.clone()
        },
        priority: 4,
        tags: vec!["robot".to_string(), "question".to_string()],
        actions: Some(actions),
    };

    {
        let mut backlog = state.ntfy_backlog.write().await;
        backlog.push(msg.clone());
        let overflow = backlog.len().saturating_sub(64);
        if overflow > 0 {
            backlog.drain(0..overflow);
        }
    }
    let _ = state.ntfy_events.send(msg);
}

// Not used yet but kept for parity with the hook script's "permission_prompt" matcher.
#[allow(dead_code)]
pub fn hook_script_template(companion_url: &str, secret_env: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
# Auto-generated by pane-management companion. Edit at your own risk.
exec curl -sS -X POST \
  -H "Content-Type: application/json" \
  -H "X-Hook-Secret: ${secret_env}" \
  --max-time 130 \
  --data-binary @- \
  {companion_url}/api/v1/hooks/notification
"#
    )
}

/// Placeholder import to silence unused warnings on the `StatusCode`
/// import if the module ever drops all status-code returns.
#[allow(dead_code)]
fn _silence() -> StatusCode {
    StatusCode::OK
}
