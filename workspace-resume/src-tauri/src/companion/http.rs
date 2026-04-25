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
        now_ms, AccountRateLimit, ApprovalDto, ApprovalResponse, AttachRemoteSessionRequest,
        AttachRemoteSessionResponse, CaptureResponse, ConversationMessage, ConversationResponse,
        CreatePaneRequest, CreatePaneResponse, CreateWindowRequest, CreateWindowResponse,
        ForkPaneRequest, HealthDto, ImageRequest, InputRequest, KeyRequest,
        LaunchHostSessionRequest, LaunchHostSessionResponse, PaneDto, ProjectDto,
        RemoteHostsResponse, SessionDto, SyncProjectRequest, SyncProjectResponse, VoiceRequest,
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
        .route("/launch-host-session", post(launch_host_session))
        .route("/sync-project-to-mac", post(sync_project_to_mac))
        .route("/attach-remote-session", post(attach_remote_session))
        .route("/remote-hosts", get(list_remote_hosts))
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
    let remote_hosts = crate::services::remote_hosts::collect_remote_hosts(&state.app);
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

/// Split a pane id into its host target and the local-tmux coord.
///
/// Wire format:
/// * Local panes: `"<session>:<window>.<pane>"` — no `/`.
/// * Remote panes: `"<alias>/<session>:<window>.<pane>"` — leading
///   `alias/` identifies the SSH host.
///
/// This is the single source of truth for "which tmux server do I
/// send this command to" in the companion HTTP layer. Every endpoint
/// that runs a `tmux` command against a specific pane id goes through
/// here before dispatching — otherwise a `mac/main:0.3` pane id would
/// be pasted verbatim into `tmux send-keys -t mac/main:0.3` on the
/// local tmux server, which fails silently (no such pane).
fn split_pane_id_host(id: &str) -> (crate::services::host_target::HostTarget, &str) {
    use crate::services::host_target::HostTarget;
    // Local coords never contain '/'. tmux session names can't either
    // (tmux explicitly forbids them), so a leading `alias/` segment is
    // unambiguously a remote prefix.
    match id.split_once('/') {
        Some((alias, rest)) if !alias.is_empty() && !rest.is_empty() => (
            HostTarget::Remote {
                alias: alias.to_string(),
            },
            rest,
        ),
        _ => (HostTarget::Local, id),
    }
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
    let (session_id, pane_uid, cached_width) = {
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

    // Live-query the pane's actual width at capture time when the cached
    // value is 0 (fresh pane — poller hasn't stamped it yet) or when a
    // client-visible resize might have raced the poll. A stale width
    // causes the VT replay to wrap at the wrong column boundary, which
    // produces character-level mixing in the rendered output. Tight
    // wsl.exe call (one tmux display-message) so the amortized cost is
    // fine for capture requests that happen orders of magnitude less
    // often than the poll tick.
    let pane_width = if cached_width == 0 {
        match query_pane_width_live(&id).await {
            Some(w) => w,
            None => 0, // replay_to_lines falls back to REPLAY_COLS_FALLBACK internally
        }
    } else {
        cached_width
    };

    let log_path = match (session_id.as_ref(), pane_uid.is_empty()) {
        (Some(sid), _) => Some(pane_log::log_path_for_session(sid)),
        (None, false) => Some(pane_log::log_path_for_pending(&pane_uid)),
        (None, true) => None,
    };

    let tail_bytes = if q.lines == 0 {
        pane_log::DEFAULT_TAIL_BYTES
    } else {
        // When the client asks for a specific line count, size the tail
        // budget accordingly. Bumped from 200 → 500 bytes/line after
        // observing that dense cell-diff frames frequently emit >1 KB
        // per visible line (SGR color runs + cursor positioning), which
        // caused `q.lines = 1000` to under-shoot to ~100 rendered frames
        // in practice. 500 covers the observed upper bound without
        // pulling the whole log for a small `lines` request.
        (q.lines as u64).saturating_mul(500)
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

    // Fallback: tmux capture-pane. Local panes use `"<session>:<window>.<pane>"`
    // which matches tmux target syntax directly; remote panes carry an
    // `"<alias>/"` prefix we strip before dispatching to SSH. `lines=0`
    // means "all scrollback" (tmux `-S -`); any positive value captures
    // the last N lines.
    let (capture_host, capture_local_id) = split_pane_id_host(&id);
    let start_flag = if q.lines == 0 {
        "-".to_string()
    } else {
        format!("-{}", q.lines)
    };
    let script = format!(
        "tmux capture-pane -p -e -t {} -S {} 2>&1 || true",
        capture_local_id, start_flag
    );
    let out = crate::commands::tmux::run_tmux_command_async_on(capture_host, script)
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
    // `pane_pid` lives on `PaneRecord` (not PaneDto) because it's only
    // meaningful to the server — the Tauri UI and APK never consume it.
    let (mut bound_session, encoded_project, account_key, pane_pid, pane_host) = {
        let panes = state.panes.read().await;
        let rec = panes.get(&id).ok_or(AppError::NotFound)?;
        (
            rec.dto.claude_session_id.clone(),
            rec.dto.project_encoded_name.clone(),
            rec.dto.claude_account.clone(),
            rec.pane_pid.clone(),
            rec.dto.host.clone(),
        )
    };

    // Each Claude account keeps its transcripts under its own config
    // directory (`.claude` for Andrea, `.claude-b` for Bravura). Resolve
    // against the `accounts` registry instead of hardcoding.
    let config_dir = account_key
        .as_deref()
        .and_then(|k| super::accounts::ACCOUNTS.iter().find(|a| a.key == k))
        .map(|a| a.config_dir)
        .unwrap_or(".claude");

    // Sync detection fallback — when the pane has a project but no bound
    // session_id, try to resolve it NOW before falling through to the
    // MRU-unclaimed heuristic. The MRU heuristic is the prime suspect for
    // "pane A shows pane B's chat" in multi-pane setups: background
    // detection runs on a 1-3s poll, and a user who opens /conversation
    // inside that window gets an educated guess. Running detection inline
    // here costs ~100-300ms (one wsl.exe /proc walk for local, one SSH
    // lsof for remote) but the answer is authoritative. Only kicks in
    // when bound_session is None AND we have a pid to walk from.
    if bound_session.is_none() && !pane_pid.is_empty() {
        let is_remote_pane = matches!(pane_host.as_deref(), Some(h) if h != "local" && !h.is_empty());
        let detected = if is_remote_pane {
            match pane_host.as_deref() {
                Some(alias) => {
                    super::tmux_poller::detect_claude_session_remote(alias, &pane_pid).await
                }
                None => None,
            }
        } else {
            super::tmux_poller::detect_claude_session(&pane_pid).await
        };
        // Apply the same MRU grace-window check as the poller: if the
        // pane is <5s old and detection only produced an MRU result,
        // don't commit — the MRU pick during the startup window is the
        // most common cross-wiring source. Non-MRU (write-mode fd)
        // results pass through regardless of age.
        const SYNC_MRU_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
        let should_reject_mru = detected
            .as_ref()
            .map(|(_, _, is_mru)| *is_mru)
            .unwrap_or(false)
            && {
                let panes = state.panes.read().await;
                panes
                    .get(&id)
                    .map(|rec| rec.first_seen_at.elapsed() < SYNC_MRU_GRACE)
                    .unwrap_or(false)
            };
        let detected = if should_reject_mru {
            tracing::info!(
                target: "mpdiag",
                pane = %id,
                "conversation sync-detect: rejected MRU (pane within grace window)"
            );
            None
        } else {
            detected
        };

        if let Some((sid, _encoded_project, _is_mru)) = detected {
            tracing::info!(
                target: "mpdiag",
                pane = %id,
                session = %sid,
                is_remote = is_remote_pane,
                "conversation: inline session detection bound pane"
            );
            // Write through to the pane record so subsequent /conversation
            // calls and the poller skip the detect queue. Use Heuristic
            // confidence (matches the background path) so an explicit
            // --resume parse can still override it.
            {
                let mut panes = state.panes.write().await;
                if let Some(rec) = panes.get_mut(&id) {
                    if rec.binding_confidence != super::state::BindingConfidence::Explicit {
                        rec.dto.claude_session_id = Some(sid.clone());
                        rec.binding_confidence = super::state::BindingConfidence::Heuristic;
                        rec.dto.updated_at = super::models::now_ms();
                    }
                }
            }
            bound_session = Some(sid);
        }
    }

    // Resolve which JSONL to read:
    // 1. Pane has an explicit session_id (from --resume <uuid> in start cmd or hooks,
    //    or just set inline above by the sync detection fallback)
    // 2. Pane has a project but no bound session — pick the most recently
    //    modified .jsonl in that dir that is NOT already claimed by a
    //    sibling pane. Without that filter two panes sharing a project
    //    (e.g. `ncld` + split) would briefly swap transcripts while the
    //    poller's /proc walk catches up.
    // 3. Otherwise, no way to find a transcript
    let (session_id, jsonl_path, pick_path) = match (bound_session, encoded_project) {
        (Some(sid), Some(proj)) => {
            let path = format!("$HOME/{}/projects/{}/{}.jsonl", config_dir, proj, sid);
            tracing::info!(
                target: "mpdiag",
                pane = %id,
                session = %sid,
                path = %path,
                "conversation pick: bound_session"
            );
            (sid.clone(), path, "bound_session")
        }
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
            // For remote panes the JSONL files live on the remote host's
            // filesystem under its own `$HOME` — run `ls` on that host.
            let (ls_host, _) = split_pane_id_host(&id);
            let script = format!(
                "ls -t \"$HOME/{}/projects/{}\"/*.jsonl 2>/dev/null",
                config_dir, proj
            );
            let listing = crate::commands::tmux::run_tmux_command_async_on(ls_host, script)
                .await
                .map_err(tmux_err)?;
            let listing_count = listing.lines().filter(|l| !l.trim().is_empty()).count();
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
                None => {
                    tracing::warn!(
                        target: "mpdiag",
                        pane = %id,
                        project = %proj,
                        sibling_sids = sibling_sids.len(),
                        listing_count,
                        "conversation pick: MRU exhausted (all JSONLs claimed by siblings)"
                    );
                    return Err(AppError::NotFound);
                }
            };
            let sid = std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            tracing::info!(
                target: "mpdiag",
                pane = %id,
                project = %proj,
                sibling_sids = sibling_sids.len(),
                listing_count,
                chosen_session = %sid,
                path = %path,
                "conversation pick: MRU unclaimed"
            );
            (sid, path, "mru_unclaimed")
        }
        (None, None) => {
            return Err(AppError::BadRequest(
                "pane has no Claude session or project binding".into(),
            ));
        }
    };

    // Pane id carries the host prefix; read_jsonl_with_retry threads it
    // through so `cat` runs on the pane's actual filesystem (Mac for
    // remote panes).
    let raw = read_jsonl_with_retry(&jsonl_path, &id, &session_id, pick_path).await?;

    let messages = parse_jsonl_conversation(&raw, q.after.as_deref());

    Ok(Json(ConversationResponse {
        session_id,
        messages,
    }))
}

