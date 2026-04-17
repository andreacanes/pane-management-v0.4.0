//! Per-pane raw terminal log capture via `tmux pipe-pane` + VT replay.
//!
//! Claude Code's TUI sends `\x1b[3J` (erase scrollback) during its redraw
//! cycle, wiping tmux's `history-limit` buffer to ~200-400 lines regardless
//! of the configured limit. To retain full conversation history we enable
//! `tmux pipe-pane` on every Claude pane — pipe-pane captures the raw byte
//! stream **before** tmux interprets any escape sequences, so content that
//! would have been wiped from tmux's scrollback is persisted to disk.
//!
//! ## Path scheme
//!
//! Logs are keyed by **Claude session UUID** so the same log follows the
//! session even if the tmux pane gets renumbered (`renumber-windows on`)
//! or closed and reopened. When a pane has no bound session yet, its
//! output goes to a **pending** log keyed by tmux's stable pane id
//! (`#{pane_id}`, e.g. `%23`). When the session is detected, the pending
//! log is appended to the session log and the pending file is removed.
//!
//! ```text
//! %LOCALAPPDATA%\pane-management\pane-logs\
//!   sessions\<claude_session_uuid>.log
//!   pending\<tmux_pane_uid>.log
//! ```
//!
//! WSL-side paths use the `/mnt/c/...` DrvFs mount so `cat >> <path>` in
//! pipe-pane shell commands writes to the same files.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Max size before rotation triggers.
pub const MAX_LOG_SIZE: u64 = 20 * 1024 * 1024; // 20MB

/// After rotation, keep this many tail bytes.
pub const TRUNCATE_TO: u64 = 10 * 1024 * 1024; // 10MB

/// Default tail size returned by `/capture` when a log file exists.
/// In cell-diff mode (Claude 2.1.110+) avt needs to see the log from
/// Claude's first full-draw frame onward to build correct cell state;
/// starting mid-conversation leaves cells blank that Claude's diffs
/// never rewrote, producing sparse/garbled output at the top of the
/// rendered viewport. Match the log rotation cap so we always feed
/// avt the full retained history.
pub const DEFAULT_TAIL_BYTES: u64 = MAX_LOG_SIZE;

/// Fallback virtual-terminal width used when the caller doesn't know the
/// pane's actual columns (brand-new pane, lookup miss). Any real value
/// queried from `#{pane_width}` and passed to `replay_to_lines` takes
/// precedence. Do not lean on this — a hardcoded width that mismatches
/// Claude's TUI columns is exactly what produced the 2026-04-16
/// character-level mixing (cells at cols > real-width holding stale
/// writes, auto-wrap landing content on the wrong logical row).
pub const REPLAY_COLS_FALLBACK: usize = 200;
pub const REPLAY_ROWS: usize = 80;

/// Scrollback cap for the virtual terminal.
pub const REPLAY_SCROLLBACK: usize = 20_000;

/// Windows path to the log directory. `fs::*` operations use this.
const LOG_ROOT_WIN: &str = r"C:\Users\Andrea\AppData\Local\pane-management\pane-logs";

/// WSL path to the same directory (via the /mnt/c DrvFs mount).
const LOG_ROOT_WSL: &str = "/mnt/c/Users/Andrea/AppData/Local/pane-management/pane-logs";

const SESSIONS_SUBDIR: &str = "sessions";
const PENDING_SUBDIR: &str = "pending";

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Sanitize a UUID or pane-uid for filesystem safety. Claude session UUIDs
/// are `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (hex + dashes, safe). tmux
/// pane uids are `%N` — we strip the `%`. Defensive strip of anything else
/// weird so we can't ever write outside the log dir.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn log_path_for_session(session_id: &str) -> PathBuf {
    PathBuf::from(LOG_ROOT_WIN)
        .join(SESSIONS_SUBDIR)
        .join(format!("{}.log", sanitize_id(session_id)))
}

pub fn log_path_for_pending(pane_uid: &str) -> PathBuf {
    PathBuf::from(LOG_ROOT_WIN)
        .join(PENDING_SUBDIR)
        .join(format!("{}.log", sanitize_id(pane_uid)))
}

fn wsl_session_path(session_id: &str) -> String {
    format!(
        "{}/{}/{}.log",
        LOG_ROOT_WSL,
        SESSIONS_SUBDIR,
        sanitize_id(session_id)
    )
}

