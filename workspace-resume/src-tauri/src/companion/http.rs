//! axum Router assembly and HTTP handlers for the companion API.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;

use super::{
    error::AppError,
    hook_sink,
    models::{
        now_ms, ApprovalDto, ApprovalResponse, CaptureResponse, HealthDto, InputRequest, PaneDto,
        SessionDto, VoiceRequest,
    },
    ntfy_server,
    state::AppState,
    ws,
};

pub fn router(state: AppState) -> Router {
    // Protected API — bearer auth
    let api_auth = Router::new()
        .route("/sessions", get(list_sessions))
        .route("/panes", get(list_panes))
        .route("/panes/{id}/capture", get(capture_pane))
        .route("/panes/{id}/input", post(send_input))
        .route("/panes/{id}/voice", post(send_voice))
        .route("/panes/{id}/cancel", post(cancel_pane))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", post(resolve_approval))
        .route("/usage", get(usage_summary))
        .route("/usage/projects/{encoded_name}", get(project_usage))
        .route("/events", get(ws::upgrade))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::auth::bearer_mw,
        ));

    // Public API — no auth
    let api_public = Router::new().route("/health", get(health));

    // Hook sink — shared-secret auth
    let api_hooks = Router::new()
        .route("/hooks/notification", post(hook_sink::notification))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::auth::hook_secret_mw,
        ));

    // ntfy-compatible endpoints — no auth; topic in URL path is the only
    // secret (random at first start). Android ntfy app subscribes here.
    let ntfy = Router::new()
        .route("/{topic}", post(ntfy_server::publish))
        .route("/{topic}/json", get(ntfy_server::subscribe_sse));

    Router::new()
        .nest("/api/v1", api_auth.merge(api_public).merge(api_hooks))
        .merge(ntfy)
        .with_state(state)
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(state): State<AppState>) -> Json<HealthDto> {
    Json(HealthDto {
        version: env!("CARGO_PKG_VERSION"),
        bind: state.bind_addr.clone(),
        uptime_s: state.started_at.elapsed().as_secs(),
    })
}

async fn list_sessions(State(_state): State<AppState>) -> Result<Json<Vec<SessionDto>>, AppError> {
    let script = "tmux list-sessions -F '#{session_name}|#{session_windows}|#{?session_attached,1,0}' 2>/dev/null || true";
    let out = crate::commands::tmux::run_tmux_command(script).map_err(AppError::Tmux)?;
    let sessions: Vec<SessionDto> = out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 3 {
                return None;
            }
            Some(SessionDto {
                name: parts[0].to_string(),
                windows: parts[1].parse().unwrap_or(0),
                attached: parts[2] == "1",
            })
        })
        .collect();
    Ok(Json(sessions))
}

#[derive(Debug, Deserialize)]
pub struct ListPanesQuery {
    #[serde(default)]
    pub session: Option<String>,
}

async fn list_panes(
    State(state): State<AppState>,
    Query(q): Query<ListPanesQuery>,
) -> Json<Vec<PaneDto>> {
    let panes = state.panes.read().await;
    let filtered: Vec<PaneDto> = panes
        .values()
        .filter(|r| {
            q.session
                .as_ref()
                .map(|s| &r.dto.session_name == s)
                .unwrap_or(true)
        })
        .map(|r| r.dto.clone())
        .collect();
    Json(filtered)
}

#[derive(Debug, Deserialize)]
pub struct CaptureQuery {
    #[serde(default = "default_capture_lines")]
    pub lines: u32,
}
fn default_capture_lines() -> u32 {
    200
}

async fn capture_pane(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CaptureQuery>,
) -> Result<Json<CaptureResponse>, AppError> {
    // pane id format: "<session>:<window>.<pane>" — the same as tmux target syntax
    let start = -(q.lines as i64);
    let script = format!("tmux capture-pane -p -t {} -S {} 2>&1 || true", id, start);
    let out = crate::commands::tmux::run_tmux_command(&script).map_err(AppError::Tmux)?;
    let lines: Vec<String> = out.lines().map(|s| s.to_string()).collect();
    // Update the pane record's seq so clients can detect changes
    let seq = {
        let mut panes = state.panes.write().await;
        if let Some(rec) = panes.get_mut(&id) {
            rec.dto.updated_at = now_ms();
        }
        now_ms() as u64
    };
    Ok(Json(CaptureResponse { lines, seq }))
}

async fn send_input(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<InputRequest>,
) -> Result<StatusCode, AppError> {
    send_text_to_pane(&id, &req.text, req.submit)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_voice(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<VoiceRequest>,
) -> Result<StatusCode, AppError> {
    tracing::info!(pane = %id, locale = ?req.locale, "voice input");
    send_text_to_pane(&id, &req.transcript, req.submit)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_pane(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let script = format!("tmux send-keys -t {} C-c C-c 2>&1", id);
    crate::commands::tmux::run_tmux_command(&script).map_err(AppError::Tmux)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_approvals(State(state): State<AppState>) -> Json<Vec<ApprovalDto>> {
    let approvals = state.approvals.read().await;
    let out: Vec<ApprovalDto> = approvals.values().map(|p| p.dto.clone()).collect();
    Json(out)
}

async fn resolve_approval(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(resp): Json<ApprovalResponse>,
) -> Result<StatusCode, AppError> {
    let mut guard = state.approvals.write().await;
    let pending = guard.remove(&id).ok_or(AppError::NotFound)?;
    let _ = pending.responder.send(resp);
    Ok(StatusCode::NO_CONTENT)
}

async fn usage_summary(State(_s): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let all = crate::commands::usage::get_all_usage()
        .await
        .map_err(|e| AppError::BadRequest(e))?;
    let mut projects = 0u32;
    let mut sessions = 0u32;
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache_write = 0u64;
    let mut cache_read = 0u64;
    let mut cost = 0.0f64;
    for (_, p) in all.iter() {
        projects += 1;
        sessions += p.sessions.len() as u32;
        input += p.total_input;
        output += p.total_output;
        cache_write += p.total_cache_write;
        cache_read += p.total_cache_read;
        cost += p.total_cost_usd;
    }
    Ok(Json(serde_json::json!({
        "projects": projects,
        "sessions": sessions,
        "input_tokens": input,
        "output_tokens": output,
        "cache_write_tokens": cache_write,
        "cache_read_tokens": cache_read,
        "total_cost_usd": cost,
    })))
}

async fn project_usage(
    State(_s): State<AppState>,
    Path(encoded): Path<String>,
) -> Result<Json<crate::services::usage::ProjectUsage>, AppError> {
    let p = crate::commands::usage::get_project_usage(encoded)
        .await
        .map_err(|e| AppError::BadRequest(e))?;
    Ok(Json(p))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Send literal text to a tmux pane via `send-keys -l`, optionally
/// followed by Enter. Keeps quoting sane by passing via stdin.
fn send_text_to_pane(id: &str, text: &str, submit: bool) -> Result<(), AppError> {
    // Escape single quotes for bash single-quoted string
    let escaped = text.replace('\'', r"'\''");
    let submit_line = if submit {
        format!("tmux send-keys -t {} Enter", id)
    } else {
        String::new()
    };
    let script = format!(
        "tmux send-keys -t {} -l '{}'; {}",
        id, escaped, submit_line
    );
    crate::commands::tmux::run_tmux_command(&script).map_err(AppError::Tmux)?;
    Ok(())
}
