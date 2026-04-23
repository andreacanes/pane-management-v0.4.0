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
    audit_log::AuditEvent,
    error::AppError,
    hook_sink,
    models::{
        now_ms, AccountRateLimit, ApprovalDto, ApprovalResponse, CaptureResponse,
        ConversationMessage, ConversationResponse, CreatePaneRequest, CreatePaneResponse,
        CreateWindowRequest, CreateWindowResponse, ForkPaneRequest, HealthDto, ImageRequest,
        InputRequest, KeyRequest, PaneDto, ProjectDto, SessionDto, VoiceRequest,
    },
    ntfy_server, pane_log,
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
        .route("/panes/{id}/fork", post(fork_pane))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", post(resolve_approval))
        .route("/usage", get(usage_summary))
        .route("/usage/projects/{encoded_name}", get(project_usage))
        .route("/projects", get(list_projects))
        .route("/windows", post(create_window))
        .route("/windows/{session}/{index}", delete(kill_window))
        .route("/panes/{id}", delete(kill_pane))
        .route("/rate-limits", get(rate_limits))
        .route("/audit", get(audit_log))
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

/// Map a tmux command failure into an AppError, logging the underlying
/// stderr first so an HTTP 502 in the client log can be correlated with
/// the actual tmux complaint on the desktop side.
fn tmux_err(e: String) -> AppError {
    tracing::warn!(error = %e, "tmux command failed");
    AppError::Tmux(e)
}

async fn health(State(state): State<AppState>) -> Json<HealthDto> {
    Json(HealthDto {
        version: env!("CARGO_PKG_VERSION"),
        bind: state.bind_addr.clone(),
        uptime_s: state.started_at.elapsed().as_secs(),
    })
}

async fn list_sessions(State(state): State<AppState>) -> Result<Json<Vec<SessionDto>>, AppError> {
    use crate::services::host_target::HostTarget;

    let script = "tmux list-sessions -F '#{session_name}|#{session_windows}|#{?session_attached,1,0}' 2>/dev/null || true";

    // Local first (always).
    let mut sessions = parse_session_list(
        &crate::commands::tmux::run_tmux_command_async_on(HostTarget::Local, script.to_string())
            .await
            .map_err(tmux_err)?,
        None,
    );

    // Then each distinct remote host referenced by a pane assignment. SSH
    // failures degrade gracefully — a reachable Mac contributes its
    // sessions; an unreachable one logs and drops out without failing
    // the whole response.
    let remote_hosts = collect_remote_hosts(&state);
    for alias in remote_hosts {
        match crate::commands::tmux::run_tmux_command_async_on(
            HostTarget::Remote { alias: alias.clone() },
            script.to_string(),
        )
        .await
        {
            Ok(out) => {
                sessions.extend(parse_session_list(&out, Some(&alias)));
            }
            Err(e) => {
                tracing::debug!(alias = %alias, "remote list-sessions failed: {e}");
            }
        }
    }

    Ok(Json(sessions))
}

/// Parse `tmux list-sessions -F '...'` output into `SessionDto`s. When
/// `alias` is set, each session name is prefixed `<alias>/` so remote
/// sessions don't collide with local ones sharing the same name (both
/// hosts commonly have `main`, for example).
fn parse_session_list(out: &str, alias: Option<&str>) -> Vec<SessionDto> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 3 {
                return None;
            }
            let name = match alias {
                Some(a) => format!("{}/{}", a, parts[0]),
                None => parts[0].to_string(),
            };
            Some(SessionDto {
                name,
                windows: parts[1].parse().unwrap_or(0),
                attached: parts[2] == "1",
            })
        })
        .collect()
}

