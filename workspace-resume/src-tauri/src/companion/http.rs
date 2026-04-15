//! axum Router assembly and HTTP handlers for the companion API.

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use base64::Engine;
use serde::Deserialize;

use super::{
    error::AppError,
    hook_sink,
    models::{
        now_ms, AccountRateLimit, ApprovalDto, ApprovalResponse, CaptureResponse,
        ConversationMessage, ConversationResponse, CreatePaneRequest, CreatePaneResponse,
        CreateWindowRequest, CreateWindowResponse, HealthDto, ImageRequest, InputRequest,
        KeyRequest, PaneDto, ProjectDto, SessionDto, VoiceRequest,
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
        .route("/panes", get(list_panes).post(create_pane))
        .route("/panes/{id}/capture", get(capture_pane))
        .route("/panes/{id}/conversation", get(pane_conversation))
        .route("/panes/{id}/input", post(send_input))
        .route("/panes/{id}/voice", post(send_voice))
        .route("/panes/{id}/image", post(send_image))
        .route("/panes/{id}/key", post(send_key))
        .route("/panes/{id}/cancel", post(cancel_pane))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", post(resolve_approval))
        .route("/usage", get(usage_summary))
        .route("/usage/projects/{encoded_name}", get(project_usage))
        .route("/projects", get(list_projects))
        .route("/windows", post(create_window))
        .route("/windows/{session}/{index}", delete(kill_window))
        .route("/panes/{id}", delete(kill_pane))
        .route("/rate-limits", get(rate_limits))
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
        .route("/hooks/session-start", post(hook_sink::session_start))
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
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024)) // 32 MB for multi-image uploads
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
    State(_state): State<AppState>,
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
    // Capture is a READ — do not bump `updated_at` on the pane record.
    // That field is reserved for real state changes (transitions, hooks)
    // so clients like the Stashed filter can rely on it not being polluted
    // by phone views. `seq` is still derived from now() for cache busting.
    let seq = now_ms() as u64;
    Ok(Json(CaptureResponse { lines, seq }))
}

// ---------------------------------------------------------------------------
// Conversation (JSONL session transcript)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ConversationQuery {
    /// Return only messages after this UUID (for incremental fetches).
    #[serde(default)]
    pub after: Option<String>,
}

async fn pane_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ConversationQuery>,
) -> Result<Json<ConversationResponse>, AppError> {
    // Look up the pane to get session_id and project path
    let panes = state.panes.read().await;
    let rec = panes.get(&id).ok_or(AppError::NotFound)?;
    let bound_session = rec.dto.claude_session_id.clone();
    let encoded_project = rec.dto.project_encoded_name.clone();
    drop(panes);

    // Resolve which JSONL to read:
    // 1. Pane has an explicit session_id (from --resume <uuid> in start cmd or hooks)
    // 2. Pane has a project — pick the most recently modified .jsonl in that dir
    //    (handles `ncld` launches without --resume; the active session is whatever
    //    Claude last wrote to)
    // 3. Otherwise, no way to find a transcript
    let (session_id, jsonl_path) = match (bound_session, encoded_project) {
        (Some(sid), Some(proj)) => (
            sid.clone(),
            format!("$HOME/.claude/projects/{}/{}.jsonl", proj, sid),
        ),
        (Some(sid), None) => {
            return Err(AppError::BadRequest(format!(
                "pane has session {} but no project binding",
                sid
            )));
        }
        (None, Some(proj)) => {
            // Find most recent JSONL in the project directory
            let script = format!(
                "ls -t \"$HOME/.claude/projects/{}\"/*.jsonl 2>/dev/null | head -1",
                proj
            );
            let recent = crate::commands::tmux::run_tmux_command_async(script)
                .await
                .map_err(AppError::Tmux)?;
            let path = recent.lines().next().unwrap_or("").trim().to_string();
            if path.is_empty() {
                return Err(AppError::NotFound);
            }
            // Extract UUID from filename for the response
            let sid = std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            (sid, path)
        }
        (None, None) => {
            return Err(AppError::BadRequest(
                "pane has no Claude session or project binding".into(),
            ));
        }
    };

    let script = format!(
        "cat \"{}\" 2>/dev/null || echo '__FILE_NOT_FOUND__'",
        jsonl_path
    );
    let raw = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;

    if raw.trim() == "__FILE_NOT_FOUND__" || raw.is_empty() {
        return Err(AppError::NotFound);
    }

    let messages = parse_jsonl_conversation(&raw, q.after.as_deref());

    Ok(Json(ConversationResponse {
        session_id,
        messages,
    }))
}