fn wsl_pending_path(pane_uid: &str) -> String {
    format!(
        "{}/{}/{}.log",
        LOG_ROOT_WSL,
        PENDING_SUBDIR,
        sanitize_id(pane_uid)
    )
}

/// Create the log directory tree (root + sessions/ + pending/). Idempotent.
pub fn ensure_log_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(PathBuf::from(LOG_ROOT_WIN).join(SESSIONS_SUBDIR))?;
    std::fs::create_dir_all(PathBuf::from(LOG_ROOT_WIN).join(PENDING_SUBDIR))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// VT replay + ANSI emission
// ---------------------------------------------------------------------------

/// Replay a raw terminal byte stream through a virtual VT100 emulator and
/// return the rendered grid as lines with ANSI SGR codes embedded. The
/// Android AnsiParser renders these codes as colors/bold/italic.
///
/// `width` must be the tmux pane's real columns (from `#{pane_width}`) so
/// avt's auto-wrap boundary matches Claude's TUI. A mismatched width is
/// what produced the 2026-04-16 character-level mixing: Claude would wrap
/// at col W and start the overflow on the next logical row; avt at a wider
/// grid would NOT wrap there and would stamp the overflow at cols W..end
/// of the current row, stomping cells from the following row on the next
/// write. Pass `REPLAY_COLS_FALLBACK` only when you really have no width.
///
/// `avt` silently ignores ESC[3J (erase-scrollback), which is what we want
/// — it's what prevented this from working via plain `tmux capture-pane`.
pub fn replay_to_lines(bytes: &[u8], width: usize) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    // Strip alternate-screen-buffer toggles BEFORE feeding to avt.
    // Claude Code uses `\e[?1049h` to switch to a fresh buffer with no
    // scrollback (vim/less/top do the same). avt honors this and routes
    // all subsequent writes to the alt-screen, where they get overwritten
    // in place with no history retained — collapsing 2MB of log into ~40
    // visible rows of partially-corrupted in-progress redraws.
    //
    // tmux on desktop has `setw -g alternate-screen off` and ignores these
    // sequences. We do the equivalent by stripping the toggle bytes so all
    // of Claude's output lands on the main screen and scrolls naturally
    // into avt's scrollback buffer. Also strip the older 1047/47 variants.
    for seq in &["\x1b[?1049h", "\x1b[?1049l", "\x1b[?1047h", "\x1b[?1047l", "\x1b[?47h", "\x1b[?47l"] {
        text = text.replace(seq, "");
    }

    // Claude Code 2.1.110 rewrote the TUI renderer to emit **cell-diff**
    // updates on the main screen: it writes only cells whose content has
    // changed since the previous frame, with no periodic `\e[2J` clears.
    // (See the 2.1.110 changelog "/tui fullscreen" entry: full-redraw is
    // opt-in, default is diff. 2.1.111 added Ctrl+L as a manual force-
    // redraw escape hatch, confirming no automatic full clear ever fires.)
    //
    // The diff stream is self-consistent from Claude's first full draw
    // onward — each subsequent frame writes only the cells that differ
    // from the previous, and those differences land on a terminal whose
    // cell state already matches Claude's model. Our replay works the
    // same way: avt starts blank, the first frame in the tail writes its
    // cells, and subsequent frames' diffs accumulate on top. The only
    // precondition is that the tail is aligned to a frame boundary; a
    // mid-frame start leaves avt with a partial escape sequence and
    // corrupts the first render. `read_tail_bytes` snaps to `\e[H` for
    // exactly this reason.

    let effective_width = if width == 0 { REPLAY_COLS_FALLBACK } else { width };
    let mut vt = avt::Vt::builder()
        .size(effective_width, REPLAY_ROWS)
        .scrollback_limit(REPLAY_SCROLLBACK)
        .build();
    let _ = vt.feed_str(&text);

    // No continuation-row merge. With avt sized to the real pane width,
    // Claude's own line breaks are faithful — Claude never writes past
    // width without emitting its own newline — so there is nothing to
    // rejoin. An earlier implementation glued `col == width-1` rows to
    // the next row; at the wrong width that concatenated unrelated rows.
    vt.lines().map(line_to_ansi).collect()
}

