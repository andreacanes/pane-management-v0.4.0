//! axum Router assembly and HTTP handlers for the companion API.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;

use super::{
    error::AppError,
    hook_sink,
    models::{
        now_ms, ApprovalDto, ApprovalResponse, CaptureResponse, CreateWindowRequest,
        CreateWindowResponse, HealthDto, InputRequest, KeyRequest, PaneDto, ProjectDto,
        SessionDto, VoiceRequest,
    },
    ntfy_server,
    state::AppState,
    tmux_poller::normalize_path,
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
        .route("/panes/{id}/key", post(send_key))
        .route("/panes/{id}/cancel", post(cancel_pane))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", post(resolve_approval))
        .route("/usage", get(usage_summary))
        .route("/usage/projects/{encoded_name}", get(project_usage))
        .route("/projects", get(list_projects))
        .route("/windows", post(create_window))
        .route("/windows/{session}/{index}", delete(kill_window))
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
    let out = crate::commands::tmux::run_tmux_command_async(script.to_string())
        .await
        .map_err(AppError::Tmux)?;
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
    1000
}

async fn capture_pane(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CaptureQuery>,
) -> Result<Json<CaptureResponse>, AppError> {
    // pane id format: "<session>:<window>.<pane>" — the same as tmux target syntax
    let start = -(q.lines as i64);
    let script = format!("tmux capture-pane -p -e -t {} -S {} 2>&1 || true", id, start);
    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;
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
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<InputRequest>,
) -> Result<StatusCode, AppError> {
    send_text_to_pane(&id, &req.text, req.submit).await?;
    // User has answered a Claude prompt (or typed anything). Drop the
    // attention flag so the pane can leave Waiting on the next poll tick.
    state.attention_panes.write().await.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}

async fn send_voice(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<VoiceRequest>,
) -> Result<StatusCode, AppError> {
    tracing::info!(pane = %id, locale = ?req.locale, "voice input");
    send_text_to_pane(&id, &req.transcript, req.submit).await?;
    state.attention_panes.write().await.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}

async fn send_key(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<KeyRequest>,
) -> Result<StatusCode, AppError> {
    // Whitelist the key names we accept so an attacker who somehow has
    // the bearer token can't use this endpoint to inject arbitrary shell
    // strings via tmux send-keys. Cycling claude modes only needs S-Tab
    // for now; widen the list when a real client needs more.
    const ALLOWED_KEYS: &[&str] = &[
        "S-Tab", "BTab", "Tab", "Enter", "Escape", "Up", "Down", "Left", "Right",
    ];
    if !ALLOWED_KEYS.contains(&req.key.as_str()) {
        return Err(AppError::BadRequest(format!(
            "key '{}' not in allow-list",
            req.key
        )));
    }
    let script = format!("tmux send-keys -t {} {}", id, req.key);
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_pane(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let script = format!("tmux send-keys -t {} C-c C-c 2>&1", id);
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;
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
// Window lifecycle + project picker (for mobile new-window sheet)
// ---------------------------------------------------------------------------

async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<ProjectDto>>, AppError> {
    let projects = crate::commands::discovery::list_projects()
        .await
        .map_err(AppError::BadRequest)?;

    // Build a snapshot of pane cwds so we can attach an
    // active_pane_count to each project without holding the read lock
    // across the whole loop.
    let cwds: Vec<String> = {
        let panes = state.panes.read().await;
        panes
            .values()
            .map(|r| normalize_path(&r.dto.current_path))
            .collect()
    };

    let mut dtos: Vec<ProjectDto> = projects
        .into_iter()
        .filter(|p| p.path_exists && !p.actual_path.starts_with("[unresolved]"))
        .map(|p| {
            let norm = normalize_path(&p.actual_path);
            let display_name = norm
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or(&norm)
                .to_string();
            let active_pane_count = cwds
                .iter()
                .filter(|c| c == &&norm || c.starts_with(&format!("{}/", norm)))
                .count() as u32;
            ProjectDto {
                encoded_name: p.encoded_name,
                display_name,
                actual_path: p.actual_path,
                git_branch: p.git_branch,
                session_count: p.session_count as u32,
                active_pane_count,
                tier: None, // tier lives in project_meta store on the desktop — deferred
            }
        })
        .collect();

    // Sort: panes-open first, then alphabetical by display_name.
    dtos.sort_by(|a, b| {
        b.active_pane_count
            .cmp(&a.active_pane_count)
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
    });

    Ok(Json(dtos))
}

async fn create_window(
    State(_state): State<AppState>,
    Json(req): Json<CreateWindowRequest>,
) -> Result<Json<CreateWindowResponse>, AppError> {
    // Andrea = ncld, Bravura = ncld2. Both shell functions live in
    // ~/.bashrc so the command runs inside an interactive shell via
    // `send-keys`, not as a `new-window "cmd"` argument.
    let cmd = match req.account.as_str() {
        "bravura" => "ncld2",
        "andrea" | "" => "ncld",
        other => {
            return Err(AppError::BadRequest(format!(
                "unknown account '{}': expected andrea|bravura",
                other
            )))
        }
    };
    if req.session_name.is_empty() || req.project_path.is_empty() {
        return Err(AppError::BadRequest(
            "session_name and project_path are required".into(),
        ));
    }

    // Single-quote shell escape: a single quote becomes '\''
    let sess_esc = req.session_name.replace('\'', r"'\''");
    let path_esc = req.project_path.replace('\'', r"'\''");
    let name_esc = req.project_display_name.replace('\'', r"'\''");

    let script = format!(
        "IDX=$(tmux new-window -t '{sess}' -d -c '{path}' -n '{name}' -P -F '#{{window_index}}' 2>&1); \
         if [ -z \"$IDX\" ]; then echo 'ERR: new-window failed'; exit 1; fi; \
         tmux send-keys -t '{sess}':\"$IDX\" '{cmd}' Enter; \
         echo \"IDX=$IDX\"",
        sess = sess_esc,
        path = path_esc,
        name = name_esc,
        cmd = cmd,
    );

    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;

    // Parse `IDX=<n>` out of the stdout.
    let idx_str = out
        .lines()
        .find_map(|l| l.strip_prefix("IDX="))
        .ok_or_else(|| AppError::BadRequest(format!("create_window parse failed: {}", out)))?;
    let window_index: u32 = idx_str
        .trim()
        .parse()
        .map_err(|_| AppError::BadRequest(format!("create_window bad index: {}", idx_str)))?;
    let pane_id = format!("{}:{}.1", req.session_name, window_index);

    tracing::info!(%pane_id, account = %req.account, "new window created");

    Ok(Json(CreateWindowResponse {
        window_index,
        pane_id,
    }))
}

async fn kill_window(
    State(_state): State<AppState>,
    Path((session, index)): Path<(String, u32)>,
) -> Result<StatusCode, AppError> {
    if session.is_empty() {
        return Err(AppError::BadRequest("session is required".into()));
    }
    let sess_esc = session.replace('\'', r"'\''");
    let script = format!("tmux kill-window -t '{}':{} 2>&1", sess_esc, index);
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;
    tracing::info!(%session, index, "window killed");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Send literal text to a tmux pane via `send-keys -l`, optionally
/// followed by Enter. Keeps quoting sane by passing via stdin.
async fn send_text_to_pane(id: &str, text: &str, submit: bool) -> Result<(), AppError> {
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
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;
    Ok(())
}