/// Read a Claude Code JSONL transcript via `cat`-over-wsl, retrying once
/// if the last non-empty line fails to parse as JSON — the hallmark of a
/// partial read landing in the middle of Claude's write-append-fsync
/// cycle. parse_jsonl_conversation silently drops invalid lines, which
/// surfaces to the user as "the last message is missing"; a 100ms sleep
/// is long enough to clear Claude's write buffer in practice.
async fn read_jsonl_with_retry(
    jsonl_path: &str,
    pane_id: &str,
    session_id: &str,
    pick_path: &'static str,
) -> Result<String, AppError> {
    // Route the `cat` to the pane's host — remote Claude sessions
    // store their JSONL on the Mac filesystem; we can't reach them
    // from local WSL.
    let (host, _) = split_pane_id_host(pane_id);
    let script = format!(
        "cat \"{}\" 2>/dev/null || echo '__FILE_NOT_FOUND__'",
        jsonl_path
    );

    let mut attempt = 0u8;
    loop {
        attempt += 1;
        let raw = crate::commands::tmux::run_tmux_command_async_on(host.clone(), script.clone())
            .await
            .map_err(tmux_err)?;

        if raw.trim() == "__FILE_NOT_FOUND__" || raw.is_empty() {
            tracing::warn!(
                target: "mpdiag",
                pane = %pane_id,
                session = %session_id,
                pick_path,
                raw_len = raw.len(),
                attempt,
                "conversation cat: file not found or empty"
            );
            return Err(AppError::NotFound);
        }

        let last_line_valid_json = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
            .unwrap_or(true);

        tracing::info!(
            target: "mpdiag",
            pane = %pane_id,
            session = %session_id,
            pick_path,
            bytes = raw.len(),
            lines = raw.lines().filter(|l| !l.trim().is_empty()).count(),
            last_line_valid_json,
            attempt,
            "conversation cat result"
        );

        if last_line_valid_json || attempt >= 2 {
            return Ok(raw);
        }

        // Partial-write race: wait for Claude to finish its current fsync,
        // then read again. One retry is enough in practice — Claude's turn
        // commit is microseconds once it starts.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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

    // Route writes to the pane's actual host. Local panes land in WSL
    // /tmp (unchanged pre-refactor behaviour); remote panes land in the
    // remote host's /tmp so the `send_text_to_pane` message below
    // references a file Claude can actually `Read` on its own
    // filesystem. The path string is host-invariant because both WSL
    // Ubuntu and macOS ship a world-writable /tmp by default.
    let (img_host, _) = split_pane_id_host(&id);

    // Decode + write each image. Timestamp + index produces unique filenames
    // so multiple attachments in one request don't collide.
    let base_ts = chrono::Utc::now().timestamp_millis();
    let mut img_paths: Vec<String> = Vec::with_capacity(req.images.len());
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
        let path = format!("/tmp/pane-mgmt/img_{}_{}.{}", base_ts, idx, ext);
        crate::commands::tmux::write_file_to_host_async(
            img_host.clone(),
            path.clone(),
            bytes,
        )
        .await
        .map_err(tmux_err)?;
        img_paths.push(path);
    }

    // Build one message that references every image path inline so Claude
    // reads them all with the Read tool on the same turn.
    let paths_joined = img_paths.join(", ");
    let prompt = req.prompt.as_deref().unwrap_or("").trim();
    let label = if img_paths.len() == 1 { "screenshot" } else { "screenshots" };
    let message = if prompt.is_empty() {
        format!("Look at this {}: {}", label, paths_joined)
    } else {
        format!("{} ({}: {})", prompt, label, paths_joined)
    };

    tracing::info!(pane = %id, host = %img_host.wire_str(), count = img_paths.len(), "images uploaded");
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
    let (host, local_id) = split_pane_id_host(&id);
    let script = format!("tmux send-keys -t {} {}", local_id, req.key);
    crate::commands::tmux::run_tmux_command_async_on(host, script)
        .await
        .map_err(tmux_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_pane(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let (host, local_id) = split_pane_id_host(&id);
    let script = format!("tmux send-keys -t {} C-c C-c 2>&1", local_id);
    crate::commands::tmux::run_tmux_command_async_on(host, script)
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
    if req.session_name.is_empty() || req.project_path.is_empty() {
        return Err(AppError::BadRequest(
            "session_name and project_path are required".into(),
        ));
    }

    // session_name may be prefixed `<alias>/` when the APK picked a
    // remote-host session. Split off the alias so tmux targets the
    // right host's session and the response's pane_id carries the
    // same prefix back.
    let (win_host, local_session) = split_pane_id_host(&req.session_name);

    // Launcher: local panes use ncld family; remote panes use mncld
    // (patched) for consistency with `cc` on Mac. Account `"sully"`
    // picks the tertiary config dir.
    let cmd = match (win_host.is_local(), req.account.as_str()) {
        (true, "bravura") => "ncld2",
        (true, "sully") => "ncld3",
        (true, _) => "ncld",
        (false, "bravura") => "mncld2",
        (false, "sully") => "mncld3",
        (false, _) => "mncld",
    };

    // Single-quote shell escape: a single quote becomes '\''
    let sess_esc = local_session.replace('\'', r"'\''");
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

    let out = crate::commands::tmux::run_tmux_command_async_on(win_host.clone(), script)
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
    // Re-apply the alias prefix so the returned pane_id matches the
    // poller's remote id format (APK stores this and addresses future
    // /panes/{id}/... calls with it).
    let pane_id = match &win_host {
        crate::services::host_target::HostTarget::Local => {
            format!("{}:{}.1", local_session, window_index)
        }
        crate::services::host_target::HostTarget::Remote { alias } => {
            format!("{}/{}:{}.1", alias, local_session, window_index)
        }
    };

    tracing::info!(%pane_id, account = %req.account, "new window created");

    Ok(Json(CreateWindowResponse {
        window_index,
        pane_id,
    }))
}

/// APK-facing "launch Claude on a remote host" path — wraps the
/// `launch_project_session_on` Tauri command so the phone can start a
/// per-project Mac tmux session without needing the desktop in front.
///
/// Host MUST be remote; local launches go through [`create_window`]
/// (phone "Launch window" for WSL stays on that codepath because it
/// uses the currently-attached `main` session and doesn't need
/// session creation). Having a single endpoint serve both hosts would
/// mean the APK has to know about tmux session shapes per host —
/// cleaner to let each endpoint express its own contract.
///
/// Pre-flight: `check_remote_path_exists` on the derived Mac path
/// (`/Users/admin/projects/<basename>`). A 400 with a "not mirrored"
/// message is friendlier than a silent `cd: no such file` inside the
/// new pane.
async fn launch_host_session(
    State(_state): State<AppState>,
    Json(req): Json<LaunchHostSessionRequest>,
) -> Result<Json<LaunchHostSessionResponse>, AppError> {
    if req.project_path.is_empty() {
        return Err(AppError::BadRequest("project_path is required".into()));
    }
    if req.host.is_empty() {
        return Err(AppError::BadRequest("host is required".into()));
    }
    if req.host == "local" {
        return Err(AppError::BadRequest(
            "use POST /api/v1/windows for local (WSL) launches — this endpoint is for remote hosts".into(),
        ));
    }

    // Session name follows the `cc` convention: basename of the project
    // path. `std::path::Path::file_name` peels the last non-empty
    // segment cleanly whether the caller sent a trailing slash or not.
    let basename = std::path::Path::new(&req.project_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "cannot derive session name from project_path '{}'",
                req.project_path,
            ))
        })?;
    if basename.is_empty() {
        return Err(AppError::BadRequest(
            "derived basename is empty — project_path must end in a real directory".into(),
        ));
    }

    // Mac filesystem convention — see `mac_studio_bridge` memory /
    // CreatePaneModal::toMacPath. Other remote hosts might use a
    // different root in the future; when we add one, lift this into a
    // `services::remote_paths` helper keyed by host.
    let remote_path = format!("/Users/admin/projects/{}", basename);

    let path_exists = crate::commands::mac_sync::check_remote_path_exists(
        req.host.clone(),
        remote_path.clone(),
    )
    .await
    .map_err(tmux_err)?;
    if !path_exists {
        return Err(AppError::BadRequest(format!(
            "'{}' is not mirrored to host '{}' — run sync first (desktop: Settings → Remote hosts, or `sync-add-project` on WSL)",
            req.project_display_name.trim(),
            req.host,
        )));
    }

    let (session_name, window_index, pane_index) =
        crate::commands::mac_sync::launch_project_session_on(
            req.host.clone(),
            basename,
            remote_path,
            req.account.clone(),
        )
        .await
        .map_err(tmux_err)?;

    let pane_id = format!(
        "{}/{}:{}.{}",
        req.host, session_name, window_index, pane_index
    );
    tracing::info!(
        %pane_id,
        host = %req.host,
        account = %req.account,
        "host session launched from APK"
    );

    // Fire-and-forget: also open a local WSL tmux window that
    // SSH-attaches to the just-created remote session. When the user
    // walks back to their WezTerm, the Mac session has a visible local
    // terminal view without them having to type `ssh mac` manually.
    // Failures are logged but don't fail the HTTP response — the Mac
    // session exists regardless, and the desktop's "Attach here" button
    // gives the user a manual retry.
    let attach_host = req.host.clone();
    let attach_session = session_name.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::commands::tmux::attach_remote_session(
            attach_host.clone(),
            attach_session.clone(),
        )
        .await
        {
            tracing::debug!(
                host = %attach_host,
                session = %attach_session,
                error = %e,
                "local auto-attach failed (non-fatal)"
            );
        }
    });

    Ok(Json(LaunchHostSessionResponse {
        pane_id,
        window_index,
        session_name,
    }))
}