/// Enumerate distinct non-local SSH aliases currently referenced by any
/// pane assignment. Matches the poller's `discover_remote_hosts` — kept
/// inline here rather than imported to avoid a cross-module cycle
/// (poller is private within the companion).
fn collect_remote_hosts(state: &AppState) -> Vec<String> {
    let full = match crate::commands::project_meta::get_pane_assignments_full_sync(&state.app) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("pane_assignments read failed for list_sessions: {e}");
            return Vec::new();
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for a in full.values() {
        if !a.host.is_empty() && a.host != "local" && seen.insert(a.host.clone()) {
            out.push(a.host.clone());
        }
    }
    out
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
    // Look up the pane's Claude session id + tmux stable uid so we can find
    // the right per-session log file. Logs are keyed by session uuid (stable
    // across tmux renumbers, `/clear`, and companion restarts) or by the
    // tmux pane uid when no session has been detected yet (pending).
    let (session_id, pane_uid, pane_width) = {
        let panes = state.panes.read().await;
        match panes.get(&id) {
            Some(rec) => (
                rec.dto.claude_session_id.clone(),
                rec.pane_uid.clone(),
                rec.pane_width as usize,
            ),
            None => (None, String::new(), 0),
        }
    };

    let log_path = match (session_id.as_ref(), pane_uid.is_empty()) {
        (Some(sid), _) => Some(pane_log::log_path_for_session(sid)),
        (None, false) => Some(pane_log::log_path_for_pending(&pane_uid)),
        (None, true) => None,
    };

    let tail_bytes = if q.lines == 0 {
        pane_log::DEFAULT_TAIL_BYTES
    } else {
        // When the client asks for a specific line count, size the tail budget
        // accordingly: 200 bytes/line is a loose over-estimate that covers
        // long ANSI-decorated lines without pulling the whole log.
        (q.lines as u64).saturating_mul(200)
    };

    // Read the tail and replay it through a VT emulator on the blocking
    // pool. Both the file I/O and the VT replay are CPU/IO-bound and would
    // block the axum worker if run directly on the async runtime.
    if let Some(log_path) = log_path {
        match tokio::task::spawn_blocking(move || {
            pane_log::read_tail_bytes(&log_path, tail_bytes)
                .map(|bytes| pane_log::replay_to_lines(&bytes, pane_width))
        })
        .await
        {
            Ok(Ok(lines)) if !lines.is_empty() => {
                let seq = now_ms() as u64;
                return Ok(Json(CaptureResponse { lines, seq }));
            }
            Ok(Err(e)) => {
                tracing::debug!(pane = %id, "pane-log read failed, falling back to capture-pane: {e}");
            }
            Ok(Ok(_)) => {
                // Empty log file — brand-new pane whose pipe-pane hasn't
                // produced output yet, or non-Claude pane. Fall through.
            }
            Err(e) => {
                tracing::debug!(pane = %id, "pane-log task join failed: {e}");
            }
        }
    }

    // Fallback: tmux capture-pane. pane id format: "<session>:<window>.<pane>"
    // — the same as tmux target syntax. lines=0 means "all scrollback"
    // (tmux `-S -`); any positive value captures the last N lines.
    let start_flag = if q.lines == 0 {
        "-".to_string()
    } else {
        format!("-{}", q.lines)
    };
    let script = format!("tmux capture-pane -p -e -t {} -S {} 2>&1 || true", id, start_flag);
    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(tmux_err)?;
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
    // Look up the pane to get session_id, project path, and account.
    let panes = state.panes.read().await;
    let rec = panes.get(&id).ok_or(AppError::NotFound)?;
    let bound_session = rec.dto.claude_session_id.clone();
    let encoded_project = rec.dto.project_encoded_name.clone();
    let account_key = rec.dto.claude_account.clone();
    drop(panes);

    // Each Claude account keeps its transcripts under its own config
    // directory (`.claude` for Andrea, `.claude-b` for Bravura). Resolve
    // against the `accounts` registry instead of hardcoding.
    let config_dir = account_key
        .as_deref()
        .and_then(|k| super::accounts::ACCOUNTS.iter().find(|a| a.key == k))
        .map(|a| a.config_dir)
        .unwrap_or(".claude");

    // Resolve which JSONL to read:
    // 1. Pane has an explicit session_id (from --resume <uuid> in start cmd or hooks)
    // 2. Pane has a project but no bound session — pick the most recently
    //    modified .jsonl in that dir that is NOT already claimed by a
    //    sibling pane. Without that filter two panes sharing a project
    //    (e.g. `ncld` + split) would briefly swap transcripts while the
    //    poller's /proc walk catches up.
    // 3. Otherwise, no way to find a transcript
    let (session_id, jsonl_path) = match (bound_session, encoded_project) {
        (Some(sid), Some(proj)) => (
            sid.clone(),
            format!("$HOME/{}/projects/{}/{}.jsonl", config_dir, proj, sid),
        ),
        (Some(sid), None) => {
            return Err(AppError::BadRequest(format!(
                "pane has session {} but no project binding",
                sid
            )));
        }
        (None, Some(proj)) => {
            // Collect session ids already claimed by other panes so we
            // don't hand the same JSONL to two detail screens.
            let sibling_sids: std::collections::HashSet<String> = {
                let panes = state.panes.read().await;
                panes
                    .iter()
                    .filter(|(pid, _)| *pid != &id)
                    .filter_map(|(_, r)| r.dto.claude_session_id.clone())
                    .collect()
            };
            let script = format!(
                "ls -t \"$HOME/{}/projects/{}\"/*.jsonl 2>/dev/null",
                config_dir, proj
            );
            let listing = crate::commands::tmux::run_tmux_command_async(script)
                .await
                .map_err(tmux_err)?;
            let pick = listing.lines().map(|l| l.trim()).find(|path| {
                if path.is_empty() {
                    return false;
                }
                let sid = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                !sibling_sids.contains(sid)
            });
            let path = match pick {
                Some(p) => p.to_string(),
                None => return Err(AppError::NotFound),
            };
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
        .map_err(tmux_err)?;

    if raw.trim() == "__FILE_NOT_FOUND__" || raw.is_empty() {
        return Err(AppError::NotFound);
    }

    let messages = parse_jsonl_conversation(&raw, q.after.as_deref());

    Ok(Json(ConversationResponse {
        session_id,
        messages,
    }))
}

const MAX_TOOL_RESULT_CHARS: usize = 2000;

/// Parse Claude Code JSONL into conversation messages.
///
/// Extracts `type: "user"` and `type: "assistant"` records, skipping
/// system/attachment/permission-mode/file-history-snapshot records.
/// Tool-result user records are emitted with `role: "tool_result"`.
/// Assistant records include text, tool_use, and thinking blocks.
fn parse_jsonl_conversation(raw: &str, after: Option<&str>) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();
    let mut past_cursor = after.is_none();
    let mut tool_use_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

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

        if !past_cursor {
            if uuid == after.unwrap_or("") {
                past_cursor = true;
            }
            // Even before the cursor, track tool_use ids so results
            // that fall after the cursor can still pair to a tool name.
            if record_type == "assistant" {
                if let Some(content) = val
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                            if let (Some(id), Some(name)) = (
                                block.get("id").and_then(|v| v.as_str()),
                                block.get("name").and_then(|v| v.as_str()),
                            ) {
                                tool_use_names.insert(id.to_string(), name.to_string());
                            }
                        }
                    }
                }
            }
            continue;
        }

        let message = match val.get("message") {
            Some(m) => m,
            None => continue,
        };

        if record_type == "user" {
            if val.get("toolUseResult").is_some() {
                // Tool result record — extract the output content.
                let content = message.get("content").and_then(|c| c.as_array());
                if let Some(blocks) = content {
                    for block in blocks {
                        if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                            continue;
                        }
                        let is_error = block
                            .get("is_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let raw_content = extract_tool_result_content(block);
                        if raw_content.is_empty() {
                            continue;
                        }
                        let truncated = raw_content.len() > MAX_TOOL_RESULT_CHARS;
                        let content = if truncated {
                            let mut end = MAX_TOOL_RESULT_CHARS;
                            while !raw_content.is_char_boundary(end) && end > 0 {
                                end -= 1;
                            }
                            raw_content[..end].to_string()
                        } else {
                            raw_content
                        };
                        messages.push(ConversationMessage {
                            uuid: uuid.clone(),
                            role: "tool_result".into(),
                            text: String::new(),
                            timestamp: timestamp.clone(),
                            tool_name: None,
                            tool_input: None,
                            thinking: None,
                            tool_result: Some(content),
                            tool_result_truncated: if truncated { Some(true) } else { None },
                            tool_result_error: if is_error { Some(true) } else { None },
                        });
                    }
                }
            } else {
                // Regular user message: message.content is a string
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
                    thinking: None,
                    tool_result: None,
                    tool_result_truncated: None,
                    tool_result_error: None,
                });
            }
        } else {
            // Assistant message: message.content is an array of blocks
            let content = match message.get("content").and_then(|c| c.as_array()) {
                Some(arr) => arr,
                None => continue,
            };

            let mut text_parts = Vec::new();
            let mut tool_name = None;
            let mut tool_input: Option<serde_json::Value> = None;
            let mut thinking_text: Option<String> = None;

            for block in content {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(t);
                        }
                    }
                    "tool_use" => {
                        if let (Some(id), Some(name)) = (
                            block.get("id").and_then(|v| v.as_str()),
                            block.get("name").and_then(|v| v.as_str()),
                        ) {
                            tool_use_names.insert(id.to_string(), name.to_string());
                            tool_name = Some(name.to_string());
                        }
                        if let Some(input) = block.get("input") {
                            tool_input = Some(input.clone());
                        }
                    }
                    "thinking" => {
                        if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                            if !t.is_empty() {
                                thinking_text = Some(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }

            let text = text_parts.join("\n");
            if text.is_empty() && tool_name.is_none() && thinking_text.is_none() {
                continue;
            }
            messages.push(ConversationMessage {
                uuid,
                role: "assistant".into(),
                text,
                timestamp,
                tool_name,
                tool_input,
                thinking: thinking_text,
                tool_result: None,
                tool_result_truncated: None,
                tool_result_error: None,
            });
        }
    }

    messages
}

