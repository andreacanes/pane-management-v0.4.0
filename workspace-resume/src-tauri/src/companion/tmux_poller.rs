//! 2-second tmux polling loop that maintains the in-memory PaneRecord
//! store and emits state-change / output-change events.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    models::{now_ms, EventDto, PaneDto, PaneState},
    state::{AppState, BindingConfidence, PaneRecord},
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
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
        if let Err(e) = poll_once(&state).await {
            tracing::debug!("tmux poll error: {e}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn poll_once(state: &AppState) -> anyhow::Result<()> {
    // List every pane across every session in one tmux call.
    // Format: session|window_index|window_name|pane_index|current_command|current_path|pane_pid|pane_start_command
    let list_cmd = "tmux list-panes -a -F \
        '#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_current_command}|#{pane_current_path}|#{pane_pid}|#{pane_start_command}' \
        2>/dev/null || true";
    let out = crate::commands::tmux::run_tmux_command_async(list_cmd.to_string())
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut fresh: HashMap<String, (String, u32, String, u32, String, String, String, String)> =
        HashMap::new();

    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 8 {
            continue;
        }
        let session = parts[0].to_string();
        let window_index: u32 = parts[1].parse().unwrap_or(0);
        let window_name = parts[2].to_string();
        let pane_index: u32 = parts[3].parse().unwrap_or(0);
        let current_cmd = parts[4].to_string();
        let current_path = parts[5].to_string();
        let pane_pid = parts[6].to_string();
        let start_cmd = parts[7..].join("|"); // in case start_command contains pipes
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
            ),
        );
    }

    // Capture all pane previews in a single wsl.exe invocation. Each pane's
    // output is bracketed by `---CAP:<id>:BEGIN---` / `---CAP:<id>:END---`
    // markers so we can split the combined stream back into per-pane buckets.
    // Spawning wsl.exe once per pane per poll is what was saturating the
    // blocking pool and lagging the HTTP handlers.
    let captures = {
        let mut script = String::new();
        for id in fresh.keys() {
            script.push_str(&format!(
                "echo '---CAP:{id}:BEGIN---'; tmux capture-pane -p -t {id} -S -5 2>/dev/null; echo '---CAP:{id}:END---'; ",
            ));
        }
        if script.is_empty() {
            String::new()
        } else {
            crate::commands::tmux::run_tmux_command_async(script)
                .await
                .unwrap_or_default()
        }
    };
    let cap_map = parse_capture_batch(&captures);

    // Resolve each pane's working directory to a known project (cached
    // for 30 s — list_projects shells out to wsl.exe and is too expensive
    // to run on every 2 s tick).
    let project_map = ensure_project_cache(state).await;

    // Snapshot cross-pane state once per tick instead of acquiring the
    // locks per-pane. The per-pane loop uses these snapshots to derive the
    // desired PaneState without holding either of these RwLocks.
    let attention_snapshot: HashSet<String> =
        state.attention_panes.read().await.iter().cloned().collect();
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

    // Apply updates + detect new panes
    for (id, (session, window_index, window_name, pane_index, cur_cmd, cur_path, pane_pid, start_cmd))
        in fresh.into_iter()
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
                last_output_preview: preview.clone(),
                project_encoded_name: project_encoded.clone(),
                project_display_name: project_display.clone(),
                claude_session_id: claude_session_id.clone(),
                claude_account: None,
                updated_at: now,
            },
            output_hash: [0u8; 32],
            last_output_change: None,
            binding_confidence: binding,
            claude_account: None,
        });

        // Update mutable fields
        rec.dto.session_name = session.clone();
        rec.dto.window_index = window_index;
        rec.dto.window_name = window_name.clone();
        rec.dto.pane_index = pane_index;
        rec.dto.current_command = cur_cmd.clone();
        rec.dto.current_path = cur_path.clone();
        rec.dto.project_encoded_name = project_encoded.clone();
        rec.dto.project_display_name = project_display.clone();
        if claude_session_id.is_some() && rec.binding_confidence != BindingConfidence::Explicit {
            rec.dto.claude_session_id = claude_session_id.clone();
            rec.binding_confidence = BindingConfidence::Explicit;
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
        }

        // Desired PaneState derivation. Single source of truth:
        //   claude_alive  (pane_current/start_command heuristic)
        //   has_pending   (approvals map, snapshot above)
        //   in_attention  (attention_panes, snapshot above)
        let claude_alive = crate::commands::tmux::pane_is_claude(&cur_cmd, &start_cmd);
        if claude_alive && rec.claude_account.is_none() && !pane_pid.is_empty() {
            detect_queue.push((id.clone(), pane_pid.clone()));
        }
        let has_pending = pending_approval_panes.contains(&id);
        let in_attention = attention_snapshot.contains(&id);
        // If Claude has exited but the pane is still in the attention
        // set, drop the flag — no one is there to respond any more.
        if !claude_alive && in_attention {
            cleared_attention.push(id.clone());
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

        let desired = if !claude_alive {
            PaneState::Idle
        } else if has_pending || in_attention {
            PaneState::Waiting
        } else if output_stale {
            PaneState::Idle
        } else {
            PaneState::Running
        };

        if rec.dto.state != desired {
            let old = rec.dto.state;
            rec.dto.state = desired;
            rec.dto.updated_at = now;
            let _ = state.events.send(EventDto::PaneStateChanged {
                pane_id: id.clone(),
                old,
                new: desired,
                at: now,
            });
        }

        if is_new {
            tracing::debug!(pane = %id, "new pane discovered");
        }
    }

    // Flush cleared attention flags in one write. Done after the per-pane
    // loop so we only grab the attention write lock once per tick.
    if !cleared_attention.is_empty() {
        let mut att = state.attention_panes.write().await;
        for pid in &cleared_attention {
            att.remove(pid);
        }
    }

    // Run Claude account detection for any claude-alive panes that we
    // haven't detected yet. Each detection shells out once to walk
    // /proc/<pid>/environ. Results are cached on the PaneRecord so
    // steady-state cost is zero.
    for (pane_id, shell_pid) in detect_queue {
        if let Some(account) = crate::commands::tmux::detect_claude_account(&shell_pid).await {
            let updated_dto = {
                let mut panes = state.panes.write().await;
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
                let _ = state.events.send(EventDto::PaneUpdated { pane });
            }
        }
    }

    // Drop panes that disappeared from tmux output (session ended)
    let mut panes = state.panes.write().await;
    let gone: Vec<String> = panes
        .keys()
        .filter(|k| !seen.contains(*k))
        .cloned()
        .collect();
    for id in gone {
        panes.remove(&id);
        let _ = state.events.send(EventDto::SessionEnded {
            name: id.clone(),
            at: now_ms(),
        });
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