/// Kick off (or re-run) the Mutagen bidirectional sync between the
/// WSL project directory and its Mac mirror. Wraps the
/// `sync_project_to_mac` Tauri command so the APK can trigger it — the
/// desktop already exposes the same logic via a menu action. Helper
/// is idempotent (see `commands/mac_sync.rs`), so retrying is safe.
/// Errors propagate as 400 rather than 502 because the usual failure
/// mode is "encoded_project not found" (user typo / stale APK cache)
/// rather than Mutagen itself crashing.
async fn sync_project_to_mac(
    State(_state): State<AppState>,
    Json(req): Json<SyncProjectRequest>,
) -> Result<Json<SyncProjectResponse>, AppError> {
    if req.encoded_project.is_empty() {
        return Err(AppError::BadRequest("encoded_project is required".into()));
    }
    let output = crate::commands::mac_sync::sync_project_to_mac(req.encoded_project.clone())
        .await
        .map_err(AppError::BadRequest)?;
    tracing::info!(
        encoded = %req.encoded_project,
        chars = output.len(),
        "sync-add-project triggered from APK"
    );
    Ok(Json(SyncProjectResponse { output }))
}

/// Open (or re-select) a local WSL tmux window that SSH-attaches to a
/// remote tmux session. Wraps the Tauri-side `attach_remote_session`
/// command so the APK can ask the desktop to pre-create a mirror without
/// the user having to switch to WezTerm first.
///
/// Idempotent at the Rust layer (a duplicate request just re-selects
/// the existing `<alias>/<session>` window), so retrying is safe.
async fn attach_remote_session(
    State(_state): State<AppState>,
    Json(req): Json<AttachRemoteSessionRequest>,
) -> Result<Json<AttachRemoteSessionResponse>, AppError> {
    if req.alias.is_empty() {
        return Err(AppError::BadRequest("alias is required".into()));
    }
    if req.session_name.is_empty() {
        return Err(AppError::BadRequest("session_name is required".into()));
    }
    if req.alias == "local" {
        return Err(AppError::BadRequest(
            "alias must be a non-local SSH host — local sessions are already in WSL tmux".into(),
        ));
    }
    crate::commands::tmux::attach_remote_session(req.alias.clone(), req.session_name.clone())
        .await
        .map_err(tmux_err)?;
    let local_window_name = format!("{}/{}", req.alias, req.session_name);
    tracing::info!(
        alias = %req.alias,
        session = %req.session_name,
        "remote session mirrored locally from APK"
    );
    Ok(Json(AttachRemoteSessionResponse { local_window_name }))
}