/// Extract the text content from a `tool_result` block.
///
/// Content is either a plain string or an array of content blocks.
/// Array blocks of type `text` are concatenated; `tool_reference`
/// and other non-text sub-blocks are skipped.
fn extract_tool_result_content(block: &serde_json::Value) -> String {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                    b.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
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
    let was_attention = state.attention_panes.write().await.remove(&id).is_some();
    state.attention_details.write().await.remove(&id);
    state.last_attention_notif.write().await.remove(&id);
    if was_attention {
        state.audit_log(AuditEvent::Cancelled {
            pane_id: id.clone(),
            notification_type: "attention".into(),
            reason: "user_input".into(),
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn send_voice(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<VoiceRequest>,
) -> Result<StatusCode, AppError> {
    tracing::info!(pane = %id, locale = ?req.locale, "voice input");
    send_text_to_pane(&id, &req.transcript, req.submit).await?;
    let was_attention = state.attention_panes.write().await.remove(&id).is_some();
    state.attention_details.write().await.remove(&id);
    state.last_attention_notif.write().await.remove(&id);
    if was_attention {
        state.audit_log(AuditEvent::Cancelled {
            pane_id: id.clone(),
            notification_type: "attention".into(),
            reason: "user_voice_input".into(),
        });
    }
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
            .map_err(tmux_err)?;
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
    let was_attention = state.attention_panes.write().await.remove(&id).is_some();
    state.attention_details.write().await.remove(&id);
    state.last_attention_notif.write().await.remove(&id);
    if was_attention {
        state.audit_log(AuditEvent::Cancelled {
            pane_id: id.clone(),
            notification_type: "attention".into(),
            reason: "user_image_input".into(),
        });
    }

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
        .map_err(tmux_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_pane(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let script = format!("tmux send-keys -t {} C-c C-c 2>&1", id);
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(tmux_err)?;
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
        .map_err(tmux_err)?;

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
        .map_err(tmux_err)?;
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
        .map_err(tmux_err)?;
    tracing::info!(%id, "pane killed");
    Ok(StatusCode::NO_CONTENT)
}

async fn create_pane(
    State(_state): State<AppState>,
    Json(req): Json<CreatePaneRequest>,
) -> Result<Json<CreatePaneResponse>, AppError> {
    // Split the current window (horizontally / side-by-side by default; vertical
    // when the caller opts in), inheriting the target pane's working directory,
    // then launch the account-specific Claude launcher inside it.
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
    let flag = match req.direction.as_deref() {
        Some("vertical") => "-v",
        Some("horizontal") | None => "-h",
        Some(other) => {
            return Err(AppError::BadRequest(format!(
                "unknown direction '{}': expected horizontal|vertical",
                other
            )))
        }
    };
    let target_esc = req.target_pane_id.replace('\'', r"'\''");

    // Query the target pane's cwd first — #{pane_current_path} in split-window's
    // -c arg expands relative to the *active* pane, not the -t target.
    let script = format!(
        "CWD=$(tmux display-message -p -t '{target}' '#{{pane_current_path}}'); \
         NEW=$(tmux split-window {flag} -t '{target}' -c \"$CWD\" -d \
         -P -F '#{{session_name}}:#{{window_index}}.#{{pane_index}}' 2>&1); \
         if [ -z \"$NEW\" ]; then echo 'ERR: split-window failed'; exit 1; fi; \
         tmux send-keys -t \"$NEW\" '{cmd}' Enter; \
         echo \"NEW=$NEW\"",
        target = target_esc,
        flag = flag,
        cmd = cmd,
    );

    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(tmux_err)?;

    let new_pane_id = out
        .lines()
        .find_map(|l| l.strip_prefix("NEW="))
        .ok_or_else(|| AppError::BadRequest(format!("create_pane parse failed: {}", out)))?
        .trim()
        .to_string();

    tracing::info!(%new_pane_id, target = %req.target_pane_id, account = %req.account, flag = %flag, "pane split created");

    Ok(Json(CreatePaneResponse {
        pane_id: new_pane_id,
    }))
}

/// Fork the conversation currently running in a pane. Drives Claude's
/// built-in `/branch` slash command on the source pane (Claude creates
/// a new session UUID Y internally and continues in place), then splits
/// a new sibling pane that resumes the *original* session X so the
/// pre-fork conversation is preserved and viewable.
///
/// Session id X is read from the companion's live `AppState.panes`
/// snapshot, not from the request — the caller (mobile APK) only
/// supplies the account so we know whether to launch `ncld` vs `ncld2`.
/// If the pane has no detected session_id yet, the call 400s rather
/// than guessing; the mobile UI should disable the Fork action until
/// a session is bound.
///
/// Operation order matches `launch.ts::forkPaneSession`:
///   1. Send `/branch` to the source pane.
///   2. Sleep 1.5 s so Claude mints Y and stops writing X.
///   3. Split a new pane -h adjacent to the source, inheriting cwd.
///   4. Send `<cmd> -r X` to the new pane.
async fn fork_pane(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ForkPaneRequest>,
) -> Result<Json<CreatePaneResponse>, AppError> {
    if id.is_empty() {
        return Err(AppError::BadRequest("pane id is required".into()));
    }
    // Read session_id + check for collision atomically under one lock.
    // A collision means another pane already owns the source session. If
    // we proceed past it, the split+resume step below will launch a second
    // cli-ncld process appending to the same JSONL — exactly the concurrent
    // writer that produces the tool-use-concurrency 400 the user has been
    // chasing. Better to 409 here and force them to resolve the source
    // state first (kill one of the panes, or wait for the write-mode FD
    // filter in detect_claude_session to re-bind detection correctly).
    let session_id = {
        let panes = state.panes.read().await;
        let rec = panes.get(&id).ok_or_else(|| {
            AppError::BadRequest("pane not found".into())
        })?;
        let sid = rec.dto.claude_session_id.clone().ok_or_else(|| {
            AppError::BadRequest(
                "pane has no detected claude session to fork".into(),
            )
        })?;
        // Is any other pane currently bound to this same session? If so
        // the binding we're about to fork from is already shared —
        // forking again would create a third writer.
        let also_holding: Vec<String> = panes
            .iter()
            .filter(|(other_id, other_rec)| {
                *other_id != &id
                    && other_rec.dto.claude_session_id.as_deref()
                        == Some(sid.as_str())
            })
            .map(|(other_id, _)| other_id.clone())
            .collect();
        if !also_holding.is_empty() {
            return Err(AppError::BadRequest(format!(
                "fork refused: source session {} is also bound to {}. \
                 resolve the duplicate binding first (kill one of the panes \
                 or wait for the session detector to rebind), then retry.",
                sid,
                also_holding.join(", "),
            )));
        }
        sid
    };
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
    let id_esc = id.replace('\'', r"'\''");
    let sid_esc = session_id.replace('\'', r"'\''");
    // Target the new pane by its immutable `%N` pane_id, not by
    // `session:win.pane` index. When split-window creates a pane adjacent
    // to the source, tmux renumbers the remaining panes by spatial layout
    // (so if the source window had panes 1 and 2 and we split pane 1, the
    // new pane becomes index 2 and old pane 2 becomes index 3). The index
    // reported at -P -F time isn't guaranteed to match the post-settle
    // layout, but the %N id is immutable. We then resolve the %N back to
    // the human `session:win.pane` for the API response.
    let script = format!(
        "tmux send-keys -t '{id}' '/branch' Enter; \
         sleep 1.5; \
         CWD=$(tmux display-message -p -t '{id}' '#{{pane_current_path}}'); \
         NEW_PID=$(tmux split-window -h -t '{id}' -c \"$CWD\" -d \
           -P -F '#{{pane_id}}' 2>&1); \
         if [ -z \"$NEW_PID\" ] || [ \"${{NEW_PID#%}}\" = \"$NEW_PID\" ]; then \
           echo \"ERR: split failed: $NEW_PID\"; exit 1; fi; \
         tmux send-keys -t \"$NEW_PID\" '{cmd} -r {sid}' Enter; \
         NEW=$(tmux display-message -p -t \"$NEW_PID\" \
           '#{{session_name}}:#{{window_index}}.#{{pane_index}}'); \
         echo \"NEW=$NEW\"",
        id = id_esc,
        cmd = cmd,
        sid = sid_esc,
    );

    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map_err(tmux_err)?;

    let new_pane_id = out
        .lines()
        .find_map(|l| l.strip_prefix("NEW="))
        .ok_or_else(|| AppError::BadRequest(format!("fork_pane parse failed: {}", out)))?
        .trim()
        .to_string();

    tracing::info!(
        source_pane = %id,
        %new_pane_id,
        source_sid = %session_id,
        account = %req.account,
        "pane forked via /branch"
    );

    Ok(Json(CreatePaneResponse {
        pane_id: new_pane_id,
    }))
}

async fn rate_limits(
    State(state): State<AppState>,
) -> Json<Vec<AccountRateLimit>> {
    Json(state.rate_limits.read().await.clone())
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    #[serde(default = "default_audit_limit")]
    limit: usize,
    since_ms: Option<i64>,
}
fn default_audit_limit() -> usize {
    100
}

async fn audit_log(
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Json<Vec<serde_json::Value>> {
    let entries = match &state.audit_data_dir {
        Some(dir) => {
            super::audit_log::read_recent(dir, q.limit, q.since_ms).await
        }
        None => Vec::new(),
    };
    Json(entries)
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
        .map_err(tmux_err)?;
    Ok(())
}