/// Convert an `avt::Line` to a `String` with ANSI SGR escape codes embedded
/// so the client's ANSI parser can render colors and text attributes.
fn line_to_ansi(line: &avt::Line) -> String {
    let cells = line.cells();
    let last_meaningful = cells.iter().rposition(|c| {
        let pen = c.pen();
        !(c.char() == ' ' && pen_is_default(pen))
    });
    let end = match last_meaningful {
        Some(idx) => idx + 1,
        None => return String::new(),
    };

    // Strip excessive leading whitespace. Claude's TUI centers splash/header
    // content with 30-50 columns of padding so it sits mid-screen on a 189-
    // column terminal. On a narrow phone that padding wraps awkwardly,
    // pushing text into disjoint visual lines. Keep small indents (<= 16
    // cols) so code blocks and tree structures preserve their structure.
    const MAX_PRESERVED_INDENT: usize = 16;
    let first_meaningful = cells
        .iter()
        .position(|c| {
            let pen = c.pen();
            !(c.char() == ' ' && pen_is_default(pen))
        })
        .unwrap_or(end);
    let start = if first_meaningful > MAX_PRESERVED_INDENT {
        first_meaningful
    } else {
        0
    };

    let mut out = String::with_capacity(end.saturating_sub(start) + 16);
    let mut prev_pen: Option<avt::Pen> = None;
    let mut had_styled = false;

    for cell in &cells[start..end] {
        let curr = *cell.pen();
        if Some(curr) != prev_pen {
            if !pen_is_default(&curr) || had_styled {
                emit_sgr(&mut out, &curr);
                had_styled = true;
            }
            prev_pen = Some(curr);
        }
        let ch = cell.char();
        if ch != '\0' && cell.width() > 0 {
            out.push(ch);
        }
    }
    if had_styled {
        out.push_str("\x1b[0m");
    }
    out
}

fn pen_is_default(pen: &avt::Pen) -> bool {
    pen.foreground().is_none()
        && pen.background().is_none()
        && !pen.is_bold()
        && !pen.is_faint()
        && !pen.is_italic()
        && !pen.is_underline()
        && !pen.is_strikethrough()
        && !pen.is_blink()
        && !pen.is_inverse()
}

fn emit_sgr(out: &mut String, pen: &avt::Pen) {
    // Always reset first, then apply attributes in order. Simpler than
    // diff-emitting and correct — the client parser handles it fine.
    out.push_str("\x1b[0m");
    if pen.is_bold() {
        out.push_str("\x1b[1m");
    }
    if pen.is_faint() {
        out.push_str("\x1b[2m");
    }
    if pen.is_italic() {
        out.push_str("\x1b[3m");
    }
    if pen.is_underline() {
        out.push_str("\x1b[4m");
    }
    if pen.is_blink() {
        out.push_str("\x1b[5m");
    }
    if pen.is_inverse() {
        out.push_str("\x1b[7m");
    }
    if pen.is_strikethrough() {
        out.push_str("\x1b[9m");
    }
    if let Some(fg) = pen.foreground() {
        emit_color(out, fg, true);
    }
    if let Some(bg) = pen.background() {
        emit_color(out, bg, false);
    }
}