/// Enumerate distinct remote SSH aliases the app currently knows about.
/// Mirrors the union used by `/api/v1/sessions` and the poller — the
/// APK uses this to populate its host segmented control instead of
/// hardcoding ["mac"], so adding a third host requires no APK rebuild.
async fn list_remote_hosts(
    State(state): State<AppState>,
) -> Json<RemoteHostsResponse> {
    Json(RemoteHostsResponse {
        hosts: crate::services::remote_hosts::collect_remote_hosts(&state.app),
    })
}

async fn kill_window(
    State(_state): State<AppState>,
    Path((session, index)): Path<(String, u32)>,
) -> Result<StatusCode, AppError> {
    if session.is_empty() {
        return Err(AppError::BadRequest("session is required".into()));
    }
    let (host, local_session) = split_pane_id_host(&session);
    let sess_esc = local_session.replace('\'', r"'\''");
    let script = format!("tmux kill-window -t '{}':{} 2>&1", sess_esc, index);
    crate::commands::tmux::run_tmux_command_async_on(host, script)
        .await
        .map_err(tmux_err)?;
    tracing::info!(%session, index, "window killed");
    Ok(StatusCode::NO_CONTENT)
}

async fn kill_pane(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    // id is either local `"session:window.pane"` (e.g. `main:3.1`) or
    // remote `"<alias>/session:window.pane"` (e.g. `mac/akamai:0.0`).
    if id.is_empty() {
        return Err(AppError::BadRequest("pane id is required".into()));
    }
    let (host, local_id) = split_pane_id_host(&id);
    let id_esc = local_id.replace('\'', r"'\''");
    let script = format!("tmux kill-pane -t '{}' 2>&1", id_esc);
    crate::commands::tmux::run_tmux_command_async_on(host, script)
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
    // then launch the account-specific Claude launcher inside it. The launcher
    // name is resolved below once we know the target's host (mncld on Mac,
    // ncld on local) — pre-refactor code had a local-only `cmd` mapping here
    // that's now dead.
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
    // Split target's pane id may be prefixed with `<alias>/` when the
    // source lives on a remote host. Route the split to that host's
    // tmux server, and swap `ncld`/`ncld2` for `mncld`/`mncld2` so the
    // new pane uses the patched Mac binary by convention.
    let (split_host, target_local) = split_pane_id_host(&req.target_pane_id);
    let cmd = match (split_host.is_local(), req.account.as_str()) {
        (true, "bravura") => "ncld2",
        (true, _) => "ncld",
        (false, "bravura") => "mncld2",
        (false, _) => "mncld",
    };
    let target_esc = target_local.replace('\'', r"'\''");

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

    let out = crate::commands::tmux::run_tmux_command_async_on(split_host.clone(), script)
        .await
        .map_err(tmux_err)?;

    let new_local_id = out
        .lines()
        .find_map(|l| l.strip_prefix("NEW="))
        .ok_or_else(|| AppError::BadRequest(format!("create_pane parse failed: {}", out)))?
        .trim()
        .to_string();

    // Re-apply the alias prefix on the returned pane id so the APK
    // stores + addresses it the same way the poller does.
    let new_pane_id = match &split_host {
        crate::services::host_target::HostTarget::Local => new_local_id,
        crate::services::host_target::HostTarget::Remote { alias } => {
            format!("{}/{}", alias, new_local_id)
        }
    };

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
    // Pick the launcher for the new pane's host — local uses ncld,
    // Mac uses mncld (patched Claude binary per convention). Accounts
    // other than andrea get the `2` variant which the bashrc/zshenv
    // functions alias to the correct CLAUDE_CONFIG_DIR.
    let (fork_host, source_local_id) = split_pane_id_host(&id);
    let cmd = match (fork_host.is_local(), req.account.as_str()) {
        (true, "bravura") => "ncld2",
        (true, _) => "ncld",
        (false, "bravura") => "mncld2",
        (false, _) => "mncld",
    };
    let id_esc = source_local_id.replace('\'', r"'\''");
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

    let out = crate::commands::tmux::run_tmux_command_async_on(fork_host.clone(), script)
        .await
        .map_err(tmux_err)?;

    let new_local_id = out
        .lines()
        .find_map(|l| l.strip_prefix("NEW="))
        .ok_or_else(|| AppError::BadRequest(format!("fork_pane parse failed: {}", out)))?
        .trim()
        .to_string();

    // Re-apply the alias prefix on the returned id for consistency
    // with the poller's remote pane id format.
    let new_pane_id = match &fork_host {
        crate::services::host_target::HostTarget::Local => new_local_id,
        crate::services::host_target::HostTarget::Remote { alias } => {
            format!("{}/{}", alias, new_local_id)
        }
    };

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
    let (host, local_id) = split_pane_id_host(id);
    // Escape single quotes for bash single-quoted string
    let escaped = text.replace('\'', r"'\''");
    let submit_line = if submit {
        format!("tmux send-keys -t {} Enter", local_id)
    } else {
        String::new()
    };
    let script = format!(
        "tmux send-keys -t {} -l '{}'; {}",
        local_id, escaped, submit_line
    );
    crate::commands::tmux::run_tmux_command_async_on(host, script)
        .await
        .map_err(tmux_err)?;
    Ok(())
}

/// Query a pane's real `#{pane_width}` from tmux — bypasses the poller's
/// cached value for capture requests that might race a recent pane
/// resize. Routes to the pane's actual host so Mac pane widths are
/// queried against the Mac's tmux server. Returns None on failure;
/// caller treats that as width=0 and lets replay_to_lines use
/// REPLAY_COLS_FALLBACK.
async fn query_pane_width_live(pane_id: &str) -> Option<usize> {
    let (host, local_id) = split_pane_id_host(pane_id);
    let script = format!(
        "tmux display-message -p -t {} '#{{pane_width}}' 2>/dev/null",
        local_id
    );
    let out = crate::commands::tmux::run_tmux_command_async_on(host, script)
        .await
        .ok()?;
    out.trim().parse::<usize>().ok().filter(|&w| w > 0)
}
