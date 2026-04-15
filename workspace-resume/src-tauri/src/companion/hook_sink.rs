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

use std::collections::HashMap;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{
    error::AppError,
    models::{now_ms, ApprovalDto, Decision, EventDto, PaneState},
    state::{AppState, BindingConfidence, NtfyAction, NtfyMessage, PendingApproval},
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
    /// Claude Code newer versions use `notification_type` instead of `matcher`
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Matchers that signal "Claude wants user attention but there's
/// nothing to allow/deny" — any of these funnels into the attention
/// broadcast path and fires a medium-priority `pm_attention`
/// notification on the phone. `idle_prompt` is also routed through
/// here via its own dedicated wrapper that sets a different default
/// title.
fn is_attention_matcher(m: &str) -> bool {
    matches!(
        m,
        "elicitation_dialog" | "user_prompt" | "ask_user_question"
    )
}

#[derive(Debug, Serialize)]
pub struct HookReply {
    pub decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub async fn notification(
    State(state): State<AppState>,
    body: String,
) -> Result<Json<HookReply>, AppError> {
    // Log the raw JSON body so we can diagnose payload format changes
    tracing::info!(raw = %body, "hook_sink raw payload");

    let payload: HookPayload = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid hook payload: {}", e)))?;

    tracing::info!(
        event = ?payload.hook_event_name,
        matcher = ?payload.matcher,
        cwd = ?payload.cwd,
        session_id = ?payload.session_id,
        tool_name = ?payload.tool_name,
        "hook_sink received"
    );

    // Route by (hook_event_name, matcher) — different Claude Code hook
    // events land here through the same endpoint:
    //
    //   Notification + permission_prompt  → park-and-wait approval flow
    //   Notification + idle_prompt        → attention (soft waiting)
    //   Notification + elicitation_dialog → attention
    //   Stop                             → attention ("Claude finished")
    //   PreToolUse + AskUserQuestion     → attention ("Claude question")
    //
    // Everything else returns approve immediately.
    let event = payload.hook_event_name.as_deref().unwrap_or("");
    // Claude Code uses `matcher` in older versions, `notification_type` in newer ones
    let matcher = payload.matcher.as_deref()
        .or(payload.notification_type.as_deref())
        .unwrap_or("");

    match (event, matcher) {
        // Notification hook: permission prompt → full approval flow
        (_, "permission_prompt") => handle_permission_prompt(state, payload).await,
        // Notification hook: idle / elicitation / unknown-attention matchers
        (_, "idle_prompt") => {
            handle_attention_prompt_inner(
                state, payload, "Claude idle", "Claude is waiting for you", "input",
            ).await
        }
        (_, m) if is_attention_matcher(m) => {
            handle_attention_prompt_inner(
                state, payload, "Claude question", "Claude needs your attention", "input",
            ).await
        }
        // Stop hook: Claude finished its turn, instant push
        ("Stop" | "stop", _) => {
            handle_attention_prompt_inner(
                state, payload, "Claude finished", "Claude is done — tap to continue", "info",
            ).await
        }
        // PreToolUse hook: AskUserQuestion about to render
        ("PreToolUse" | "pre_tool_use", _)
            if payload.tool_name.as_deref() == Some("AskUserQuestion") =>
        {
            handle_attention_prompt_inner(
                state, payload, "Claude question", "Claude is asking you a question", "input",
            ).await
        }
        // Elicitation hook: Claude Code asking user a question (AskUserQuestion
        // may route here instead of PreToolUse for some Claude Code versions)
        ("Elicitation" | "elicitation", _) => {
            handle_attention_prompt_inner(
                state, payload, "Claude question", "Claude is asking you a question", "input",
            ).await
        }
        // Anything else — approve and move on
        _ => Ok(Json(HookReply {
            decision: "approve",
            reason: None,
        })),
    }
}

async fn handle_permission_prompt(
    state: AppState,
    payload: HookPayload,
) -> Result<Json<HookReply>, AppError> {
    let pane_id = match resolve_pane(&state, &payload).await {
        Some(id) => id,
        None => {
            return Ok(Json(HookReply {
                decision: "approve",
                reason: None,
            }))
        }
    };

    let (project_name, account) = pane_context(&state, &pane_id).await;

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
        project_display_name: project_name,
        claude_account: account,
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

    // Flip the pane to Waiting so the grid card turns amber immediately.
    // Must happen BEFORE we park on the oneshot so the state flip races the
    // notification rather than the user's response.
    state.transition(&pane_id, PaneState::Waiting).await;

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

    // If this was the last pending approval for this pane, demote from
    // Waiting. The next tmux_poller tick (within 2s) will correct to Idle
    // if Claude actually exited.
    clear_pane_waiting_if_no_pending(&state, &pane_id).await;

    Ok(Json(reply))
}

async fn handle_attention_prompt_inner(
    state: AppState,
    payload: HookPayload,
    default_title: &str,
    default_message: &str,
    kind: &str,
) -> Result<Json<HookReply>, AppError> {
    let pane_id = match resolve_pane(&state, &payload).await {
        Some(id) => id,
        None => {
            return Ok(Json(HookReply {
                decision: "approve",
                reason: None,
            }))
        }
    };

    let (project_name, account) = pane_context(&state, &pane_id).await;

    state.attention_panes.write().await.insert(pane_id.clone());
    state.transition(&pane_id, PaneState::Waiting).await;

    // Title precedence: explicit payload.title > tool_name > default.
    let base_title = payload
        .title
        .clone()
        .or_else(|| payload.tool_name.clone())
        .unwrap_or_else(|| default_title.to_string());
    // Enrich with project name: "[project] Claude idle"
    let title = match &project_name {
        Some(name) => format!("[{}] {}", name, base_title),
        None => base_title,
    };
    let message = payload
        .message
        .clone()
        .unwrap_or_else(|| default_message.to_string());
    let _ = state.events.send(EventDto::AttentionNeeded {
        pane_id,
        title: title.clone(),
        message: message.clone(),
        at: now_ms(),
        kind: kind.to_string(),
        project_display_name: project_name,
        claude_account: account,
    });

    // Push to ntfy so the phone gets a lock-screen notification even
    // when the companion app is killed / phone is sleeping.
    publish_ntfy_attention(&state, &title, &message).await;

    Ok(Json(HookReply {
        decision: "approve",
        reason: None,
    }))
}

/// If no other approvals are still pending for this pane and the pane
/// isn't flagged for idle-attention, transition it out of Waiting. We
/// pick Running optimistically; the 2 s tmux_poller tick will demote to
/// Idle next cycle if Claude actually exited.
async fn clear_pane_waiting_if_no_pending(state: &AppState, pane_id: &str) {
    let still_pending = state
        .approvals
        .read()
        .await
        .values()
        .any(|p| p.dto.pane_id == pane_id);
    if still_pending {
        return;
    }
    let in_attention = state.attention_panes.read().await.contains(pane_id);
    if in_attention {
        return;
    }
    state.transition(pane_id, PaneState::Running).await;
}

// ---------------------------------------------------------------------------
// SessionStart hook — fires once per Claude session start. Authoritative
// binding because the hook command runs in Claude's own process tree
// with $TMUX_PANE inherited from the shell that exec'd Claude.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SessionStartQuery {
    /// Tmux pane id (e.g. `%5`) captured from `$TMUX_PANE` in the hook
    /// shell command. Lets us resolve to the exact pane that owns this
    /// Claude session, even when multiple Claudes share the same cwd.
    #[serde(default)]
    pub tmux_pane: Option<String>,
}

pub async fn session_start(
    State(state): State<AppState>,
    Query(q): Query<SessionStartQuery>,
    body: String,
) -> Result<StatusCode, AppError> {
    let payload: HookPayload = serde_json::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid session_start payload: {}", e)))?;
    let session_id = match payload.session_id.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err(AppError::BadRequest("missing session_id".into())),
    };

    // Prefer the tmux_pane query param — that's the unique pane id that
    // owns Claude's process. Falls back to cwd matching if not provided.
    let pane_id = if let Some(tp) = q.tmux_pane.as_deref().filter(|s| !s.is_empty()) {
        match resolve_tmux_pane(tp).await {
            Some(p) => Some(p),
            None => resolve_pane(&state, &payload).await,
        }
    } else {
        resolve_pane(&state, &payload).await
    };

    if let Some(pane) = pane_id {
        force_bind_session_id(&state, &pane, &session_id).await;
        tracing::info!(
            pane = %pane,
            session = %session_id,
            tmux_pane = ?q.tmux_pane,
            "session_start: bound (Explicit)"
        );
    } else {
        tracing::warn!(
            session = %session_id,
            tmux_pane = ?q.tmux_pane,
            cwd = ?payload.cwd,
            "session_start: could not resolve pane"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve a tmux pane spec (e.g. `%5`, `@3`, or `<session>:<win>.<pane>`)
/// to the canonical `<session>:<window>.<pane>` id used by PaneRecord.
async fn resolve_tmux_pane(tmux_pane: &str) -> Option<String> {
    let script = format!(
        "tmux display-message -t '{}' -p '#{{session_name}}:#{{window_index}}.#{{pane_index}}' 2>/dev/null",
        tmux_pane.replace('\'', r"'\''")
    );
    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .ok()?;
    let id = out.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Like `bind_session_id` but always overrides — the SessionStart hook
/// is authoritative, so any prior heuristic binding (MRU jsonl, etc.)
/// gets replaced. Also bumps `binding_confidence` to `Explicit` and
/// emits a `PaneUpdated` event.
async fn force_bind_session_id(state: &AppState, pane_id: &str, session_id: &str) {
    let updated_dto = {
        let mut panes = state.panes.write().await;
        if let Some(rec) = panes.get_mut(pane_id) {
            let changed = rec.dto.claude_session_id.as_deref() != Some(session_id)
                || rec.binding_confidence != BindingConfidence::Explicit;
            if changed {
                rec.dto.claude_session_id = Some(session_id.to_string());
                rec.binding_confidence = BindingConfidence::Explicit;
                rec.dto.updated_at = now_ms();
                Some(rec.dto.clone())
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(pane) = updated_dto {
        let _ = state.events.send(EventDto::PaneUpdated { pane });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn resolve_pane(state: &AppState, payload: &HookPayload) -> Option<String> {
    let hook_sid = payload.session_id.as_deref();

    // 1. Prefer session_id match — unique per Claude instance, most reliable.
    if let Some(sid) = hook_sid {
        let panes = state.panes.read().await;
        for rec in panes.values() {
            if rec.dto.claude_session_id.as_deref() == Some(sid) {
                tracing::info!(pane = %rec.dto.id, "resolve_pane: matched by session_id");
                return Some(rec.dto.id.clone());
            }
        }
        tracing::info!(sid = %sid, "resolve_pane: no session_id match, trying cwd");
    }

    // 2. Exact cwd match (normalized trailing slash).  When multiple panes
    //    share the same cwd, prefer the one whose session_id is still
    //    unbound — that's the one that hasn't been claimed by an earlier
    //    hook yet.  If all are bound (or all unbound), fall through to the
    //    first match like before.
    if let Some(cwd) = payload.cwd.as_ref() {
        let cwd_norm = cwd.trim_end_matches('/');
        let panes = state.panes.read().await;

        let mut first_exact: Option<String> = None;
        let mut unbound_exact: Option<String> = None;
        for rec in panes.values() {
            let pane_path = rec.dto.current_path.trim_end_matches('/');
            if pane_path == cwd_norm {
                if first_exact.is_none() {
                    first_exact = Some(rec.dto.id.clone());
                }
                if rec.dto.claude_session_id.is_none() && unbound_exact.is_none() {
                    unbound_exact = Some(rec.dto.id.clone());
                }
            }
        }
        if let Some(pane_id) = unbound_exact.or(first_exact) {
            tracing::info!(pane = %pane_id, cwd = %cwd_norm, "resolve_pane: matched by exact cwd");
            drop(panes);
            // Bind the hook's session_id to this pane so subsequent
            // hooks from the same Claude instance resolve unambiguously.
            if let Some(sid) = hook_sid {
                bind_session_id(state, &pane_id, sid).await;
            }
            return Some(pane_id);
        }

        // 3. Prefix match — Claude may cd into a subdir of the pane's cwd.
        let mut first_prefix: Option<String> = None;
        let mut unbound_prefix: Option<String> = None;
        for rec in panes.values() {
            let pane_path = rec.dto.current_path.trim_end_matches('/');
            if !pane_path.is_empty()
                && (cwd_norm.starts_with(pane_path) || pane_path.starts_with(cwd_norm))
            {
                if first_prefix.is_none() {
                    first_prefix = Some(rec.dto.id.clone());
                }
                if rec.dto.claude_session_id.is_none() && unbound_prefix.is_none() {
                    unbound_prefix = Some(rec.dto.id.clone());
                }
            }
        }
        if let Some(pane_id) = unbound_prefix.or(first_prefix) {
            tracing::info!(pane = %pane_id, cwd = %cwd_norm, "resolve_pane: matched by prefix cwd");
            drop(panes);
            if let Some(sid) = hook_sid {
                bind_session_id(state, &pane_id, sid).await;
            }
            return Some(pane_id);
        }

        // Log all known pane paths so we can see why nothing matched
        let known: Vec<String> = panes
            .values()
            .map(|r| format!("{}={}", r.dto.id, r.dto.current_path))
            .collect();
        tracing::info!(
            hook_cwd = %cwd_norm,
            panes = ?known,
            "resolve_pane: NO MATCH — hook cwd vs known pane paths"
        );
    } else {
        tracing::info!("resolve_pane: no cwd in payload, cannot resolve");
    }

    None
}

/// Bind a Claude session UUID to a pane so future hooks resolve via the
/// fast session_id path instead of the ambiguous cwd fallback.
async fn bind_session_id(state: &AppState, pane_id: &str, session_id: &str) {
    let mut panes = state.panes.write().await;
    if let Some(rec) = panes.get_mut(pane_id) {
        if rec.dto.claude_session_id.is_none() {
            rec.dto.claude_session_id = Some(session_id.to_string());
            tracing::debug!(pane = %pane_id, sid = %session_id, "bound session_id via hook");
        }
    }
}

/// Look up project display name and Claude account for a resolved pane.
async fn pane_context(state: &AppState, pane_id: &str) -> (Option<String>, Option<String>) {
    let panes = state.panes.read().await;
    panes
        .get(pane_id)
        .map(|r| (r.dto.project_display_name.clone(), r.dto.claude_account.clone()))
        .unwrap_or((None, None))
}

async fn publish_ntfy_approval(state: &AppState, dto: &ApprovalDto) {
    let host = &state.bind_addr;
    let bearer = state.bearer.read().await.clone();
    let approval_url = format!("http://{}/api/v1/approvals/{}", host, dto.id);

    let make_action = |label: &str, decision: &str| NtfyAction {
        action: "http".into(),
        label: label.into(),
        url: approval_url.clone(),
        method: Some("POST".into()),
        headers: Some(HashMap::from([
            ("Authorization".into(), format!("Bearer {}", bearer)),
            ("Content-Type".into(), "application/json".into()),
        ])),
        body: Some(format!(r#"{{"decision":"{}"}}"#, decision)),
    };

    let actions = vec![
        make_action("Allow", "allow"),
        make_action("Deny", "deny"),
    ];

    let msg = NtfyMessage {
        id: dto.id.to_string(),
        time: chrono::Utc::now().timestamp(),
        topic: state.ntfy_topic.to_string(),
        event: "message".to_string(),
        title: Some(match &dto.project_display_name {
            Some(name) => format!("[{}] Claude: {}", name, dto.title),
            None => format!("Claude: {}", dto.title),
        }),
        message: if dto.message.is_empty() {
            "Approval requested".to_string()
        } else {
            dto.message.clone()
        },
        priority: 4,
        tags: vec!["robot".to_string(), "question".to_string()],
        actions: Some(actions),
    };

    enqueue_ntfy(state, msg).await;
}

/// Push an ntfy message into the backlog ring buffer and broadcast
/// to any live SSE subscribers. Shared by approval + attention paths.
async fn enqueue_ntfy(state: &AppState, msg: NtfyMessage) {
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

/// Publish a lightweight ntfy notification for attention events (idle,
/// stop, elicitation). No action buttons — just a bell push so the
/// phone lights up even when the companion app is killed.
async fn publish_ntfy_attention(state: &AppState, title: &str, message: &str) {
    let msg = NtfyMessage {
        id: Uuid::new_v4().to_string(),
        time: chrono::Utc::now().timestamp(),
        topic: state.ntfy_topic.to_string(),
        event: "message".to_string(),
        title: Some(title.to_string()),
        message: message.to_string(),
        priority: 3,
        tags: vec!["robot".to_string(), "bell".to_string()],
        actions: None,
    };
    enqueue_ntfy(state, msg).await;
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
