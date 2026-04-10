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
    let out = crate::commands::tmux::run_tmux_command(list_cmd)
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

    // Apply updates + detect new panes
    for (id, (session, window_index, window_name, pane_index, cur_cmd, cur_path, _pid, start_cmd))
        in fresh.into_iter()
    {
        // Capture last 5 lines of output for the preview
        let cap_cmd = format!("tmux capture-pane -p -t {} -S -5 2>/dev/null || true", id);
        let cap_out = crate::commands::tmux::run_tmux_command(&cap_cmd).unwrap_or_default();
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
                project_encoded_name: None,
                claude_session_id: claude_session_id.clone(),
                updated_at: now,
            },
            output_hash: [0u8; 32],
            last_output_change: None,
            binding_confidence: binding,
        });

        // Update mutable fields
        rec.dto.session_name = session.clone();
        rec.dto.window_index = window_index;
        rec.dto.window_name = window_name.clone();
        rec.dto.pane_index = pane_index;
        rec.dto.current_command = cur_cmd.clone();
        rec.dto.current_path = cur_path.clone();
        if claude_session_id.is_some() && rec.binding_confidence != BindingConfidence::Explicit {
            rec.dto.claude_session_id = claude_session_id.clone();
            rec.binding_confidence = BindingConfidence::Explicit;
        }

        // Output change detection via hash
        let output_changed = rec.output_hash != output_hash;
        if output_changed {
            rec.output_hash = output_hash;
            rec.last_output_change = Some(Instant::now());
            rec.dto.last_output_preview = preview.clone();

            // Transition to Running if Claude is the current command
            if cur_cmd.eq_ignore_ascii_case("claude") && rec.dto.state == PaneState::Idle {
                let old = rec.dto.state;
                rec.dto.state = PaneState::Running;
                rec.dto.updated_at = now;
                let _ = state.events.send(EventDto::PaneStateChanged {
                    pane_id: id.clone(),
                    old,
                    new: PaneState::Running,
                    at: now,
                });
            }

            let _ = state.events.send(EventDto::PaneOutputChanged {
                pane_id: id.clone(),
                tail: preview.clone(),
                seq: now as u64,
                at: now,
            });
        } else {
            // No new output for IDLE_TIMEOUT → Running decays to Idle
            if rec.dto.state == PaneState::Running {
                if let Some(last) = rec.last_output_change {
                    if last.elapsed() > IDLE_TIMEOUT {
                        let old = rec.dto.state;
                        rec.dto.state = PaneState::Idle;
                        rec.dto.updated_at = now;
                        let _ = state.events.send(EventDto::PaneStateChanged {
                            pane_id: id.clone(),
                            old,
                            new: PaneState::Idle,
                            at: now,
                        });
                    }
                }
            }
        }

        if is_new {
            tracing::debug!(pane = %id, "new pane discovered");
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