/// Parse Claude Code JSONL into conversation messages.
///
/// Extracts `type: "user"` and `type: "assistant"` records, skipping
/// system/attachment/permission-mode/file-history-snapshot records.
/// For user records whose `message.content` starts with `[{` (tool
/// results), the record is skipped — tool results are internal plumbing.
/// For assistant records, only `text` content blocks are included;
/// `thinking` and `tool_use` blocks are excluded.
fn parse_jsonl_conversation(raw: &str, after: Option<&str>) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();
    let mut past_cursor = after.is_none();

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let record_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if record_type != "user" && record_type != "assistant" {
            continue;
        }

        let uuid = val
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let timestamp = val
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Skip until we pass the cursor
        if !past_cursor {
            if uuid == after.unwrap_or("") {
                past_cursor = true;
            }
            continue;
        }

        // Skip tool-result user messages (these are internal plumbing)
        if record_type == "user" && val.get("toolUseResult").is_some() {
            continue;
        }

        let message = match val.get("message") {
            Some(m) => m,
            None => continue,
        };

        if record_type == "user" {
            // User message: message.content is a string
            let text = message
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                continue;
            }
            messages.push(ConversationMessage {
                uuid,
                role: "user".into(),
                text,
                timestamp,
                tool_name: None,
                tool_input: None,
            });
        } else {
            // Assistant message: message.content is an array of blocks
            let content = match message.get("content").and_then(|c| c.as_array()) {
                Some(arr) => arr,
                None => continue,
            };

            let mut text_parts = Vec::new();
            let mut tool_name = None;
            let mut tool_input: Option<serde_json::Value> = None;

            for block in content {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(t);
                        }
                    }
                    "tool_use" => {
                        if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                            tool_name = Some(name.to_string());
                        }
                        if let Some(input) = block.get("input") {
                            tool_input = Some(input.clone());
                        }
                    }
                    _ => {} // skip thinking, etc.
                }
            }

            let text = text_parts.join("\n");
            if text.is_empty() && tool_name.is_none() {
                continue;
            }
            messages.push(ConversationMessage {
                uuid,
                role: "assistant".into(),
                text,
                timestamp,
                tool_name,
                tool_input,
            });
        }
    }

    messages
}

// ---------------------------------------------------------------------------
// Pane I/O
// ---------------------------------------------------------------------------

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

async fn send_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ImageRequest>,
) -> Result<StatusCode, AppError> {
    if req.images.is_empty() {
        return Err(AppError::BadRequest("no images provided".into()));
    }

    // Decode + write each image. Timestamp + index produces unique filenames
    // so multiple attachments in one request don't collide.
    let base_ts = chrono::Utc::now().timestamp_millis();
    let mut wsl_paths: Vec<String> = Vec::with_capacity(req.images.len());
    for (idx, item) in req.images.iter().enumerate() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&item.image_base64)
            .map_err(|e| AppError::BadRequest(format!("invalid base64 (image {}): {}", idx, e)))?;
        let ext = match item.media_type.as_str() {
            "image/jpeg" | "image/jpg" => "jpg",
            "image/webp" => "webp",
            "image/gif" => "gif",
            _ => "png",
        };
        let wsl_path = format!("/tmp/pane-mgmt/img_{}_{}.{}", base_ts, idx, ext);
        crate::commands::tmux::write_file_to_wsl_async(wsl_path.clone(), bytes)
            .await
            .map_err(AppError::Tmux)?;
        wsl_paths.push(wsl_path);
    }

    // Build one message that references every image path inline so Claude
    // reads them all with the Read tool on the same turn.
    let paths_joined = wsl_paths.join(", ");
    let prompt = req.prompt.as_deref().unwrap_or("").trim();
    let label = if wsl_paths.len() == 1 { "screenshot" } else { "screenshots" };
    let message = if prompt.is_empty() {
        format!("Look at this {}: {}", label, paths_joined)
    } else {
        format!("{} ({}: {})", prompt, label, paths_joined)
    };

    tracing::info!(pane = %id, count = wsl_paths.len(), "images uploaded");
    send_text_to_pane(&id, &message, true).await?;
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
        "C-u", "C-a", "C-k", "C-c",
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

async fn kill_pane(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    // id is "session:window.pane", e.g. "main:3.1"
    if id.is_empty() {
        return Err(AppError::BadRequest("pane id is required".into()));
    }
    let id_esc = id.replace('\'', r"'\''");
    let script = format!("tmux kill-pane -t '{}' 2>&1", id_esc);
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;
    tracing::info!(%id, "pane killed");
    Ok(StatusCode::NO_CONTENT)
}

async fn create_pane(
    State(_state): State<AppState>,
    Json(req): Json<CreatePaneRequest>,
) -> Result<Json<CreatePaneResponse>, AppError> {
    // Split the current window vertically (new pane below the target),
    // inheriting the target pane's working directory, then launch the
    // account-specific Claude launcher inside it.
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
    if req.target_pane_id.is_empty() {
        return Err(AppError::BadRequest("target_pane_id is required".into()));
    }
    let target_esc = req.target_pane_id.replace('\'', r"'\''");

    // Query the target pane's cwd first — #{pane_current_path} in split-window's
    // -c arg expands relative to the *active* pane, not the -t target.
    let script = format!(
        "CWD=$(tmux display-message -p -t '{target}' '#{{pane_current_path}}'); \
         NEW=$(tmux split-window -v -t '{target}' -c \"$CWD\" -d \
         -P -F '#{{session_name}}:#{{window_index}}.#{{pane_index}}' 2>&1); \
         if [ -z \"$NEW\" ]; then echo 'ERR: split-window failed'; exit 1; fi; \
         tmux send-keys -t \"$NEW\" '{cmd}' Enter; \
         echo \"NEW=$NEW\"",
        target = target_esc,
        cmd = cmd,
    );

    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(AppError::Tmux)?;

    let new_pane_id = out
        .lines()
        .find_map(|l| l.strip_prefix("NEW="))
        .ok_or_else(|| AppError::BadRequest(format!("create_pane parse failed: {}", out)))?
        .trim()
        .to_string();

    tracing::info!(%new_pane_id, target = %req.target_pane_id, account = %req.account, "pane split created");

    Ok(Json(CreatePaneResponse {
        pane_id: new_pane_id,
    }))
}

async fn rate_limits(
    State(state): State<AppState>,
) -> Json<Vec<AccountRateLimit>> {
    Json(state.rate_limits.read().await.clone())
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
