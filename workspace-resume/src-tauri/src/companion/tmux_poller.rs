//! 2-second tmux polling loop that maintains the in-memory PaneRecord
//! store and emits state-change / output-change events.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
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

/// How long an assignment's slot must be continuously missing from the
/// live pane set before we evict it from the store. Long enough that
/// brief tmux reshuffles (renumber-windows, swap-pane) don't trip it;
/// short enough that "I killed this pane 2 minutes ago" is visible as
/// a cleared slot.
const STALE_GRACE: Duration = Duration::from_secs(60);

/// Max age of a successful remote-host poll before we refuse to
/// evict that host's assignments. If the Mac has been unreachable
/// for more than this, we assume the missing panes are a transport
/// problem, not user intent. The sweep then waits for a successful
/// remote poll to re-establish ground truth before resuming.
const REMOTE_HOST_STALE: Duration = Duration::from_secs(30);

/// How often the sweep runs, measured in main-loop ticks. 10 × 1s =
/// every ~10 s — roughly 6× inside the 60 s grace window, so an
/// actually-missing slot gets 5-6 consecutive observations before
/// eviction.
const SWEEP_EVERY_N_TICKS: u32 = 10;

/// Per-assignment first-observed-missing timestamps, accumulated across
/// sweep cycles so the grace-period clock persists between ticks.
/// Entries for slots that come back alive are removed; entries that
/// cross the grace threshold are evicted and also removed. Module-local
/// because the sweep is the sole reader/writer.
static MISSING_SINCE: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Last-known active window per session key (`"<host>|<session>"` →
/// `window_index`). `poll_once` compares each tick's snapshot against
/// this and emits `WindowFocusChanged` only on a real change; without
/// the diff we'd re-emit every tick for every session, flooding the
/// event bus. Module-local because the poll loop is the sole
/// reader/writer (single-consumer, sequential).
static LAST_ACTIVE_WINDOWS: LazyLock<Mutex<HashMap<String, u32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
/// command is `claude`/`ncld` or a `cli-ncld-*.bin` patched binary from
/// `~/claude-patching`, or the start_command launched one. Used to decide
/// which panes get pipe-pane logging — must stay in sync with
/// `commands::tmux::pane_is_claude`, though this version is intentionally
/// stricter (token match instead of substring) to avoid enabling pipe-pane
/// on plain shells whose history text happens to include "claude".
/// Scan the last 5-line capture preview for Claude Code TUI markers.
/// Used when `current_command` is `ssh` — tmux reports the foreground
/// SSH process, not the Claude instance running on the far side of the
/// tunnel, so the shell-command heuristic can't see it. Checking for the
/// word "claude" (lowercased, any position) catches both Claude's own
/// banner lines (`Claude Code v2.x`, `with max effort`) and its prompt
/// decoration (`claude >`). Some false positives are acceptable here —
/// the cost of mislabeling a random `ssh` session is one misplaced
/// green dot, not a functional regression.
fn preview_looks_claude(preview: &[String]) -> bool {
    for line in preview {
        let lower = line.to_ascii_lowercase();
        if lower.contains("claude") || lower.contains("with max effort") {
            return true;
        }
    }
    false
}

/// Whole-word containment — `w` must be bounded by non-alphanumeric
/// characters (or the string ends). Prevents `"cld"` from matching
/// inside `"ncld"` or `"cli-mncld-118.bin"`.
fn contains_word(haystack: &str, w: &str) -> bool {
    for (idx, _) in haystack.match_indices(w) {
        let bytes = haystack.as_bytes();
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let end = idx + w.len();
        let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// Synthesize a `claude_account` value for a Claude-alive pane whose
/// current command is `ssh` — the /proc walker sees only the ssh
/// process and never reaches a Claude descendant, so the usual
/// detection returns None. Scan `start_command` + the last-capture
/// preview for account markers, prioritizing explicit config-dir
/// paths > `-3`/`-2` wrapper suffixes > identity names > primary
/// fallback. Returns `None` only when nothing Claude-ish is visible
/// at all (caller already knows the pane is Claude-alive, so this is
/// rare — falls through to bare `claude` / `mncld` → andrea).
fn detect_account_from_ssh_context(
    preview: &[String],
    start_command: &str,
) -> Option<String> {
    let mut hay = String::with_capacity(256);
    hay.push_str(&start_command.to_ascii_lowercase());
    hay.push(' ');
    for line in preview {
        hay.push_str(&line.to_ascii_lowercase());
        hay.push(' ');
    }

    // Tier 1 — explicit CLAUDE_CONFIG_DIR path. Most authoritative: a
    // literal `.claude-c` / `.claude-b` in the ssh command (e.g.
    // `ssh mac -t 'env CLAUDE_CONFIG_DIR=~/.claude-c mncld'`) pins the
    // account unambiguously.
    if hay.contains(".claude-c") {
        return Some("sully".to_string());
    }
    if hay.contains(".claude-b") {
        return Some("bravura".to_string());
    }

    // Tier 2 — `-3` / `-2` suffixed wrappers from ~/.bashrc or
    // ~/.zshenv (`cld3`, `ncld3`, `claude3`, `mncld3` → sully;
    // `cld2`, `ncld2`, `claude2`, `mncld2` → bravura), or the
    // identity name as a standalone token (e.g. `cc akamai sully`).
    for w in ["cld3", "ncld3", "claude3", "mncld3", "sully"] {
        if contains_word(&hay, w) {
            return Some("sully".to_string());
        }
    }
    for w in ["cld2", "ncld2", "claude2", "mncld2", "bravura"] {
        if contains_word(&hay, w) {
            return Some("bravura".to_string());
        }
    }

    // Tier 3 — bare wrapper / patched-binary names → andrea (the
    // primary account). `cli-ncld`/`cli-mncld` are the patched native
    // builds; `cld`/`ncld`/`claude`/`mncld` are the bare wrappers;
    // `cc` is the Mac `~/bin/cc <project>` launcher with no account
    // arg (defaults to andrea); `andrea` catches literal identity
    // mentions.
    for w in [
        "cld", "ncld", "claude", "mncld", "cli-ncld", "cli-mncld", "cc", "andrea",
    ] {
        if contains_word(&hay, w) {
            return Some("andrea".to_string());
        }
    }

    None
}

// Former `is_claude_like` has been removed. It was intentionally stricter
// than `commands::tmux::pane_is_claude` to avoid enabling pipe-pane on
// shell panes whose start_command history happened to contain "claude" —
// but `pane_is_claude` already gates its indirect-start branch on
// `!is_shell`, so the shell false-positive case it defended against does
// not actually fire there. And `is_claude_like` itself missed Andrea's
// `mncld`, `cld2`, `cld3` wrappers (they don't match the `claude`/`ncld`/
// `cli-ncld` token allow-list), which resulted in those panes silently
// going without pipe-pane logging. Use `pane_is_claude` everywhere.

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

/// Look up a pane's current_path in the project cache.
///
/// Two passes:
///
/// 1. **Direct tree walk.** Normalize the pane's cwd and walk up the
///    directory tree, checking each ancestor against the cache's
///    (WSL-side) project paths. Handles the local case where the pane
///    sits inside a WSL project's root (exact hit) or a subdirectory
///    of it (ancestor hit).
///
/// 2. **Basename fallback.** When no ancestor matched by path, walk up
///    again comparing each ancestor's *basename* against the basename
///    of every cached project path. This captures the WSL ↔ Mac
///    Mutagen pair convention — `/home/andrea/<name>` on WSL is the
///    same project as `/Users/admin/projects/<name>` on Mac because
///    the user set up the mirror that way. Without this fallback, a
///    Mac pane's `project_encoded_name` would be the Mac-style
///    `-Users-admin-projects-<name>` which doesn't match any entry in
///    `/api/v1/projects` (WSL-encoded), so the APK and desktop grid
///    can't resolve a display name.
///
///    Ambiguity guard: if two WSL projects share a basename (e.g.
///    `~/foo-a/widgets` and `~/foo-b/widgets`), we return None rather
///    than picking one arbitrarily. Those cases are rare enough that
///    falling through to "no project bound" (current behaviour) is
///    safer than wiring the wrong identity.
fn lookup_project<'a>(
    cache: &'a HashMap<String, (String, String)>,
    pane_path: &str,
) -> Option<&'a (String, String)> {
    let mut p = normalize_path(pane_path);

    // Pass 1: direct path tree walk.
    {
        let mut q = p.clone();
        loop {
            if let Some(hit) = cache.get(&q) {
                return Some(hit);
            }
            match q.rfind('/') {
                Some(idx) if idx > 0 => q.truncate(idx),
                _ => break,
            }
        }
    }

    // Pass 2: basename walk. Reuse `p` as the cursor.
    loop {
        let base = path_basename(&p);
        if !base.is_empty() {
            let mut found: Option<&'a (String, String)> = None;
            let mut ambiguous = false;
            for (cache_key, value) in cache.iter() {
                if path_basename(cache_key) == base {
                    if found.is_some() {
                        ambiguous = true;
                        break;
                    }
                    found = Some(value);
                }
            }
            if !ambiguous {
                if let Some(hit) = found {
                    return Some(hit);
                }
            }
        }
        match p.rfind('/') {
            Some(idx) if idx > 0 => p.truncate(idx),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod lookup_project_tests {
    use super::*;

    fn cache(items: &[(&str, &str, &str)]) -> HashMap<String, (String, String)> {
        let mut m = HashMap::new();
        for (path, encoded, display) in items {
            m.insert(normalize_path(path), (encoded.to_string(), display.to_string()));
        }
        m
    }

    #[test]
    fn direct_match() {
        let c = cache(&[("/home/andrea/akamai", "enc-a", "akamai")]);
        let hit = lookup_project(&c, "/home/andrea/akamai").unwrap();
        assert_eq!(hit.0, "enc-a");
    }

    #[test]
    fn subdir_match() {
        let c = cache(&[("/home/andrea/akamai", "enc-a", "akamai")]);
        let hit = lookup_project(&c, "/home/andrea/akamai/src/deep").unwrap();
        assert_eq!(hit.0, "enc-a");
    }

    #[test]
    fn mac_basename_fallback() {
        let c = cache(&[("/home/andrea/akamai", "enc-a", "akamai")]);
        let hit = lookup_project(&c, "/Users/admin/projects/akamai").unwrap();
        assert_eq!(hit.0, "enc-a");
    }

    #[test]
    fn mac_subdir_basename_fallback() {
        let c = cache(&[("/home/andrea/akamai", "enc-a", "akamai")]);
        let hit = lookup_project(&c, "/Users/admin/projects/akamai/src/deep").unwrap();
        assert_eq!(hit.0, "enc-a");
    }

    #[test]
    fn ambiguous_basename_returns_none() {
        let c = cache(&[
            ("/home/andrea/foo-a/widgets", "enc-x", "widgets"),
            ("/home/andrea/foo-b/widgets", "enc-y", "widgets"),
        ]);
        assert!(lookup_project(&c, "/Users/admin/projects/widgets").is_none());
    }

    #[test]
    fn miss_returns_none() {
        let c = cache(&[("/home/andrea/akamai", "enc-a", "akamai")]);
        assert!(lookup_project(&c, "/tmp/nowhere").is_none());
    }
}

/// Remote polling cadence. Slower than local (1s) because each tick is
/// a full SSH round-trip over Tailscale — ~10-30ms with ControlMaster
/// warm, 200-500ms on a cold handshake. 3s is responsive enough for
/// approval / attention events, slow enough to absorb a DERP-relay
/// fallback without flooding the Mac.
const REMOTE_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Clear per-Claude-lifecycle fields on a pane when `claude_alive` is
/// false. Shared between the local and remote poll functions so that
/// remote panes stop accumulating stale `claude_account`,
/// `claude_session_id`, and `claude_effort` after Claude exits on the
/// far host. Also drops any pending attention flag since no one on the
/// exited process can respond to it.
///
/// Idempotent: safe to call on every tick where `claude_alive` is
/// false — after the first effective clear, subsequent calls see empty
/// fields and skip. Returns true if the pane had an attention flag
/// that was just cleared, so the caller can override its local
/// `in_attention` value before state derivation.
pub(super) fn apply_claude_exit_cleanup(
    rec: &mut PaneRecord,
    pane_id: &str,
    pane_pid: &str,
    attention_snapshot: &HashMap<String, WaitingReason>,
    cleared_attention: &mut Vec<String>,
    state: &AppState,
) -> bool {
    if rec.claude_account.is_some() {
        if !pane_pid.is_empty() {
            crate::commands::tmux::invalidate_account_cache(pane_pid);
        }
        rec.claude_account = None;
        rec.dto.claude_account = None;
    }

    if rec.binding_confidence != BindingConfidence::None {
        rec.dto.claude_session_id = None;
        rec.binding_confidence = BindingConfidence::None;
    }

    if rec.dto.claude_effort.is_some() {
        rec.dto.claude_effort = None;
        let _ = state.events.send(EventDto::PaneUpdated {
            pane: rec.dto.clone(),
        });
    }

    let had_attention = attention_snapshot.contains_key(pane_id);
    if had_attention {
        cleared_attention.push(pane_id.to_string());
        state.audit_log(AuditEvent::Cancelled {
            pane_id: pane_id.to_string(),
            notification_type: "attention".into(),
            reason: "claude_exited".into(),
        });
    }
    had_attention
}

/// Flush cleared attention flags from the three parallel attention maps
/// in [`AppState`] (`attention_panes`, `attention_details`,
/// `last_attention_notif`) and mark any ntfy backlog entries for the
/// cleared panes as resolved so they don't replay on SSE reconnect.
/// One call per poll tick per function, after the per-pane loop, so we
/// grab each write lock once. No-op if `cleared` is empty.
pub(super) async fn flush_cleared_attention(state: &AppState, cleared: &[String]) {
    if cleared.is_empty() {
        return;
    }
    {
        let mut att = state.attention_panes.write().await;
        for pid in cleared {
            att.remove(pid);
        }
    }
    {
        let mut det = state.attention_details.write().await;
        for pid in cleared {
            det.remove(pid);
        }
    }
    {
        let mut lan = state.last_attention_notif.write().await;
        for pid in cleared {
            lan.remove(pid);
        }
    }
    for pid in cleared {
        super::hook_sink::mark_ntfy_resolved_by_pane(state, pid).await;
    }
}

pub async fn run(state: AppState) {
    let mut remote_last: HashMap<String, Instant> = HashMap::new();
    let mut remote_last_ok: HashMap<String, Instant> = HashMap::new();
    let mut tick: u32 = 0;
    loop {
        let t0 = std::time::Instant::now();

        // Local poll — every tick, unchanged cadence.
        if let Err(e) = poll_once(&state).await {
            tracing::debug!("tmux poll error (local): {e}");
        }

        // Remote polls — discover active remote hosts from pane
        // assignments and fire each on its own cadence. When no pane
        // targets a remote host, the inner loop doesn't execute and
        // we don't pay any SSH cost.
        let remote_hosts = crate::services::remote_hosts::collect_remote_hosts(&state.app);
        for alias in remote_hosts {
            let due = remote_last
                .get(&alias)
                .map(|t| t.elapsed() >= REMOTE_POLL_INTERVAL)
                .unwrap_or(true);
            if due {
                match poll_once_remote(&state, &alias).await {
                    Ok(()) => {
                        remote_last_ok.insert(alias.clone(), Instant::now());
                    }
                    Err(e) => {
                        tracing::debug!(alias = %alias, "tmux poll error (remote): {e}");
                    }
                }
                remote_last.insert(alias, Instant::now());
            }
        }

        // Periodic sweep of stale pane_assignment entries. Gated on
        // tick count so we don't serialize/deserialize the store on
        // every 1 s tick.
        tick = tick.wrapping_add(1);
        if tick % SWEEP_EVERY_N_TICKS == 0 {
            if let Err(e) = sweep_stale_assignments(&state, &remote_last_ok).await {
                tracing::debug!("stale-assignment sweep error: {e}");
            }
        }

        let dt = t0.elapsed();
        if dt.as_millis() > 500 {
            tracing::info!(elapsed_ms = %dt.as_millis(), "slow poll tick");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Garbage-collect `pane_assignments` whose keys no longer correspond to
/// any live pane. Runs every [`SWEEP_EVERY_N_TICKS`] ticks; an entry
/// must be continuously missing for [`STALE_GRACE`] before eviction.
///
/// Remote assignments are only considered when their host has been
/// successfully polled within [`REMOTE_HOST_STALE`] — a Mac that's
/// currently unreachable should not cause its assignments to disappear
/// while the user isn't even aware the link is down.
///
/// Race window: `state.panes` and `pane_assignments` are read a few
/// microseconds apart. A pane that winks out of existence between the
/// two reads looks missing this tick; it gets picked up again and
/// ignored next tick once the `pane_assignments` read sees the next
/// state. The 60 s grace period absorbs single-tick flicker like that.
async fn sweep_stale_assignments(
    state: &AppState,
    remote_last_ok: &HashMap<String, Instant>,
) -> anyhow::Result<()> {
    let live_keys: HashSet<String> = {
        let panes = state.panes.read().await;
        panes.keys().cloned().collect()
    };

    let assignments =
        crate::commands::project_meta::get_pane_assignments_full_sync(&state.app)
            .map_err(|e| anyhow::anyhow!(e))?;

    let now = Instant::now();
    let mut to_evict: Vec<String> = Vec::new();

    {
        let mut missing = MISSING_SINCE.lock().expect("MISSING_SINCE poisoned");
        for key in assignments.keys() {
            // 4-segment assignment key: "host|session|window|pane". The
            // parse_key helper also tolerates a stray legacy 3-segment
            // key if migration hasn't landed yet (promotes to
            // host="local"), so we stay forward/backward safe.
            let Some((host, session, window, pane)) =
                crate::models::pane_assignment::parse_key(key)
            else {
                // Malformed key — drop any grace record and skip.
                missing.remove(key);
                continue;
            };

            let is_remote = host != "local" && !host.is_empty();

            if is_remote {
                let host_recently_ok = remote_last_ok
                    .get(&host)
                    .map(|t| t.elapsed() < REMOTE_HOST_STALE)
                    .unwrap_or(false);
                if !host_recently_ok {
                    // Host offline or never seen: skip eviction consideration,
                    // reset any prior grace timer so a recovered host doesn't
                    // inherit partial credit.
                    missing.remove(key);
                    continue;
                }
            }

            // poll_once gives local panes id = "<session>:<window>.<pane>".
            // poll_once_remote gives remote panes id = "<alias>/<session>:<window>.<pane>".
            let coord = format!("{}:{}.{}", session, window, pane);
            let live_key = if is_remote {
                format!("{}/{}", host, coord)
            } else {
                coord
            };

            if live_keys.contains(&live_key) {
                missing.remove(key);
            } else {
                let since = missing.entry(key.clone()).or_insert(now);
                if now.duration_since(*since) > STALE_GRACE {
                    to_evict.push(key.clone());
                }
            }
        }

        for k in &to_evict {
            missing.remove(k);
        }
    }

    if to_evict.is_empty() {
        return Ok(());
    }

    // RMW the store. Read a fresh snapshot: the pane_assignments key may
    // have been mutated (drag-drop, dropdown change) between the read
    // above and now. Only evict keys that (a) we flagged for eviction
    // AND (b) still look stale at write time — any key that's been
    // overwritten has a fresh value and should be kept.
    let mut current =
        crate::commands::project_meta::get_pane_assignments_full_sync(&state.app)
            .map_err(|e| anyhow::anyhow!(e))?;
    let mut changed = false;
    for k in &to_evict {
        if let Some(prior) = assignments.get(k) {
            if let Some(now_entry) = current.get(k) {
                if now_entry == prior {
                    current.remove(k);
                    tracing::info!(key = %k, "evicted stale pane_assignment");
                    changed = true;
                }
            }
        }
    }
    if changed {
        crate::services::store::save_store(&state.app, "pane_assignments", &current)
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    Ok(())
}

/// Build a lookup from `"<session>|<window>|<pane>"` → configured
/// account for every pane assignment targeting `alias`. Used by
/// [`poll_once_remote`] to synthesize `claude_account` on the DTO
/// since /proc isn't reachable across SSH.
///
/// Returns a map keyed on the 3-segment coord `session|window|pane`
/// (not the full 4-segment assignment key) so the remote pane loop
/// can look up accounts with the tmux-native coord it already parsed
/// out of list-panes output. Only entries whose host matches `alias`
/// are included.
async fn remote_account_map_for_alias(
    state: &AppState,
    alias: &str,
) -> HashMap<String, String> {
    let full = match crate::commands::project_meta::get_pane_assignments_full_sync(&state.app) {
        Ok(m) => m,
        Err(_) => return HashMap::new(),
    };
    let mut out = HashMap::new();
    for (key, assignment) in full {
        if assignment.host != alias {
            continue;
        }
        // Drop the host segment from the 4-segment key so the coord
        // matches what poll_once_remote builds from tmux format fields.
        if let Some((_h, session, window, pane)) =
            crate::models::pane_assignment::parse_key(&key)
        {
            let coord = format!("{}|{}|{}", session, window, pane);
            out.insert(coord, assignment.account);
        }
    }
    out
}

/// Remote-host variant of [`poll_once`]. Stripped down: no /proc
/// account detection (not reachable across SSH), no session binding
/// (would need lsof-on-Mac), no pipe-pane / log rotation (those write
/// to the local filesystem). Responsibilities:
///
/// 1. Fetch list-panes + capture-pane via SSH in one batched call.
/// 2. Upsert PaneRecord entries with `<alias>/` prefixed ids.
/// 3. Synthesize `claude_account` from pane assignments.
/// 4. Derive PaneState / waiting_reason using the same rules as local.
/// 5. Emit PaneStateChanged / PaneOutputChanged / PaneUpdated events.
/// 6. Prune remote panes under this alias that disappeared from tmux.
async fn poll_once_remote(state: &AppState, alias: &str) -> anyhow::Result<()> {
    use crate::services::host_target::HostTarget;
    let host = HostTarget::Remote { alias: alias.to_string() };

    // Same batched script as the local poll. `$(...)` and quoting work
    // identically on the Mac; tmux's format strings are portable.
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
    let combined_out =
        crate::commands::tmux::run_tmux_command_async_on(host.clone(), combined.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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
    let captures_section = combined_out
        .find(list_end)
        .map(|i| combined_out[i + list_end.len()..].to_string())
        .unwrap_or_default();
    let cap_map = parse_capture_batch(&captures_section);

    let account_map = remote_account_map_for_alias(state, alias).await;
    // Same project lookup table the local poll uses. Needed so Mac pane
    // `current_path`s (e.g. `/Users/admin/projects/akamai`) can resolve
    // to the WSL-encoded project name via the basename fallback in
    // `lookup_project` — otherwise the DTO would carry the Mac-side
    // encoding `-Users-admin-projects-akamai`, which `/api/v1/projects`
    // never returns, and the APK / desktop grid can't pair the pane to
    // a project display name.
    let project_cache = ensure_project_cache(state).await;
    let mut seen: HashSet<String> = HashSet::new();
    // Remote session-binding queue — same idea as the local
    // `session_detect_queue`, but resolved via
    // `detect_claude_session_remote` (lsof over SSH) instead of the
    // Linux /proc walk. Collected during the main loop, spawned as a
    // background task after the panes write lock is released.
    let mut remote_session_queue: Vec<(String, String)> = Vec::new();
    // Pane ids whose attention flag was cleared this tick (either because
    // Claude exited or for another reason). Flushed from the three
    // attention maps in one pass at the end — matches the local poll's
    // pattern via `flush_cleared_attention`.
    let mut cleared_attention: Vec<String> = Vec::new();

    // Snapshots used by the state-derivation rule (same source of truth
    // as local poll — phone hook flags + pending approvals).
    let attention_snapshot: HashMap<String, WaitingReason> =
        state.attention_panes.read().await.clone();
    let pending_approval_panes: HashSet<String> = state
        .approvals
        .read()
        .await
        .values()
        .map(|p| p.dto.pane_id.clone())
        .collect();

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
        // pane_pid on the remote host — used to feed `lsof -p <pid>`
        // over SSH for session binding (see `remote_session_queue`).
        let pane_pid = parts[6].to_string();
        // pane_pipe is irrelevant for remote (no pipe-pane on Mac side).
        let pane_uid = parts[8].trim_start_matches('%').to_string();
        let pane_width: u16 = parts[9].parse().unwrap_or(0);
        let start_cmd = parts[10..].join("|");

        // Coord-only key (host stripped) matching what
        // `remote_account_map_for_alias` populates. The pane_assignment
        // store itself uses 4-segment keys ("<host>|<session>|<window>|<pane>")
        // but here we already know the host via `alias`, so we drop it.
        let coord_key = format!("{}|{}|{}", session, window_index, pane_index);
        let synthesized_account = account_map.get(&coord_key).cloned();

        // Remote pane id: `<alias>/<session>:<window>.<pane>` for
        // uniqueness against local panes sharing the same tmux coordinate.
        let id = format!("{}/{}:{}.{}", alias, session, window_index, pane_index);
        let tmux_coord = format!("{}:{}.{}", session, window_index, pane_index);
        seen.insert(id.clone());

        let cap_out = cap_map.get(&tmux_coord).cloned().unwrap_or_default();
        let preview: Vec<String> = cap_out
            .lines()
            .rev()
            .take(5)
            .map(strip_ansi)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let output_hash = sha256(&cap_out);

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
                current_command: current_cmd.clone(),
                current_path: current_path.clone(),
                state: PaneState::Idle,
                waiting_reason: None,
                last_output_preview: preview.clone(),
                project_encoded_name: None,
                project_display_name: None,
                claude_session_id: claude_session_id.clone(),
                claude_account: synthesized_account.clone(),
                host: Some(alias.to_string()),
                claude_effort: None,
                updated_at: now,
                last_activity_at: None,
                warning: None,
                // Remote panes (Mac tmux) are never SSH mirrors —
                // they're the actual destinations. Field stays None.
                mirror_target: None,
            },
            output_hash: [0u8; 32],
            last_output_change: None,
            binding_confidence: binding,
            claude_account: synthesized_account.clone(),
            pane_uid: pane_uid.clone(),
            pane_width,
            pane_pid: pane_pid.clone(),
            first_seen_at: Instant::now(),
        });

        // Field updates — synthesized_account can change if the user
        // flips the account dropdown; host is idempotent.
        rec.dto.session_name = session.clone();
        rec.dto.window_index = window_index;
        rec.dto.window_name = window_name.clone();
        rec.dto.pane_index = pane_index;
        rec.dto.current_command = current_cmd.clone();
        rec.dto.current_path = current_path.clone();
        rec.pane_width = pane_width;
        rec.pane_uid = pane_uid.clone();
        rec.pane_pid = pane_pid.clone();
        if rec.dto.claude_account != synthesized_account {
            rec.dto.claude_account = synthesized_account.clone();
            rec.claude_account = synthesized_account.clone();
        }
        if rec.dto.host.as_deref() != Some(alias) {
            rec.dto.host = Some(alias.to_string());
        }
        // Cross-host project identity. `lookup_project` walks the cache
        // first by exact path-tree match, then falls back to basename
        // — so a Mac pane in `/Users/admin/projects/akamai` resolves to
        // the WSL project whose path ends in `/akamai`. Always prefer
        // this over any previously stamped value: the async
        // lsof-over-SSH session detector below sets `project_encoded_name`
        // to the *Mac-side* encoding (`.claude/projects/-Users-admin-...`),
        // which the frontend can't match against `/api/v1/projects`.
        // Refreshing per tick keeps the DTO correct even if the first
        // binding was Mac-encoded.
        if let Some((enc, display)) = lookup_project(&project_cache, &current_path) {
            if rec.dto.project_encoded_name.as_deref() != Some(enc.as_str()) {
                rec.dto.project_encoded_name = Some(enc.clone());
            }
            if rec.dto.project_display_name.as_deref() != Some(display.as_str()) {
                rec.dto.project_display_name = Some(display.clone());
            }
        }
        if claude_session_id.is_some()
            && rec.binding_confidence != BindingConfidence::Explicit
        {
            rec.dto.claude_session_id = claude_session_id.clone();
            rec.binding_confidence = BindingConfidence::Explicit;
        }

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
            // Effort detection works cross-host: it's text parsing of
            // the terminal capture, no filesystem dependency.
            if let Some(new_effort) = detect_effort(&cap_out) {
                if rec.dto.claude_effort.as_deref() != Some(new_effort) {
                    rec.dto.claude_effort = Some(new_effort.to_string());
                    let _ = state.events.send(EventDto::PaneUpdated {
                        pane: rec.dto.clone(),
                    });
                }
            }
        }

        // State derivation — matches local poll's rules (Idle when Claude
        // exits or output stales; Waiting when an approval/attention flag
        // is set; Running when output moves).
        let claude_alive = crate::commands::tmux::pane_is_claude(&current_cmd, &start_cmd);
        // Close the post-exit cleanup gap. Without this the remote pane
        // record accumulates stale `claude_account`, `claude_session_id`,
        // `claude_effort`, and attention flags after Claude exits on the
        // Mac — symptom: remote panes keep displaying their last-session
        // chip after the process is gone. Shared helper with the local
        // poll so both hosts get identical cleanup semantics.
        let cleared_exit_attention = if !claude_alive {
            apply_claude_exit_cleanup(
                rec,
                &id,
                &pane_pid,
                &attention_snapshot,
                &mut cleared_attention,
                state,
            )
        } else {
            false
        };
        // Queue a session-binding resolve via lsof-over-SSH when Claude
        // is alive and we haven't bound a session id yet. `BindingConfidence`
        // is `Explicit` only when we parsed `--resume <uuid>` from the
        // start command; anything else — the normal `mncld` path — lands
        // here.
        if claude_alive
            && rec.binding_confidence == BindingConfidence::None
            && !pane_pid.is_empty()
        {
            remote_session_queue.push((id.clone(), pane_pid.clone()));
        }
        let has_pending = pending_approval_panes.contains(&id);
        let in_attention = !cleared_exit_attention && attention_snapshot.contains_key(&id);
        let output_stale = rec
            .last_output_change
            .map(|t| t.elapsed() > IDLE_TIMEOUT)
            .unwrap_or(true);

        let desired = if !claude_alive {
            PaneState::Idle
        } else if has_pending || in_attention {
            PaneState::Waiting
        } else if output_stale {
            PaneState::Idle
        } else {
            PaneState::Running
        };

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
                let _ = state.events.send(EventDto::PaneUpdated {
                    pane: rec.dto.clone(),
                });
            }
        }

        if is_new {
            tracing::debug!(pane = %id, alias = %alias, "new remote pane discovered");
        }
    }

    // Prune remote panes under THIS alias that disappeared this tick.
    // Other aliases and local panes are untouched.
    {
        let mut panes = state.panes.write().await;
        let prefix = format!("{}/", alias);
        let gone: Vec<String> = panes
            .keys()
            .filter(|k| k.starts_with(&prefix) && !seen.contains(*k))
            .cloned()
            .collect();
        for id in &gone {
            panes.remove(id);
            let _ = state.events.send(EventDto::PaneRemoved {
                pane_id: id.clone(),
                at: now_ms(),
            });
        }
    }

    // Resolve pending session bindings in the background. Each call
    // is one SSH round-trip running `lsof -p <pid>` on the Mac — cheap
    // with ControlMaster, expensive without. Serialized rather than
    // parallel so we don't fan out N concurrent SSH sessions for a
    // flurry of new Claude panes.
    if !remote_session_queue.is_empty() {
        let bg_state = state.clone();
        let bg_alias = alias.to_string();
        tokio::spawn(async move {
            for (pane_id, shell_pid) in remote_session_queue {
                if let Some((session_id, encoded_project, is_mru)) =
                    detect_claude_session_remote(&bg_alias, &shell_pid).await
                {
                    // Same MRU grace-window guard as the local poller.
                    // See tmux_poller::poll_once's session_detect_queue.
                    const MRU_GRACE: Duration = Duration::from_secs(5);
                    if is_mru {
                        let too_young = {
                            let panes = bg_state.panes.read().await;
                            panes
                                .get(&pane_id)
                                .map(|rec| rec.first_seen_at.elapsed() < MRU_GRACE)
                                .unwrap_or(false)
                        };
                        if too_young {
                            tracing::info!(
                                target: "mpdiag",
                                pane = %pane_id,
                                session = %session_id,
                                "rejected remote MRU binding — pane still in grace window"
                            );
                            continue;
                        }
                    }
                    let updated_dto = {
                        let mut panes = bg_state.panes.write().await;
                        if let Some(rec) = panes.get_mut(&pane_id) {
                            if rec.binding_confidence != BindingConfidence::Explicit {
                                rec.dto.claude_session_id = Some(session_id);
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
                        let _ = bg_state.events.send(EventDto::PaneUpdated { pane });
                    }
                }
            }
        });
    }

    // Flush cleared attention flags — same pattern as the local poll.
    flush_cleared_attention(state, &cleared_attention).await;

    Ok(())
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
    // Active-window detection per session. Multiple panes in the active
    // window all report `#{window_active}` = 1; dedupe by session and
    // remember the first `window_index` we see. Compared to
    // `LAST_ACTIVE_WINDOWS` at the end of this poll to emit
    // `WindowFocusChanged` only when the focus actually shifts.
    let mut active_windows_this_tick: HashMap<String, u32> = HashMap::new();

    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 12 {
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
        let window_active = parts[10] == "1";
        let start_cmd = parts[11..].join("|"); // in case start_command contains pipes

        // Track the active window per session so we can emit
        // WindowFocusChanged at the end of this tick. Multiple panes
        // in the active window all report window_active=1; dedupe by
        // session and pick the first window_index we see.
        if window_active {
            active_windows_this_tick
                .entry(session.clone())
                .or_insert(window_index);
        }
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
        // SSH-mirror panes are local panes whose process is `ssh -t mac
        // tmux attach-session -t <session>` — viewports into a remote
        // tmux server. Their cwd is wherever ssh was invoked from
        // (`/home/andrea` or `/mnt/c/Users/Andrea`), which the
        // path-fallback in `lookup_project` would mistag as the
        // `andrea` project. Skip the lookup entirely; clients use the
        // `mirror_target` field for rendering instead.
        let mirror_target = crate::services::ssh_mirror::parse_mirror_target(&start_cmd);
        let project_match = if mirror_target.is_some() {
            None
        } else {
            lookup_project(&project_map, &cur_path).cloned()
        };
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
                host: Some("local".to_string()),
                claude_effort: None,
                updated_at: now,
                last_activity_at: None,
                warning: None,
                mirror_target: mirror_target.clone(),
            },
            output_hash: [0u8; 32],
            last_output_change: None,
            binding_confidence: binding,
            claude_account: None,
            pane_uid: pane_uid.clone(),
            pane_width,
            pane_pid: pane_pid.clone(),
            first_seen_at: Instant::now(),
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
            rec.dto.claude_effort = None;
            rec.output_hash = [0u8; 32]; // force next hash compare to re-emit
            // Reset the grace window — the record still exists but now
            // hosts a different underlying pane. MRU fallback should be
            // gated off for the first few seconds of this new identity
            // just as it would be for a fresh PaneRecord.
            rec.first_seen_at = Instant::now();
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
        // Refresh per tick — start_command shouldn't change, but if a
        // user kills a mirror pane and reuses the slot for a regular
        // shell, the field must clear.
        rec.dto.mirror_target = mirror_target.clone();
        rec.pane_width = pane_width;
        rec.pane_uid = pane_uid.clone();
        rec.pane_pid = pane_pid.clone();
        if claude_session_id.is_some() && rec.binding_confidence != BindingConfidence::Explicit {
            rec.dto.claude_session_id = claude_session_id.clone();
            rec.binding_confidence = BindingConfidence::Explicit;
        }

        // Compute the desired pipe-pane target based on the NEW session id
        // (post-update) and queue an action if something needs changing.
        let new_session_id = rec.dto.claude_session_id.clone();
        let is_claude = crate::commands::tmux::pane_is_claude(&cur_cmd, &start_cmd);
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

            // Effort detection — sticky cache. Only emit when the detected
            // level differs from what we had; when detection returns None
            // we keep the previous value (the banner scrolls off the
            // 45-line window eventually, but the setting is still accurate).
            if let Some(new_effort) = detect_effort(&cap_out) {
                if rec.dto.claude_effort.as_deref() != Some(new_effort) {
                    rec.dto.claude_effort = Some(new_effort.to_string());
                    let _ = state.events.send(EventDto::PaneUpdated {
                        pane: rec.dto.clone(),
                    });
                    tracing::debug!(pane = %id, effort = %new_effort, "emit PaneUpdated (effort)");
                }
            }
        }

        // Desired PaneState derivation. Single source of truth:
        //   claude_alive  (pane_current/start_command heuristic)
        //   has_pending   (approvals map, snapshot above)
        //   in_attention  (attention_panes, snapshot above)
        //
        // Special case: a pane whose current_command is `ssh` can still
        // be interactively running Claude when the user did
        // `ssh mac -t "cc <project> <account>"` or similar. tmux only
        // sees `ssh` as the foreground process, so the command-name
        // heuristic comes up empty — but the capture output carries
        // Claude's TUI (the word "claude" in banners, prompts, or the
        // rendered /effort tag). Fall back to scanning the last-capture
        // preview when the current command is `ssh`.
        let claude_alive = crate::commands::tmux::pane_is_claude(&cur_cmd, &start_cmd)
            || (cur_cmd.trim().eq_ignore_ascii_case("ssh") && preview_looks_claude(&preview));
        let is_ssh_claude =
            cur_cmd.trim().eq_ignore_ascii_case("ssh") && claude_alive;
        if claude_alive && rec.claude_account.is_none() && !pane_pid.is_empty() && !is_ssh_claude {
            // Skip /proc-based detection for ssh-wrapped panes — the
            // walker never reaches Claude because it runs on the far
            // side of the tunnel. The context-based synth below picks
            // the account up from start_command / capture preview.
            detect_queue.push((id.clone(), pane_pid.clone()));
        }
        if is_ssh_claude && rec.claude_account.is_none() {
            if let Some(acct) = detect_account_from_ssh_context(&preview, &start_cmd) {
                rec.claude_account = Some(acct.clone());
                rec.dto.claude_account = Some(acct);
            }
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
        // When Claude exits, clear per-Claude-lifecycle fields (account,
        // session_id binding, effort) and drop any pending attention flag.
        // See `apply_claude_exit_cleanup` for the full rationale. Shared
        // with the remote poll path so both hosts get identical cleanup.
        let cleared_exit_attention = if !claude_alive {
            apply_claude_exit_cleanup(
                rec,
                &id,
                &pane_pid,
                &attention_snapshot,
                &mut cleared_attention,
                &state,
            )
        } else {
            false
        };
        let has_pending = pending_approval_panes.contains(&id);
        let mut in_attention = !cleared_exit_attention && attention_snapshot.contains_key(&id);
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
            // Pending → session transition: stop the old pipe FIRST so
            // the cat-subprocess flushes and exits before we move the
            // pending file underneath it (otherwise cat keeps writing
            // to a now-unlinked inode and that output is lost). Then
            // migrate the pending bytes into the session log so
            // history stays contiguous across the transition.
            let needs_pending_migration = pipe_active
                && old_session.is_none()
                && matches!(&target, super::pane_log::LogTarget::Session(_));
            if needs_pending_migration {
                if let super::pane_log::LogTarget::Session(sid) = &target {
                    let _ = super::pane_log::disable_pipe_pane(&pane_id).await;
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
            match super::pane_log::atomic_migrate_pipe_pane(&pane_id, &target).await {
                Ok(()) => tracing::info!(
                    pane = %pane_id,
                    target = ?target,
                    "pipe-pane configured (verified writing)"
                ),
                Err(e) => tracing::error!(
                    pane = %pane_id,
                    target = ?target,
                    "atomic pipe migration failed: {e}"
                ),
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

    // Flush cleared attention flags in one pass. See `flush_cleared_attention`.
    flush_cleared_attention(&state, &cleared_attention).await;

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
            // Overlap-prevention grace window: a brand-new pane hasn't
            // had time to open its jsonl in write mode, so detection's
            // MRU fallback is very likely to hand us a sibling pane's
            // active transcript. Reject MRU bindings until the pane has
            // been alive for 5s — by then the primary fd-scan path
            // should have succeeded with the pane's real session id.
            // Non-MRU bindings (write-mode fd found) are authoritative
            // and pass through immediately.
            const MRU_GRACE: Duration = Duration::from_secs(5);
            if is_mru {
                let too_young = {
                    let panes = state.panes.read().await;
                    match panes.get(&pane_id) {
                        Some(rec) => rec.first_seen_at.elapsed() < MRU_GRACE,
                        None => false,
                    }
                };
                if too_young {
                    tracing::info!(
                        target: "mpdiag",
                        pane = %pane_id,
                        session = %session_id,
                        "rejected MRU binding — pane still in grace window"
                    );
                    continue;
                }
            }

            // Migration info captured while holding the lock, acted on
            // after we drop it — avoids awaiting inside the write guard.
            let mut migrate_info: Option<(String, String)> = None; // (pane_uid, session_id)
            let updated_dto = {
                let mut panes = state.panes.write().await;

                // Session-id collision check. Two panes must never bind to
                // the same session_id regardless of detection path (FD or
                // MRU). Prior code only guarded MRU; with that guard, an
                // FD-path race during a `/branch` copy-forward could silently
                // bind a second pane to a session that already belongs to
                // a sibling, producing the tool-use/tool-result positional
                // violations that surface as Anthropic API 400s.
                //
                // Policy: first-writer-wins. The pane losing the race keeps
                // its previous binding (or None) and gets a `warning` field
                // set on its DTO so the UI flags the collision. The winning
                // pane's binding is unchanged.
                let claiming_pane: Option<String> =
                    panes.iter().find_map(|(other_id, other_rec)| {
                        if other_id != &pane_id
                            && other_rec.dto.claude_session_id.as_deref()
                                == Some(&session_id)
                            && other_rec.binding_confidence
                                != BindingConfidence::None
                        {
                            Some(other_id.clone())
                        } else {
                            None
                        }
                    });

                if let Some(other) = claiming_pane {
                    let warning = format!("session_id collides with {}", other);
                    if let Some(rec) = panes.get_mut(&pane_id) {
                        if rec.dto.warning.as_deref() != Some(&warning) {
                            tracing::warn!(
                                pane = %pane_id,
                                session = %session_id,
                                %other,
                                is_mru,
                                "session_id collision — binding skipped"
                            );
                            rec.dto.warning = Some(warning);
                            rec.dto.updated_at = now_ms();
                            Some(rec.dto.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else if let Some(rec) = panes.get_mut(&pane_id) {
                    if rec.binding_confidence != BindingConfidence::Explicit {
                        // Capture the Pending → Session transition now that
                        // the session_id is being bound. If the pane's
                        // session_id was None before this call, we need to
                        // migrate pending/<pane_uid>.log into
                        // sessions/<session_id>.log and re-target pipe-pane.
                        let was_pending = rec.dto.claude_session_id.is_none();
                        let prev_session = rec.dto.claude_session_id.clone();
                        if was_pending && !rec.pane_uid.is_empty() {
                            migrate_info =
                                Some((rec.pane_uid.clone(), session_id.clone()));
                        }
                        // Log the transition. Silent Some(A) → Some(B) rebinds
                        // (where prev_session is Some but != new) are the most
                        // likely cause of "pane A shows pane B's transcript"
                        // in multi-pane setups, because no migration fires to
                        // move the pipe-pane log and the old binding is
                        // dropped without notice.
                        if prev_session.as_deref() != Some(session_id.as_str()) {
                            tracing::info!(
                                target: "mpdiag",
                                pane = %pane_id,
                                prev_session = ?prev_session,
                                new_session = %session_id,
                                was_pending,
                                is_mru,
                                pane_uid = %rec.pane_uid,
                                "session-id rebind"
                            );
                        }
                        rec.dto.claude_session_id = Some(session_id.clone());
                        rec.binding_confidence = BindingConfidence::Heuristic;
                        if rec.dto.project_encoded_name.is_none() {
                            rec.dto.project_encoded_name = Some(encoded_project);
                        }
                        // Clear any stale collision warning now that the
                        // binding is valid again.
                        if rec.dto.warning.is_some() {
                            rec.dto.warning = None;
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
                    // Stop the pending pipe before moving the file so
                    // no in-flight cat writes land on the pending inode
                    // after we unlink it (buffered output would be lost).
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
                    match super::pane_log::atomic_migrate_pipe_pane(&pid_clone, &target).await {
                        Ok(()) => tracing::info!(
                            pane = %pid_clone,
                            target = ?target,
                            "pipe-pane migrated to session (verified writing)"
                        ),
                        Err(e) => tracing::error!(
                            pane = %pid_clone,
                            target = ?target,
                            "atomic pipe migration failed: {e}"
                        ),
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
    //
    // Skip remote panes here — their ids contain a `<alias>/` prefix
    // (e.g. `mac/main:0.0`) and are pruned by their own host-specific
    // poll pass. Letting the local prune touch them would remove every
    // Mac pane on every tick.
    let mut panes = state.panes.write().await;
    let gone: Vec<String> = panes
        .keys()
        .filter(|k| !k.contains('/') && !seen.contains(*k))
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

    // Window-focus transitions. Emit inside the same 1 s poll tick the
    // change was detected in so the desktop's auto-follow reacts
    // effectively immediately, instead of waiting on the frontend's
    // 60 s fallback cycle. Session keys may disappear from a tick
    // (entire session killed) — those show up as SessionEnded above;
    // we leave LAST_ACTIVE_WINDOWS entries stale because the session
    // won't come back under the same name without a user action that
    // would produce its own fresh window_active=1 line.
    {
        let mut last = LAST_ACTIVE_WINDOWS.lock().unwrap();
        for (session, win) in active_windows_this_tick.drain() {
            let key = format!("local|{}", session);
            if last.get(&key).copied() == Some(win) {
                continue;
            }
            last.insert(key, win);
            let _ = state.events.send(EventDto::WindowFocusChanged {
                host: "local".to_string(),
                session_name: session,
                window_index: win,
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

/// Scan a capture buffer for Claude Code's current effort level.
///
/// Two signals, both emitted by Claude Code 2.1+:
/// - Session banner: `"with max effort · Claude Max"` — the level precedes
///   the word "effort". Written once at the top of scrollback on session
///   start; stays visible until the banner scrolls out.
/// - Echoed slash command: `"/effort max"` — appears in the pane whenever
///   the user (or the mobile EffortChip setter) runs the slash.
///
/// Scan is bottom-up and returns on the first hit, so when both signals
/// appear in the buffer the more recent one wins. Returns `None` when
/// neither is present — callers should keep any previously-cached value.
fn detect_effort(capture: &str) -> Option<&'static str> {
    for raw_line in capture.lines().rev() {
        let line = strip_ansi(raw_line).to_ascii_lowercase();
        // Keep `/` and `:` glued onto their tokens so "/effort" and "effort:"
        // survive as single tokens. Everything else alphanumeric-adjacent is
        // a delimiter. ASCII-only is fine: the signals are all Latin.
        let tokens: Vec<&str> = line
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != ':')
            .filter(|s| !s.is_empty())
            .collect();
        for w in tokens.windows(2) {
            // Banner: "<level> effort"
            if w[1] == "effort" {
                if let Some(lvl) = effort_level(w[0]) {
                    return Some(lvl);
                }
            }
            // Slash echo: "/effort <level>"
            if w[0] == "/effort" {
                if let Some(lvl) = effort_level(w[1]) {
                    return Some(lvl);
                }
            }
        }
    }
    None
}

fn effort_level(s: &str) -> Option<&'static str> {
    match s {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "max" => Some("max"),
        _ => None,
    }
}

/// Extract the uuid from `claude --resume <uuid>` or `claude -r <uuid>`
/// in a pane's start command. Returns None if not a resume invocation.
///
/// When `--fork-session` is present, the uuid after `-r`/`--resume` names
/// the *source* (frozen) session — the running Claude is writing to a
/// brand-new uuid it picks at startup. Return None so the MRU fallback
/// (`find_claude_session`) picks up the new JSONL on the next poll tick,
/// instead of locking the pane to the frozen original.
fn parse_resume_uuid(cmd: &str) -> Option<String> {
    let mut parts = cmd.split_whitespace();
    let _ = parts.next()?; // `claude`
    let mut expect_uuid = false;
    let mut resume_uuid: Option<String> = None;
    let mut forked = false;
    for part in parts {
        if expect_uuid {
            if part.len() == 36 && part.matches('-').count() == 4 {
                resume_uuid = Some(part.to_string());
            }
            expect_uuid = false;
        }
        if part == "--resume" || part == "-r" {
            expect_uuid = true;
        }
        if part == "--fork-session" {
            forked = true;
        }
    }
    if forked { None } else { resume_uuid }
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
/// Remote (Mac) port of [`detect_claude_session`]. macOS has no `/proc`
/// so the Linux strategy (walking `/proc/<pid>/fd/` + reading fdinfo
/// flags) is replaced with `lsof -p <pid>`, which lists open file
/// descriptors with an access-mode letter in column 4 (`w` = write,
/// `u` = read+write). Same return convention: `Some((session_id,
/// encoded_project, is_mru))` on hit, `None` on miss.
///
/// Executed via SSH through `run_tmux_command_async_on(Remote)`, so
/// the pgrep / ps / lsof calls resolve against the Mac's real process
/// tree. Config dir fallback iterates `.claude` / `.claude-b` /
/// `.claude-c`.
pub(super) async fn detect_claude_session_remote(
    alias: &str,
    shell_pid: &str,
) -> Option<(String, String, bool)> {
    use crate::services::host_target::HostTarget;

    if shell_pid.is_empty() {
        return None;
    }
    // Single-quote escaping handled by ssh_shell_quote one layer up —
    // the script itself is a plain bash body with no ssh-side escapes.
    let script = format!(
        r#"
walk() {{
  local pid=$1 depth=$2
  [ "$depth" -gt 5 ] && return 1
  local comm
  comm=$(ps -o comm= -p "$pid" 2>/dev/null | tr -d ' ')
  case "$comm" in
    */claude|claude|mcld|*/mcld|mncld|*/mncld|cli-mncld-*|*/cli-mncld-*)
      echo "$pid"
      return 0
      ;;
  esac
  # Claude on macOS runs as a node process. Inspect full args.
  if [ "$(basename "${{comm:-}}")" = "node" ]; then
    if ps -o args= -p "$pid" 2>/dev/null | grep -q claude; then
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
if [ -z "$claude_pid" ]; then
  echo "REASON:no_claude_in_tree"
  exit 0
fi

# First-choice: lsof-based open-file scan. Default lsof columns are:
#   COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
# FD column ends with `w` or `u` for write-capable handles. Claude
# writes its active JSONL append-mode, which surfaces as `<num>w`
# in the FD column. Read-only transients (parent session resolve
# during a fork resume) are filtered out — matches the Linux fdinfo
# flags & 3 check.
hit=$(lsof -p "$claude_pid" 2>/dev/null \
  | awk '$NF ~ /\.jsonl$/ && $4 ~ /[wu]$/ {{ print $NF; exit }}')
if [ -n "$hit" ]; then
  echo "$hit"
  exit 0
fi

# Fallback: lsof cwd → MRU .jsonl in the matching project dir.
cwd=$(lsof -p "$claude_pid" -a -d cwd 2>/dev/null \
  | awk 'NR > 1 {{ print $NF; exit }}')
if [ -z "$cwd" ]; then
  echo "REASON:no_cwd_for_claude_pid"
  exit 0
fi
encoded=$(echo "$cwd" | tr '/. ' '-')
for cfg in ".claude" ".claude-b" ".claude-c"; do
  proj_dir="$HOME/$cfg/projects/$encoded"
  [ -d "$proj_dir" ] || continue
  mru=$(ls -t "$proj_dir"/*.jsonl 2>/dev/null | head -1)
  if [ -n "$mru" ]; then
    echo "MRU:$mru"
    exit 0
  fi
done
echo "REASON:no_write_fd_no_mru_encoded_${{encoded}}"
exit 0
"#,
        pid = shell_pid
    );
    let out = crate::commands::tmux::run_tmux_command_async_on(
        HostTarget::Remote { alias: alias.to_string() },
        script,
    )
    .await
    .ok()?;
    let first = out.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    if let Some(reason) = first.strip_prefix("REASON:") {
        tracing::info!(
            target: "mpdiag",
            alias = %alias,
            pid = %shell_pid,
            reason = %reason,
            "detect_claude_session_remote returned None"
        );
        return None;
    }
    let (path, is_mru) = if let Some(rest) = first.strip_prefix("MRU:") {
        (rest, true)
    } else {
        (first, false)
    };
    let p = std::path::Path::new(path);
    let session_id = p.file_stem()?.to_str()?.to_string();
    let encoded_project = p.parent()?.file_name()?.to_str()?.to_string();
    Some((session_id, encoded_project, is_mru))
}

pub(super) async fn detect_claude_session(shell_pid: &str) -> Option<(String, String, bool)> {
    if shell_pid.is_empty() {
        return None;
    }
    // The shell script emits exactly one of:
    //   <abs-path>           — primary write-mode fd found
    //   MRU:<abs-path>       — mru-fallback (caller treats as low-confidence)
    //   REASON:<short-tag>   — detection failed; tag surfaced so Rust/log can
    //                          distinguish "no claude pid in process tree"
    //                          from "walked tree but no jsonl fd" etc.
    //
    // Config-dir list covers all three accounts currently in use:
    //   .claude   → andrea (primary, default)
    //   .claude-b → bravura
    //   .claude-c → sully
    // Adding a new account means a line here AND a line in the MRU fallback
    // AND a row in companion/accounts.rs::ACCOUNTS.
    //
    // Walk's case statement matches every wrapper-family Andrea has used
    // (cld/cld2/cld3 aren't binaries themselves — they're bash functions
    // that exec into claude/ncld/mncld, so the subprocess side is what the
    // walker sees). mncld is the patched-binary wrapper that mirrors ncld
    // for .claude-c sessions; before this patch the walker missed it and
    // fell through to the node-cmdline grep, which worked only when
    // /proc/$pid/cmdline was already populated (race during exec).
    let script = format!(
        r#"
walk() {{
  local pid=$1 depth=$2
  [ "$depth" -gt 5 ] && return 1
  [ -d "/proc/$pid" ] || return 1
  local comm
  comm=$(cat "/proc/$pid/comm" 2>/dev/null)
  # comm is truncated to 15 chars by the kernel (TASK_COMM_LEN), so
  # `cli-ncld-114.bin` appears as `cli-ncld-114.bi`. Match a prefix.
  case "$comm" in
    claude|cli-ncld-*|cli-mncld-*|ncld|ncld2|ncld3|mncld|mncld2|mncld3)
      echo "$pid"
      return 0
      ;;
  esac
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
if [ -z "$claude_pid" ]; then
  echo "REASON:no_claude_in_tree"
  exit 0
fi

# First-choice signal: catch a JSONL file descriptor Claude has open for
# WRITING while it's active. The write-mode filter is load-bearing — when
# Claude resumes a session whose JSONL is `forkedFrom` an ancestor, it
# briefly opens the parent JSONL in read-only mode to resolve fork history.
# Without the mode check we'd bind the pane to the parent session name,
# which is how two panes could end up appearing to claim the same session
# id even though only one is actually writing it. The fdinfo `flags:` line
# encodes POSIX open flags in octal — low two bits: 0=RDONLY, 1=WRONLY,
# 2=RDWR. Claude's append-to-JSONL path uses O_WRONLY|O_APPEND (so flags &
# 3 == 1). Read-only transients have flags & 3 == 0 and must be skipped.
for i in $(seq 1 15); do
  for fd in /proc/$claude_pid/fd/*; do
    target=$(readlink "$fd" 2>/dev/null) || continue
    case "$target" in
      "$HOME/.claude/projects/"*/*.jsonl|"$HOME/.claude-b/projects/"*/*.jsonl|"$HOME/.claude-c/projects/"*/*.jsonl) ;;
      *) continue ;;
    esac
    fdnum=$(basename "$fd")
    flags=$(awk '/^flags:/ {{print $2; exit}}' "/proc/$claude_pid/fdinfo/$fdnum" 2>/dev/null)
    [ -z "$flags" ] && continue
    # Octal flags: bitwise AND with 3 (low two bits) picks RDONLY/WRONLY/RDWR.
    mode=$(( 0$flags & 3 ))
    if [ "$mode" = "1" ] || [ "$mode" = "2" ]; then
      echo "$target"
      exit 0
    fi
  done
  read -t 0.1 </dev/null 2>/dev/null || true
done

# Fallback: cwd → MRU in project dir. Caller should treat this as
# low-confidence and skip binding if a sibling pane already claims
# this session_id. Prefix `MRU:` flags it as the weak path.
cwd=$(readlink "/proc/$claude_pid/cwd" 2>/dev/null)
if [ -z "$cwd" ]; then
  echo "REASON:no_cwd_for_claude_pid"
  exit 0
fi
encoded=$(echo "$cwd" | tr '/. ' '-')
for cfg in ".claude" ".claude-b" ".claude-c"; do
  proj_dir="$HOME/$cfg/projects/$encoded"
  [ -d "$proj_dir" ] || continue
  mru=$(ls -t "$proj_dir"/*.jsonl 2>/dev/null | head -1)
  if [ -n "$mru" ]; then
    echo "MRU:$mru"
    exit 0
  fi
done
echo "REASON:no_write_fd_no_mru_encoded_${{encoded}}"
exit 0
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
    // Surface the specific failure reason into mpdiag so stuck panes can
    // be diagnosed from logs. One line per failure per tick is fine — the
    // caller rate-limits by only queueing when binding_confidence is None.
    if let Some(reason) = first.strip_prefix("REASON:") {
        tracing::info!(
            target: "mpdiag",
            pid = %shell_pid,
            reason = %reason,
            "detect_claude_session returned None"
        );
        return None;
    }
    let (path, is_mru) = if let Some(rest) = first.strip_prefix("MRU:") {
        (rest, true)
    } else {
        (first, false)
    };
    // Path: /home/<user>/.claude[-b|-c]/projects/<encoded_project>/<session_id>.jsonl
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

    #[test]
    fn contains_word_boundaries() {
        assert!(contains_word("cld3 something", "cld3"));
        assert!(contains_word("prefix cld3", "cld3"));
        assert!(contains_word("cld3", "cld3"));
        assert!(!contains_word("encld3ed", "cld3"));
        // "cld" must NOT match inside "ncld" or "mncld" or "cli-ncld-118.bin"
        assert!(!contains_word("ncld file", "cld"));
        assert!(!contains_word("mncld", "cld"));
        assert!(!contains_word("cli-mncld-118.bin", "cld"));
        // "cld" CAN stand alone.
        assert!(contains_word("type cld then enter", "cld"));
    }

    #[test]
    fn detect_account_ssh_config_dir_sully() {
        // `ssh mac -t 'env CLAUDE_CONFIG_DIR=~/.claude-c mncld'`
        let start = r#"ssh mac -t env CLAUDE_CONFIG_DIR=/Users/admin/.claude-c mncld"#;
        assert_eq!(
            detect_account_from_ssh_context(&[], start),
            Some("sully".to_string())
        );
    }

    #[test]
    fn detect_account_ssh_config_dir_bravura() {
        let start = r#"ssh mac -t env CLAUDE_CONFIG_DIR=$HOME/.claude-b mncld"#;
        assert_eq!(
            detect_account_from_ssh_context(&[], start),
            Some("bravura".to_string())
        );
    }

    #[test]
    fn detect_account_ssh_wrapper_suffix_3() {
        let start = "ssh mac -t cld3";
        assert_eq!(
            detect_account_from_ssh_context(&[], start),
            Some("sully".to_string())
        );
    }

    #[test]
    fn detect_account_ssh_wrapper_suffix_2() {
        let start = "ssh mac -t ncld2";
        assert_eq!(
            detect_account_from_ssh_context(&[], start),
            Some("bravura".to_string())
        );
    }

    #[test]
    fn detect_account_ssh_cc_with_account_arg() {
        // Mac's `cc <project> <account>` launcher.
        let start = "ssh mac -t cc akamai-v3-bestbuy sully";
        assert_eq!(
            detect_account_from_ssh_context(&[], start),
            Some("sully".to_string())
        );
    }

    #[test]
    fn detect_account_ssh_bare_cc_is_andrea() {
        // Bare `cc <project>` defaults to andrea.
        let start = "ssh mac -t cc akamai-v3-bestbuy";
        assert_eq!(
            detect_account_from_ssh_context(&[], start),
            Some("andrea".to_string())
        );
    }

    #[test]
    fn detect_account_from_preview_only() {
        // start_command is a resurrect wrapper — no Claude hint.
        // Preview carries the banner line.
        let start = r#"cat '/home/andrea/.local/share/tmux/resurrect/...'"#;
        let preview = vec![
            "✳ Claude Code v2.1.118".to_string(),
            "  Opus 4.7 with max effort · Claude Max".to_string(),
        ];
        assert_eq!(
            detect_account_from_ssh_context(&preview, start),
            Some("andrea".to_string())
        );
    }

    #[test]
    fn detect_account_no_claude_markers_at_all() {
        let start = "ssh somewhere-else-completely";
        assert_eq!(detect_account_from_ssh_context(&[], start), None);
    }

    #[test]
    fn test_detect_effort_banner() {
        // Claude Code 2.1+ banner — level precedes "effort".
        let cap = "Claude Code v2.1.111\n  Opus 4.7 (1M context) with max effort · Claude Max\n  ~/pane-management\n";
        assert_eq!(detect_effort(cap), Some("max"));

        let cap = "Opus 4.7 with high effort · Claude Pro";
        assert_eq!(detect_effort(cap), Some("high"));

        let cap = "Sonnet 4.6 with medium effort · Claude Max";
        assert_eq!(detect_effort(cap), Some("medium"));

        let cap = "Haiku with low effort · Claude";
        assert_eq!(detect_effort(cap), Some("low"));
    }

    #[test]
    fn test_detect_effort_slash_echo() {
        // User ran /effort via the chip or typed it manually.
        let cap = "some earlier output\n> /effort high\n";
        assert_eq!(detect_effort(cap), Some("high"));
    }

    #[test]
    fn test_detect_effort_prefers_most_recent() {
        // Banner up top, user later ran /effort low → low should win.
        let cap = "Opus 4.7 (1M context) with max effort · Claude Max\nsome work\n/effort low\n";
        assert_eq!(detect_effort(cap), Some("low"));
    }

    #[test]
    fn test_detect_effort_survives_ansi() {
        // Typical ANSI-decorated banner line — escape codes get stripped.
        let cap = "\x1b[38;2;200;200;200mOpus 4.7 (1M context) with \x1b[1mmax effort\x1b[0m · Claude Max\x1b[0m";
        assert_eq!(detect_effort(cap), Some("max"));
    }

    #[test]
    fn test_detect_effort_no_false_positive_on_claude_max() {
        // "Claude Max" at the end of the banner is the Anthropic plan name,
        // not an effort setting. The level must immediately precede "effort".
        let cap = "Opus 4.7 (1M context) · Claude Max";
        assert_eq!(detect_effort(cap), None);
    }

    #[test]
    fn test_detect_effort_none_when_absent() {
        assert_eq!(detect_effort(""), None);
        assert_eq!(detect_effort("bash prompt\n$ ls\n"), None);
        // "max" appears but not paired with "effort".
        assert_eq!(detect_effort("max context window\n"), None);
    }
}
