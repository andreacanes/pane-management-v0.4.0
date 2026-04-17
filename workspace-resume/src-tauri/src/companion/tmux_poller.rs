//! 2-second tmux polling loop that maintains the in-memory PaneRecord
//! store and emits state-change / output-change events.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    audit_log::AuditEvent,
    models::{now_ms, EventDto, PaneDto, PaneState, WaitingReason},
    state::{AppState, BindingConfidence, PaneRecord},
};

const POLL_INTERVAL: Duration = Duration::from_millis(1000);
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
const PROJECT_CACHE_TTL: Duration = Duration::from_secs(30);

/// Normalize a path for cross-format matching: lowercase, strip trailing
/// slashes, convert backslashes to forward slashes. Used to compare a
/// pane's `current_path` (always POSIX from WSL tmux) against a known
/// project's `actual_path` (which can be POSIX or Windows-style).
pub(super) fn normalize_path(p: &str) -> String {
    p.trim()
        .trim_end_matches(|c| c == '/' || c == '\\')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// Last segment of a normalized path.
fn path_basename(p: &str) -> String {
    p.rsplit('/').find(|s| !s.is_empty()).unwrap_or(p).to_string()
}

/// True when the pane is running Claude Code — either the live foreground
/// command is `claude`/`ncld` or the start_command launched one. Used to
/// decide which panes get pipe-pane logging.
fn is_claude_like(current_cmd: &str, start_cmd: &str) -> bool {
    let cc = current_cmd.trim();
    if cc == "claude" || cc == "ncld" {
        return true;
    }
    // start_command is the full invocation, e.g. "claude --resume <uuid>"
    // or "ncld --resume <uuid>". Match on a whole-word basis so `bash` panes
    // whose history contains "claude" don't match.
    let sc = start_cmd.trim();
    for tok in sc.split_whitespace().take(2) {
        let stripped = tok.rsplit('/').next().unwrap_or(tok);
        if stripped == "claude" || stripped == "ncld" {
            return true;
        }
    }
    false
}

/// Refresh the project cache if older than `PROJECT_CACHE_TTL`. Returns
/// a clone of the current map (small enough to clone — under a few hundred
/// projects is normal). Failures are silent: stale cache is preferable to
/// no cache, since `list_projects` shells out to `wsl.exe` and can fail
/// transiently.
async fn ensure_project_cache(state: &AppState) -> HashMap<String, (String, String)> {
    {
        let cache = state.project_cache.read().await;
        if let Some(fetched) = cache.fetched_at {
            if fetched.elapsed() < PROJECT_CACHE_TTL && !cache.by_path.is_empty() {
                return cache.by_path.clone();
            }
        }
    }
    let projects = match crate::commands::discovery::list_projects().await {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("project cache refresh failed: {e}");
            return state.project_cache.read().await.by_path.clone();
        }
    };
    let mut by_path: HashMap<String, (String, String)> = HashMap::with_capacity(projects.len());
    for p in projects {
        if !p.path_exists || p.actual_path.starts_with("[unresolved]") {
            continue;
        }
        let key = normalize_path(&p.actual_path);
        let display = path_basename(&key);
        by_path.insert(key, (p.encoded_name.clone(), display));
    }
    let mut cache = state.project_cache.write().await;
    cache.fetched_at = Some(Instant::now());
    cache.by_path = by_path.clone();
    by_path
}

/// Look up a pane's current_path in the project cache. Walks up the
/// directory tree so a pane sitting in a project subdirectory still
/// resolves to the correct project root.
fn lookup_project<'a>(
    cache: &'a HashMap<String, (String, String)>,
    pane_path: &str,
) -> Option<&'a (String, String)> {
    let mut p = normalize_path(pane_path);
    loop {
        if let Some(hit) = cache.get(&p) {
            return Some(hit);
        }
        match p.rfind('/') {
            Some(idx) if idx > 0 => p.truncate(idx),
            _ => return None,
        }
    }
}