fn emit_color(out: &mut String, color: avt::Color, is_fg: bool) {
    let prefix = if is_fg { 38 } else { 48 };
    match color {
        avt::Color::Indexed(n) => {
            let _ = write!(out, "\x1b[{};5;{}m", prefix, n);
        }
        avt::Color::RGB(rgb) => {
            let _ = write!(
                out,
                "\x1b[{};2;{};{};{}m",
                prefix, rgb.r, rgb.g, rgb.b
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tail read + rotation
// ---------------------------------------------------------------------------

/// Read the last `max_bytes` bytes from `path`, snapping to the next `\n`
/// boundary so we don't return a partial line at the head. Returns an empty
/// Vec when the file is missing or empty.
pub fn read_tail_bytes(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let total = file.metadata()?.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let start = total.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(max_bytes.min(total) as usize);
    file.read_to_end(&mut buf)?;
    // Snap the tail start to a Claude frame boundary (`\e[H`), not a `\n`.
    // Cell-diff mode rarely emits literal `\n` inside a redraw, so an `\n`
    // snap lands mid-frame and avt processes a partial escape sequence —
    // cursor state is indeterminate, cells at the top of the render drift.
    // `\e[H` is Claude's "begin redraw" marker; snapping there aligns avt
    // with a natural reset point.
    if start > 0 {
        if let Some(pos) = find_subsequence(&buf, b"\x1b[H") {
            buf.drain(..pos);
        } else if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            buf.drain(..=nl);
        }
    }
    Ok(buf)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Rotate the log file if it exceeds MAX_LOG_SIZE. Keeps the last TRUNCATE_TO
/// bytes. Returns `true` if rotation happened.
pub fn rotate_if_needed(path: &Path) -> std::io::Result<bool> {
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if size <= MAX_LOG_SIZE {
        return Ok(false);
    }
    let tail = read_tail_bytes(path, TRUNCATE_TO)?;
    std::fs::write(path, tail)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// pipe-pane management
// ---------------------------------------------------------------------------

/// Where a pane's output should be logged. Determined by whether the pane
/// has a known Claude session id yet.
#[derive(Debug, Clone)]
pub enum LogTarget {
    Session(String),
    Pending(String),
}

impl LogTarget {
    fn win_path(&self) -> PathBuf {
        match self {
            LogTarget::Session(sid) => log_path_for_session(sid),
            LogTarget::Pending(uid) => log_path_for_pending(uid),
        }
    }

    fn wsl_path(&self) -> String {
        match self {
            LogTarget::Session(sid) => wsl_session_path(sid),
            LogTarget::Pending(uid) => wsl_pending_path(uid),
        }
    }
}

/// Enable `tmux pipe-pane` on a pane, seeding an empty log with the pane's
/// current scrollback so historical content isn't lost. If the target log
/// file already has content (re-attach after rotation or tmux restart),
/// the seed is skipped to avoid duplicating the content.
pub async fn enable_pipe_pane(pane_id: &str, target: &LogTarget) -> Result<(), String> {
    let wsl_path = target.wsl_path();
    let wsl_dir = match target {
        LogTarget::Session(_) => format!("{}/{}", LOG_ROOT_WSL, SESSIONS_SUBDIR),
        LogTarget::Pending(_) => format!("{}/{}", LOG_ROOT_WSL, PENDING_SUBDIR),
    };
    let script = format!(
        "mkdir -p '{dir}' 2>/dev/null; \
         [ ! -s '{path}' ] && tmux capture-pane -p -e -t {pane_id} -S - >> '{path}' 2>/dev/null; \
         tmux pipe-pane -t {pane_id} 'cat >> \"{path}\"' 2>/dev/null",
        dir = wsl_dir,
        path = wsl_path,
        pane_id = pane_id,
    );
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map(|_| ())
}

/// Disable pipe-pane for the given pane. Used before rotation or before
/// switching the pipe to a new target (e.g. session transition).
pub async fn disable_pipe_pane(pane_id: &str) -> Result<(), String> {
    let script = format!("tmux pipe-pane -t {} 2>/dev/null", pane_id);
    crate::commands::tmux::run_tmux_command_async(script)
        .await
        .map(|_| ())
}

/// Append a pending log into the session log, then delete the pending file.
/// Called when a pane's claude_session_id transitions from None to Some.
/// Preserves ordering: pending content appears before any session content
/// written after this call.
pub async fn migrate_pending_to_session(
    pane_uid: &str,
    session_id: &str,
) -> Result<(), String> {
    let pending = log_path_for_pending(pane_uid);
    let session = log_path_for_session(session_id);
    if !pending.exists() {
        return Ok(());
    }
    // Use tokio::task::spawn_blocking because this is fs IO off the poll
    // loop's hot path.
    let pending_clone = pending.clone();
    let session_clone = session.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if let Some(parent) = session_clone.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = match std::fs::read(&pending_clone) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut session_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session_clone)?;
        use std::io::Write;
        session_file.write_all(&content)?;
        session_file.sync_all()?;
        let _ = std::fs::remove_file(&pending_clone);
        Ok(())
    })
    .await
    .map_err(|e| format!("migrate task join: {e}"))?
    .map_err(|e| format!("migrate io: {e}"))
}

/// Close the existing pipe, rotate the target log if needed, then re-open
/// the pipe. Returns true if rotation happened.
pub async fn rotate_and_reattach(
    pane_id: &str,
    target: &LogTarget,
) -> Result<bool, String> {
    let path = target.win_path();
    let size = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("log metadata: {e}")),
    };
    if size <= MAX_LOG_SIZE {
        return Ok(false);
    }
    disable_pipe_pane(pane_id).await?;
    rotate_if_needed(&path).map_err(|e| format!("log rotate: {e}"))?;
    enable_pipe_pane(pane_id, target).await?;
    Ok(true)
}