pub async fn run(state: AppState) {
    loop {
        let t0 = std::time::Instant::now();
        if let Err(e) = poll_once(&state).await {
            tracing::debug!("tmux poll error: {e}");
        }
        let dt = t0.elapsed();
        if dt.as_millis() > 500 {
            tracing::info!(elapsed_ms = %dt.as_millis(), "slow poll tick");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn poll_once(state: &AppState) -> anyhow::Result<()> {
    // Fuse list-panes + per-pane capture-pane into ONE wsl.exe invocation.
    // Each wsl.exe spawn on Windows costs ~200ms of overhead; doing 3 calls
    // per tick (list, capture, stat) stretched the poll to 700ms+. Merging
    // list+capture saves one full round-trip.
    //
    // Output layout:
    //   ---LIST:BEGIN---
    //   <one pane per line, format below>
    //   ---LIST:END---
    //   ---CAP:<id>:BEGIN--- / <capture bytes> / ---CAP:<id>:END---  (repeated)
    //
    // pane_pipe (tmux 3.2+) = 1 when pipe-pane is active.
    // pane_id is tmux's stable `%N` uid.
    // pane_width is columns — fed into avt at /capture time so line wrap
    // matches Claude's TUI and cell-diff renders without character-level
    // mixing.
    let combined = "echo '---LIST:BEGIN---'; \
        tmux list-panes -a -F \
        '#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_current_command}|#{pane_current_path}|#{pane_pid}|#{pane_pipe}|#{pane_id}|#{pane_width}|#{pane_start_command}' \
        2>/dev/null; \
        echo '---LIST:END---'; \
        for id in $(tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}' 2>/dev/null); do \
            echo \"---CAP:$id:BEGIN---\"; \
            tmux capture-pane -p -t \"$id\" -S -5 2>/dev/null; \
            echo \"---CAP:$id:END---\"; \
        done";
    let combined_out = crate::commands::tmux::run_tmux_command_async(combined.to_string())
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    // Split into the LIST section and the capture section.
    let list_begin = "---LIST:BEGIN---";
    let list_end = "---LIST:END---";
    let out = if let (Some(b), Some(e)) = (
        combined_out.find(list_begin),
        combined_out.find(list_end),
    ) {
        let list_start = b + list_begin.len();
        combined_out[list_start..e].trim_matches('\n').to_string()
    } else {
        String::new()
    };
    // Everything after the LIST section is the capture markers.
    let captures_section = combined_out
        .find(list_end)
        .map(|i| combined_out[i + list_end.len()..].to_string())
        .unwrap_or_default();

    let mut seen: HashSet<String> = HashSet::new();
    let mut fresh: HashMap<
        String,
        (String, u32, String, u32, String, String, String, String, String, bool, u16),
    > = HashMap::new();

    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 11 {
            continue;
        }
        let session = parts[0].to_string();
        let window_index: u32 = parts[1].parse().unwrap_or(0);
        let window_name = parts[2].to_string();
        let pane_index: u32 = parts[3].parse().unwrap_or(0);
        let current_cmd = parts[4].to_string();
        let current_path = parts[5].to_string();
        let pane_pid = parts[6].to_string();
        let pane_pipe_active = parts[7] == "1";
        // `pane_id` from tmux is `%N` — strip the `%` for filesystem safety.
        let pane_uid = parts[8].trim_start_matches('%').to_string();
        let pane_width: u16 = parts[9].parse().unwrap_or(0);
        let start_cmd = parts[10..].join("|"); // in case start_command contains pipes
        let id = format!("{}:{}.{}", session, window_index, pane_index);

        seen.insert(id.clone());
        fresh.insert(
            id,
            (
                session,
                window_index,
                window_name,
                pane_index,
                current_cmd,
                current_path,
                pane_pid,
                start_cmd,
                pane_uid,
                pane_pipe_active,
                pane_width,
            ),
        );
    }

    // Captures already fetched in the same wsl call above.
    let cap_map = parse_capture_batch(&captures_section);

    // Resolve each pane's working directory to a known project (cached
    // for 30 s — list_projects shells out to wsl.exe and is too expensive
    // to run on every 2 s tick).
    let project_map = ensure_project_cache(state).await;

    // Snapshot cross-pane state once per tick instead of acquiring the
    // locks per-pane. The per-pane loop uses these snapshots to derive the
    // desired PaneState without holding either of these RwLocks.
    let attention_snapshot: HashMap<String, WaitingReason> =
        state.attention_panes.read().await.clone();
    let pending_approval_panes: HashSet<String> = state
        .approvals
        .read()
        .await
        .values()
        .map(|p| p.dto.pane_id.clone())
        .collect();
    // Attention flags to clear at end-of-loop — populated when a pane's
    // Claude session has exited. We deliberately do NOT clear on fresh
    // output: Claude's Barkeep statusline ticks every few hundred ms, so
    // any "output changed" heuristic would evict idle-prompt attention
    // within one poll tick. Attention sticks until Claude dies or a
    // future ack endpoint explicitly dismisses it.
    let mut cleared_attention: Vec<String> = Vec::new();
    // Panes that need a one-time Claude account detection via
    // /proc/<pid>/environ. Collected during the main loop, resolved
    // after the lock is released.
    let mut detect_queue: Vec<(String, String)> = Vec::new();
    // Panes that need Claude session detection — proc-walk for an open
    // .jsonl file descriptor. Resolved after the main loop the same way.
    let mut session_detect_queue: Vec<(String, String)> = Vec::new();

    // Per-pane log actions to fire AFTER we release the panes write lock.
    // Each entry: (pane_id, pane_uid, old_session, new_target, pipe_active).
    // pane_uid is the tmux stable id (`%N`, `%`-stripped) used for pending
    // log lookup during pending→session migration.
    let mut pipe_actions: Vec<(String, String, Option<String>, super::pane_log::LogTarget, bool)> =
        Vec::new();

    // Apply updates + detect new panes
    for (
        id,
        (session, window_index, window_name, pane_index, cur_cmd, cur_path, pane_pid, start_cmd, pane_uid, pane_pipe_active, pane_width),
    ) in fresh.into_iter()
    {
        let project_match = lookup_project(&project_map, &cur_path).cloned();
        let project_encoded = project_match.as_ref().map(|(e, _)| e.clone());
        let project_display = project_match.as_ref().map(|(_, d)| d.clone());
        let cap_out = cap_map.get(&id).cloned().unwrap_or_default();
        let preview: Vec<String> = cap_out
            .lines()
            .rev()
            .take(5)
            .map(|s| strip_ansi(s))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let output_hash = sha256(&cap_out);

        // Detect claude --resume <uuid> in the start command
        let claude_session_id = parse_resume_uuid(&start_cmd);
        let binding = if claude_session_id.is_some() {
            BindingConfidence::Explicit
        } else {
            BindingConfidence::None
        };

        let mut panes = state.panes.write().await;
        let now = now_ms();

        // Capture OLD session id BEFORE the per-tick update overwrites it —
        // we use it to detect pending → known transitions so we can migrate
        // the pending log into the session log.
        let old_session_id = panes
            .get(&id)
            .and_then(|r| r.dto.claude_session_id.clone());

        let is_new = !panes.contains_key(&id);
        let rec = panes.entry(id.clone()).or_insert_with(|| PaneRecord {
            dto: PaneDto {
                id: id.clone(),
                session_name: session.clone(),
                window_index,
                window_name: window_name.clone(),
                pane_index,
                current_command: cur_cmd.clone(),
                current_path: cur_path.clone(),
                state: PaneState::Idle,
                waiting_reason: None,
                last_output_preview: preview.clone(),
                project_encoded_name: project_encoded.clone(),
                project_display_name: project_display.clone(),
                claude_session_id: claude_session_id.clone(),
                claude_account: None,
                updated_at: now,
                last_activity_at: None,
            },
            output_hash: [0u8; 32],
            last_output_change: None,
            binding_confidence: binding,
            claude_account: None,
            pane_uid: pane_uid.clone(),
            pane_width,
        });

        // Detect pane renumbering: if the same tmux coordinate (session:window.pane)
        // now points to a DIFFERENT underlying pane (different %N uid), clear the
        // stale session binding so proc-walk rebinds to the actual running session.
        // Without this, killing a pane and letting its sibling renumber into the
        // vacated slot leaves the record pointing at the dead session.
        let prev_uid = rec.pane_uid.clone();
        if !prev_uid.is_empty() && prev_uid != pane_uid {
            tracing::info!(
                pane = %id,
                old_uid = %prev_uid,
                new_uid = %pane_uid,
                "pane renumbered into this coordinate — clearing session binding"
            );
            rec.dto.claude_session_id = None;
            rec.binding_confidence = BindingConfidence::None;
            rec.claude_account = None;
            rec.dto.claude_account = None;
            rec.output_hash = [0u8; 32]; // force next hash compare to re-emit
        }

        // Update mutable fields
        rec.dto.session_name = session.clone();
        rec.dto.window_index = window_index;
        rec.dto.window_name = window_name.clone();
        rec.dto.pane_index = pane_index;
        rec.dto.current_command = cur_cmd.clone();
        rec.dto.current_path = cur_path.clone();
        rec.dto.project_encoded_name = project_encoded.clone();
        rec.dto.project_display_name = project_display.clone();
        rec.pane_width = pane_width;
        rec.pane_uid = pane_uid.clone();
        if claude_session_id.is_some() && rec.binding_confidence != BindingConfidence::Explicit {
            rec.dto.claude_session_id = claude_session_id.clone();
            rec.binding_confidence = BindingConfidence::Explicit;
        }

        // Compute the desired pipe-pane target based on the NEW session id
        // (post-update) and queue an action if something needs changing.
        let new_session_id = rec.dto.claude_session_id.clone();
        let is_claude = is_claude_like(&cur_cmd, &start_cmd);
        if is_claude {
            let target = match &new_session_id {
                Some(sid) => super::pane_log::LogTarget::Session(sid.clone()),
                None => super::pane_log::LogTarget::Pending(pane_uid.clone()),
            };
            let needs_action = if !pane_pipe_active {
                true // pipe not attached yet
            } else {
                old_session_id != new_session_id // session transitioned
            };
            if needs_action {
                pipe_actions.push((
                    id.clone(),
                    pane_uid.clone(),
                    old_session_id.clone(),
                    target,
                    pane_pipe_active,
                ));
            }
        }

        // Output change detection via hash — used for the live preview
        // and for setting `last_output_change`, but no longer the sole
        // signal for the Running state.
        let output_changed = rec.output_hash != output_hash;
        if output_changed {
            rec.output_hash = output_hash;
            rec.last_output_change = Some(Instant::now());
            rec.dto.last_output_preview = preview.clone();
            let _ = state.events.send(EventDto::PaneOutputChanged {
                pane_id: id.clone(),
                tail: preview.clone(),
                seq: now as u64,
                at: now,
            });
            tracing::debug!(pane = %id, "emit PaneOutputChanged");
        }

        // Desired PaneState derivation. Single source of truth:
        //   claude_alive  (pane_current/start_command heuristic)
        //   has_pending   (approvals map, snapshot above)
        //   in_attention  (attention_panes, snapshot above)
        let claude_alive = crate::commands::tmux::pane_is_claude(&cur_cmd, &start_cmd);
        if claude_alive && rec.claude_account.is_none() && !pane_pid.is_empty() {
            detect_queue.push((id.clone(), pane_pid.clone()));
        }
        // Queue session detection (proc-walk for open .jsonl) when Claude
        // is alive but we haven't explicitly bound a session_id yet.
        // Skips panes already bound via --resume parsing or hook events.
        if claude_alive
            && rec.binding_confidence == BindingConfidence::None
            && !pane_pid.is_empty()
        {
            session_detect_queue.push((id.clone(), pane_pid.clone()));
        }
        // When Claude exits from a pane, clear the cached account so that
        // if a different account's Claude is launched in the same shell,
        // detection re-runs instead of returning the stale cached result.
        if !claude_alive && rec.claude_account.is_some() {
            if !pane_pid.is_empty() {
                crate::commands::tmux::invalidate_account_cache(&pane_pid);
            }
            rec.claude_account = None;
            rec.dto.claude_account = None;
        }
        // Same for the session binding — when Claude exits, the bound
        // session_id is no longer authoritative for the pane.
        if !claude_alive && rec.binding_confidence != BindingConfidence::None {
            rec.dto.claude_session_id = None;
            rec.binding_confidence = BindingConfidence::None;
        }
        let has_pending = pending_approval_panes.contains(&id);
        let mut in_attention = attention_snapshot.contains_key(&id);
        // If Claude has exited but the pane is still in the attention
        // set, drop the flag — no one is there to respond any more.
        if !claude_alive && in_attention {
            cleared_attention.push(id.clone());
            in_attention = false;
            state.audit_log(AuditEvent::Cancelled {
                pane_id: id.clone(),
                notification_type: "attention".into(),
                reason: "claude_exited".into(),
            });
        }
        // Output-stale detection: if Claude is alive but the pane's output
        // hash hasn't changed for IDLE_TIMEOUT, Claude is sitting at its
        // input prompt — not actively working. That's Idle, not Running.
        //
        // State model:
        //   Idle    = Claude at prompt (output stale) OR Claude process exited
        //   Running = Claude actively generating (output changing)
        //   Waiting = Claude explicitly needs input (approval, question, hook)
        let output_stale = rec
            .last_output_change
            .map(|t| t.elapsed() > IDLE_TIMEOUT)
            .unwrap_or(true); // never had output → treat as stale

        // Clear attention only when Claude resumes working (output is
        // actively changing). Do NOT clear when Claude is idle at the
        // prompt — that's exactly when we want "waiting for input" to
        // persist and the phone notification to stay visible.
        if claude_alive && in_attention && !has_pending && output_changed && !output_stale {
            cleared_attention.push(id.clone());
            in_attention = false;
            state.audit_log(AuditEvent::Cancelled {
                pane_id: id.clone(),
                notification_type: "attention".into(),
                reason: "output_resumed".into(),
            });
        }

        let desired = if !claude_alive {
            PaneState::Idle
        } else if has_pending || in_attention {
            PaneState::Waiting
        } else if output_stale {
            PaneState::Idle
        } else {
            PaneState::Running
        };

        // Approvals outrank attention: a permission prompt is always a
        // Request even if a Stop hook later flagged Continue on the same
        // pane. Non-Waiting states carry no reason.
        let desired_reason = if desired == PaneState::Waiting {
            if has_pending {
                Some(WaitingReason::Request)
            } else {
                attention_snapshot.get(&id).copied()
            }
        } else {
            None
        };

        let state_changed = rec.dto.state != desired;
        let reason_changed = rec.dto.waiting_reason != desired_reason;
        if state_changed || reason_changed {
            let old = rec.dto.state;
            rec.dto.state = desired;
            rec.dto.waiting_reason = desired_reason;
            rec.dto.updated_at = now;
            if state_changed {
                let _ = state.events.send(EventDto::PaneStateChanged {
                    pane_id: id.clone(),
                    old,
                    new: desired,
                    at: now,
                });
            } else {
                // Same state, different reason — push the full DTO so
                // clients can re-render the Waiting chip.
                let _ = state.events.send(EventDto::PaneUpdated {
                    pane: rec.dto.clone(),
                });
            }
        }

        if is_new {
            tracing::debug!(pane = %id, "new pane discovered");
        }
    }

    // Fire pipe-pane (re-)enablement and session transitions in parallel.
    // Each spawns its own wsl.exe via run_tmux_command_async — independent
    // of the rest of the poll flow. Errors log but don't fail the poll.
    for (pane_id, pane_uid, old_session, target, pipe_active) in pipe_actions {
        tokio::spawn(async move {
            // Pending → known transition: migrate the pending log's content
            // into the new session log so history is preserved contiguously
            // across the transition.
            if pipe_active && old_session.is_none() {
                if let super::pane_log::LogTarget::Session(sid) = &target {
                    if let Err(e) =
                        super::pane_log::migrate_pending_to_session(&pane_uid, sid).await
                    {
                        tracing::warn!(pane = %pane_id, "migrate pending log failed: {e}");
                    } else {
                        tracing::info!(
                            pane = %pane_id,
                            uid = %pane_uid,
                            session = %sid,
                            "migrated pending log into session log"
                        );
                    }
                }
            }
            // If the pipe is active but we're switching targets, close first.
            if pipe_active {
                let _ = super::pane_log::disable_pipe_pane(&pane_id).await;
            }
            match super::pane_log::enable_pipe_pane(&pane_id, &target).await {
                Ok(()) => tracing::info!(pane = %pane_id, target = ?target, "pipe-pane configured"),
                Err(e) => tracing::debug!(pane = %pane_id, "pipe-pane enable failed: {e}"),
            }
        });
    }

    // Log rotation: check every Claude pane with an active pipe. We resolve
    // the target from the pane's current state (post-update) so we rotate
    // the CORRECT file when session-switches happen. `rotate_and_reattach`
    // early-returns if the file is under the size cap, so this is cheap to
    // fire every tick.
    let rotation_targets: Vec<(String, super::pane_log::LogTarget)> = {
        let panes = state.panes.read().await;
        panes
            .iter()
            .filter_map(|(id, rec)| {
                // Any pane with a derivable log target is a rotation
                // candidate — even if Claude is no longer alive we still
                // want to cap the on-disk size.
                let target = match rec.dto.claude_session_id.as_ref() {
                    Some(sid) => super::pane_log::LogTarget::Session(sid.clone()),
                    None if !rec.pane_uid.is_empty() => {
                        super::pane_log::LogTarget::Pending(rec.pane_uid.clone())
                    }
                    None => return None,
                };
                Some((id.clone(), target))
            })
            .collect()
    };
    for (pane_id, target) in rotation_targets {
        tokio::spawn(async move {
            match super::pane_log::rotate_and_reattach(&pane_id, &target).await {
                Ok(true) => tracing::info!(pane = %pane_id, "rotated pane log"),
                Ok(false) => {}
                Err(e) => tracing::warn!(pane = %pane_id, "pane log rotation failed: {e}"),
            }
        });
    }

    // Flush cleared attention flags in one write. Done after the per-pane
    // loop so we only grab the attention write lock once per tick.
    if !cleared_attention.is_empty() {
        let mut att = state.attention_panes.write().await;
        for pid in &cleared_attention {
            att.remove(pid);
        }
        drop(att);
        let mut det = state.attention_details.write().await;
        for pid in &cleared_attention {
            det.remove(pid);
        }
        drop(det);
        let mut lan = state.last_attention_notif.write().await;
        for pid in &cleared_attention {
            lan.remove(pid);
        }
        drop(lan);
        // Mark ntfy backlog entries for cleared panes as resolved so they
        // won't replay on SSE reconnect.
        for pid in &cleared_attention {
            super::hook_sink::mark_ntfy_resolved_by_pane(&state, pid).await;
        }
    }

    // Claude account + session detection both shell out via wsl.exe and
    // can take 500ms-1s per pane. Running them sequentially inside the poll
    // blocked the main loop for 10-15s on the first boot (9 panes × ~1s ×
    // 2 detections). Spawn each as a background task that writes into
    // state.panes when done — the next poll tick will see the result.

    let detect_state = state.clone();
    tokio::spawn(async move {
        for (pane_id, shell_pid) in detect_queue {
            if let Some(account) =
                crate::commands::tmux::detect_claude_account(&shell_pid).await
            {
                let updated_dto = {
                    let mut panes = detect_state.panes.write().await;
                    if let Some(rec) = panes.get_mut(&pane_id) {
                        rec.claude_account = Some(account.clone());
                        rec.dto.claude_account = Some(account);
                        rec.dto.updated_at = now_ms();
                        Some(rec.dto.clone())
                    } else {
                        None
                    }
                };
                if let Some(pane) = updated_dto {
                    let _ = detect_state.events.send(EventDto::PaneUpdated { pane });
                }
            }
        }
    });

    // Resolve session bindings: walk /proc for each pane that runs claude
    // but has no explicit binding. Also runs as a background task so it
    // doesn't block the main poll cadence. Pending→session log migration
    // fires as the session id gets bound.
    let session_state = state.clone();
    tokio::spawn(async move {
        let state = session_state; // shadow to match the original variable name below
        for (pane_id, shell_pid) in session_detect_queue {
        if let Some((session_id, encoded_project, is_mru)) =
            detect_claude_session(&shell_pid).await
        {
            // Migration info captured while holding the lock, acted on
            // after we drop it — avoids awaiting inside the write guard.
            let mut migrate_info: Option<(String, String)> = None; // (pane_uid, session_id)
            let updated_dto = {
                let mut panes = state.panes.write().await;
                if is_mru {
                    let claimed_elsewhere = panes.iter().any(|(other_id, other_rec)| {
                        other_id != &pane_id
                            && other_rec.dto.claude_session_id.as_deref() == Some(&session_id)
                    });
                    if claimed_elsewhere {
                        continue;
                    }
                }
                if let Some(rec) = panes.get_mut(&pane_id) {
                    if rec.binding_confidence != BindingConfidence::Explicit {
                        // Capture the Pending → Session transition now that
                        // the session_id is being bound. If the pane's
                        // session_id was None before this call, we need to
                        // migrate pending/<pane_uid>.log into
                        // sessions/<session_id>.log and re-target pipe-pane.
                        let was_pending = rec.dto.claude_session_id.is_none();
                        if was_pending && !rec.pane_uid.is_empty() {
                            migrate_info =
                                Some((rec.pane_uid.clone(), session_id.clone()));
                        }
                        rec.dto.claude_session_id = Some(session_id.clone());
                        rec.binding_confidence = BindingConfidence::Heuristic;
                        if rec.dto.project_encoded_name.is_none() {
                            rec.dto.project_encoded_name = Some(encoded_project);
                        }
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
            // Fire the pipe-pane re-target + pending migration.
            if let Some((uid, sid)) = migrate_info {
                let pid_clone = pane_id.clone();
                tokio::spawn(async move {
                    // Stop the pending pipe before moving the file.
                    let _ = super::pane_log::disable_pipe_pane(&pid_clone).await;
                    if let Err(e) =
                        super::pane_log::migrate_pending_to_session(&uid, &sid).await
                    {
                        tracing::warn!(pane = %pid_clone, "migrate pending failed: {e}");
                    } else {
                        tracing::info!(
                            pane = %pid_clone,
                            uid = %uid,
                            session = %sid,
                            "migrated pending log into session log"
                        );
                    }
                    let target = super::pane_log::LogTarget::Session(sid);
                    if let Err(e) =
                        super::pane_log::enable_pipe_pane(&pid_clone, &target).await
                    {
                        tracing::debug!(pane = %pid_clone, "pipe re-target failed: {e}");
                    }
                });
            }
        }
        }
    }); // end tokio::spawn for session_detect_queue

    // Refresh `last_activity_at` from JSONL file mtime. This is the
    // honest "last conversation activity" signal — it advances when
    // Claude writes a turn (assistant/user/tool) and is NOT bumped by
    // phone capture views or state-only transitions like updated_at.
    // One batched wsl.exe call per tick stats every bound pane's JSONL.
    let stat_targets: Vec<(String, String)> = {
        let panes_r = state.panes.read().await;
        panes_r
            .values()
            .filter_map(|rec| {
                let sid = rec.dto.claude_session_id.as_ref()?;
                let proj = rec.dto.project_encoded_name.as_ref()?;
                Some((
                    rec.dto.id.clone(),
                    format!("$HOME/.claude/projects/{}/{}.jsonl", proj, sid),
                ))
            })
            .collect()
    };
    if !stat_targets.is_empty() {
        // The JSONL mtime refresh is informational only (powers the
        // `last_activity_at` field used by the Stashed filter). It should
        // NOT block the poll — run it as a background task so the main
        // loop can immediately emit PaneOutputChanged events and start
        // the next tick.
        let stat_state = state.clone();
        tokio::spawn(async move {
            let mut script = String::new();
            for (id, path) in &stat_targets {
                script.push_str(&format!(
                    "printf '%s|' '{}'; stat -c '%Y' \"{}\" 2>/dev/null || echo '0'\n",
                    id, path
                ));
            }
            if let Ok(out) = crate::commands::tmux::run_tmux_command_async(script).await {
                let mut updates: Vec<(String, i64)> = Vec::new();
                for line in out.lines() {
                    let mut parts = line.splitn(2, '|');
                    let id = parts.next().unwrap_or("");
                    let mtime_s: i64 = parts
                        .next()
                        .unwrap_or("0")
                        .trim()
                        .parse()
                        .unwrap_or(0);
                    if mtime_s > 0 && !id.is_empty() {
                        updates.push((id.to_string(), mtime_s * 1000));
                    }
                }
                if !updates.is_empty() {
                    let mut panes_w = stat_state.panes.write().await;
                    for (id, ms) in updates {
                        if let Some(rec) = panes_w.get_mut(&id) {
                            if rec.dto.last_activity_at != Some(ms) {
                                rec.dto.last_activity_at = Some(ms);
                            }
                        }
                    }
                }
            }
        });
    }

    // Drop panes that disappeared from tmux output. Emit PaneRemoved
    // per pane, then SessionEnded only for sessions with no remaining panes.
    let mut panes = state.panes.write().await;
    let gone: Vec<String> = panes
        .keys()
        .filter(|k| !seen.contains(*k))
        .cloned()
        .collect();
    let mut affected_sessions: HashSet<String> = HashSet::new();
    for id in &gone {
        if let Some(rec) = panes.get(id) {
            affected_sessions.insert(rec.dto.session_name.clone());
        }
        panes.remove(id);
        let _ = state.events.send(EventDto::PaneRemoved {
            pane_id: id.clone(),
            at: now_ms(),
        });
    }
    // Only emit SessionEnded when the entire session has no remaining panes.
    for session in affected_sessions {
        let still_has_panes = panes.values().any(|r| r.dto.session_name == session);
        if !still_has_panes {
            let _ = state.events.send(EventDto::SessionEnded {
                name: session,
                at: now_ms(),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sha256(s: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().into()
}

/// Split the combined batched-capture stdout back into per-pane buckets,
/// using the `---CAP:<id>:BEGIN---` / `---CAP:<id>:END---` markers emitted
/// by the batch script.
fn parse_capture_batch(output: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut current_id: Option<String> = None;
    let mut buffer = String::new();
    for line in output.lines() {
        if let Some(id) = line.strip_prefix("---CAP:").and_then(|s| s.strip_suffix(":BEGIN---")) {
            current_id = Some(id.to_string());
            buffer.clear();
        } else if let Some(id) = line.strip_prefix("---CAP:").and_then(|s| s.strip_suffix(":END---")) {
            if current_id.as_deref() == Some(id) {
                out.insert(id.to_string(), buffer.clone());
            }
            current_id = None;
            buffer.clear();
        } else if current_id.is_some() {
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(line);
        }
    }
    out
}

/// Strip ANSI CSI escapes for the output preview. Keeps it simple and
/// mobile-friendly; full ANSI rendering is a future enhancement.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '\x1b' && iter.peek() == Some(&'[') {
            iter.next(); // consume '['
            // Consume until a letter in @..~ range terminates the CSI sequence
            for c2 in iter.by_ref() {
                if ('@'..='~').contains(&c2) {
                    break;
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Extract the uuid from `claude --resume <uuid>` or `claude -r <uuid>`
/// in a pane's start command. Returns None if not a resume invocation.
fn parse_resume_uuid(cmd: &str) -> Option<String> {
    let mut parts = cmd.split_whitespace();
    let _ = parts.next()?; // `claude`
    let mut expect_uuid = false;
    for part in parts {
        if expect_uuid {
            // Minimal UUID shape check: 36 chars with 4 dashes
            if part.len() == 36 && part.matches('-').count() == 4 {
                return Some(part.to_string());
            }
            expect_uuid = false;
        }
        if part == "--resume" || part == "-r" {
            expect_uuid = true;
        }
    }
    None
}

/// Find the Claude session JSONL for a tmux pane.
///
/// Strategy:
///   1. Walk descendants of `shell_pid` to find the running Claude PID
///   2. Read its `cwd` (authoritative — survives `cd` inside the pane)
///   3. Encode cwd to the matching `.claude/projects/<encoded>/` dir
///   4. Pick the most-recently-modified `.jsonl` in that dir
///
/// Returns `(session_id, encoded_project)` on success.
///
/// MRU works because Claude actively writes the active JSONL on every
/// turn. Start-time matching does NOT work — Claude resumes existing
/// sessions, so the JSONL's first record can predate the Claude PID by
/// hours or days. The cwd lookup ensures we look in the right project.
pub(super) async fn detect_claude_session(shell_pid: &str) -> Option<(String, String, bool)> {
    if shell_pid.is_empty() {
        return None;
    }
    let script = format!(
        r#"
walk() {{
  local pid=$1 depth=$2
  [ "$depth" -gt 5 ] && return 1
  [ -d "/proc/$pid" ] || return 1
  local comm
  comm=$(cat "/proc/$pid/comm" 2>/dev/null)
  if [ "$comm" = "claude" ]; then
    echo "$pid"
    return 0
  fi
  if [ "$comm" = "node" ]; then
    if grep -aq claude "/proc/$pid/cmdline" 2>/dev/null \
      || tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -q claude; then
      echo "$pid"
      return 0
    fi
  fi
  for child in $(pgrep -P "$pid" 2>/dev/null); do
    if walk "$child" $((depth + 1)); then return 0; fi
  done
  return 1
}}
claude_pid=$(walk {pid} 0)
[ -z "$claude_pid" ] && exit 1

# First-choice signal: catch a JSONL file descriptor while Claude has
# it open. Claude reopens the file for each message write, so a short
# burst of fd scans is usually enough for an actively-used pane. This
# disambiguates two Claude instances in the same project dir — MRU
# alone would map both to the last-written file and swap transcripts.
# The `read -t 0.1 < /dev/null` trick gives us fractional-second
# sleeps without needing `sleep 0.1` (not portable across shells).
for i in $(seq 1 15); do
  for fd in /proc/$claude_pid/fd/*; do
    target=$(readlink "$fd" 2>/dev/null) || continue
    case "$target" in
      "$HOME/.claude/projects/"*/*.jsonl|"$HOME/.claude-b/projects/"*/*.jsonl)
        echo "$target"
        exit 0
        ;;
    esac
  done
  read -t 0.1 </dev/null 2>/dev/null || true
done

# Fallback: cwd → MRU in project dir. Caller should treat this as
# low-confidence and skip binding if a sibling pane already claims
# this session_id. Prefix `MRU:` flags it as the weak path.
cwd=$(readlink "/proc/$claude_pid/cwd" 2>/dev/null)
[ -z "$cwd" ] && exit 1
encoded=$(echo "$cwd" | tr '/. ' '-')
for cfg in ".claude" ".claude-b"; do
  proj_dir="$HOME/$cfg/projects/$encoded"
  [ -d "$proj_dir" ] || continue
  mru=$(ls -t "$proj_dir"/*.jsonl 2>/dev/null | head -1)
  if [ -n "$mru" ]; then
    echo "MRU:$mru"
    exit 0
  fi
done
exit 1
"#,
        pid = shell_pid
    );
    let out = crate::commands::tmux::run_tmux_command_async(script)
        .await
        .ok()?;
    let first = out.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let (path, is_mru) = if let Some(rest) = first.strip_prefix("MRU:") {
        (rest, true)
    } else {
        (first, false)
    };
    // Path: /home/<user>/.claude[-b]/projects/<encoded_project>/<session_id>.jsonl
    let p = std::path::Path::new(path);
    let session_id = p.file_stem()?.to_str()?.to_string();
    let encoded_project = p.parent()?.file_name()?.to_str()?.to_string();
    Some((session_id, encoded_project, is_mru))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_resume_uuid() {
        assert_eq!(
            parse_resume_uuid("claude --resume 3f4594da-44f6-4c22-ba45-24f7a5304d47"),
            Some("3f4594da-44f6-4c22-ba45-24f7a5304d47".to_string())
        );
        assert_eq!(
            parse_resume_uuid("claude -r 3f4594da-44f6-4c22-ba45-24f7a5304d47 --effort high"),
            Some("3f4594da-44f6-4c22-ba45-24f7a5304d47".to_string())
        );
        assert_eq!(parse_resume_uuid("claude"), None);
        assert_eq!(parse_resume_uuid("bash"), None);
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("no ansi"), "no ansi");
        assert_eq!(strip_ansi("carry\rreturn"), "carryreturn");
    }
}
