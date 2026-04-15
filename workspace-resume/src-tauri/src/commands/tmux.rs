use crate::models::tmux_state::{TmuxPane, TmuxSession, TmuxState, TmuxWindow, WindowPaneStatus};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Process-wide cache keyed by a pane's shell PID → detected Claude
/// account (`"andrea"` or `"bravura"`). Both the companion poller and
/// the Tauri command path read through this helper so detection shells
/// out at most once per pane lifetime across the whole app.
static PID_ACCOUNT_CACHE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Walk a pane's shell PID (+ direct and grand-children) and read
/// each process's `/proc/<pid>/environ`. Returns `"bravura"` if any
/// descendant has `CLAUDE_CONFIG_DIR=~/.claude-b`, `"andrea"` if a
/// claude-ish process exists without a Bravura env var, or `None`
/// when no Claude process is visible under the pane yet.
///
/// Results are cached in [`PID_ACCOUNT_CACHE`] for the lifetime of
/// the process — `wsl.exe` cost is paid exactly once per shell pid.
pub async fn detect_claude_account(shell_pid: &str) -> Option<String> {
    if shell_pid.is_empty() {
        return None;
    }
    {
        let cache = PID_ACCOUNT_CACHE.lock().ok()?;
        if let Some(v) = cache.get(shell_pid) {
            return Some(v.clone());
        }
    }
    let script = format!(
        r#"pid={pid}
children=$(pgrep -P $pid 2>/dev/null)
grand=""
for c in $children; do
  grand="$grand $(pgrep -P $c 2>/dev/null)"
done
candidates="$pid $children $grand"

for p in $candidates; do
  [ -z "$p" ] && continue
  env_val=$(tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -E '^CLAUDE_CONFIG_DIR=' | head -1 | cut -d= -f2-)
  if [ -n "$env_val" ]; then
    case "$env_val" in
      *claude-b*) echo bravura; exit 0 ;;
      *)          echo andrea;  exit 0 ;;
    esac
  fi
done

for p in $candidates; do
  [ -z "$p" ] && continue
  comm=$(cat /proc/$p/comm 2>/dev/null)
  case "$comm" in
    claude|node) echo andrea; exit 0 ;;
  esac
done
"#,
        pid = shell_pid
    );
    let out = run_tmux_command_async(script).await.ok()?;
    let line = out.lines().find(|l| !l.trim().is_empty())?.trim();
    let account = match line {
        "bravura" => Some("bravura".to_string()),
        "andrea" => Some("andrea".to_string()),
        _ => None,
    };
    if let Some(ref v) = account {
        if let Ok(mut cache) = PID_ACCOUNT_CACHE.lock() {
            cache.insert(shell_pid.to_string(), v.clone());
        }
    }
    account
}

/// Remove a shell PID from the account-detection cache so the next
/// call to [`detect_claude_account`] re-walks `/proc`. Used by the
/// poller when it notices Claude has exited from a pane — the same
/// shell PID may later run a different account's Claude.
pub fn invalidate_account_cache(shell_pid: &str) {
    if let Ok(mut cache) = PID_ACCOUNT_CACHE.lock() {
        cache.remove(shell_pid);
    }
}

/// Run a tmux command via wsl.exe and return stdout as a String.
/// Returns Ok(empty string) for "no server running" / "no sessions" (not an error).
///
/// This is synchronous and blocks the calling thread on `wsl.exe`. Do NOT call
/// from async contexts — use [`run_tmux_command_async`] instead so the wait
/// happens on tokio's blocking pool rather than a runtime worker.
pub fn run_tmux_command(script: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", script]);

    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute wsl.exe: {}", e))?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // "no server running" and "no sessions" are expected when tmux isn't active
    if stderr.contains("no server running") || stderr.contains("no sessions") {
        return Ok(String::new());
    }

    if !output.status.success() {
        return Err(format!("tmux command failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Async wrapper that runs [`run_tmux_command`] on tokio's blocking pool.
/// Call this from the companion's async handlers and hot polling loop — the
/// sync variant would park a tokio worker on every `wsl.exe` invocation and
/// starve the runtime under load.
pub async fn run_tmux_command_async(script: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || run_tmux_command(&script))
        .await
        .map_err(|e| format!("tmux join error: {}", e))?
}

/// Write binary data to a file on the WSL filesystem by piping through
/// `wsl.exe -e bash -c "cat > '<path>'"`. The parent directory is created
/// automatically. This is synchronous — call [`write_file_to_wsl_async`]
/// from async contexts.
pub fn write_file_to_wsl(path: &str, data: &[u8]) -> Result<(), String> {
    let path_esc = path.replace('\'', r"'\''");
    let script = format!(
        "mkdir -p \"$(dirname '{}')\" && cat > '{}'",
        path_esc, path_esc,
    );
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", &script]);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let mut child = cmd.spawn().map_err(|e| format!("spawn wsl: {}", e))?;
    {
        use std::io::Write;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "no stdin handle".to_string())?;
        stdin
            .write_all(data)
            .map_err(|e| format!("stdin write: {}", e))?;
    }
    drop(child.stdin.take());

    let status = child.wait().map_err(|e| format!("wait: {}", e))?;
    if !status.success() {
        return Err(format!("wsl write_file exited {}", status));
    }
    Ok(())
}

/// Async wrapper for [`write_file_to_wsl`].
pub async fn write_file_to_wsl_async(path: String, data: Vec<u8>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || write_file_to_wsl(&path, &data))
        .await
        .map_err(|e| format!("join: {}", e))?
}

/// Parse a pipe-delimited session line into a TmuxSession.
/// Format: `name|window_count|attached_flag`
pub fn parse_session_line(line: &str) -> Option<TmuxSession> {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 3 {
        return None;
    }
    Some(TmuxSession {
        name: parts[0].to_string(),
        windows: parts[1].parse().unwrap_or(0),
        attached: parts[2] == "1",
    })
}

/// Parse a pipe-delimited window line into a TmuxWindow.
/// Format: `index|name|pane_count|active_flag`
pub fn parse_window_line(line: &str) -> Option<TmuxWindow> {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 4 {
        return None;
    }
    Some(TmuxWindow {
        index: parts[0].parse().unwrap_or(0),
        name: parts[1].to_string(),
        panes: parts[2].parse().unwrap_or(0),
        active: parts[3] == "1",
    })
}

/// Parse a pipe-delimited pane line into a TmuxPane.
///
/// Canonical format (11+ fields):
/// `pane_index|pane_id|width|height|top|left|active|pane_pid|current_command|current_path|...|start_command`
///
/// Legacy format (9 fields, no pane_pid, no start_command):
/// `pane_index|pane_id|width|height|top|left|active|current_command|current_path`
///
/// The legacy shape is kept alive for the unit tests but production
/// callers use the canonical one so they get `pane_pid` + `start_command`
/// for downstream detection.
pub fn parse_pane_line(line: &str) -> Option<TmuxPane> {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 9 {
        return None;
    }
    // Canonical format starts at 11 parts. `pane_pid` sits at index 7 so
    // it's in a stable position regardless of how many pipe segments the
    // `current_path` contributes (paths can contain literal `|`).
    let (pane_pid, current_command, path_start, start_command) = if parts.len() >= 11 {
        let pane_pid = parts[7].to_string();
        let current_command = parts[8].to_string();
        let start_command = parts[parts.len() - 1].to_string();
        (pane_pid, current_command, 9, start_command)
    } else if parts.len() >= 10 {
        // 10-field legacy with start_command but no pane_pid
        let current_command = parts[7].to_string();
        let start_command = parts[parts.len() - 1].to_string();
        (String::new(), current_command, 8, start_command)
    } else {
        // 9-field legacy (pre-pane_pid, pre-start_command)
        let current_command = parts[7].to_string();
        (String::new(), current_command, 8, String::new())
    };
    let path_end = if parts.len() >= 10 { parts.len() - 1 } else { parts.len() };
    let current_path = parts[path_start..path_end].join("|");
    Some(TmuxPane {
        pane_index: parts[0].parse().unwrap_or(0),
        pane_id: parts[1].to_string(),
        width: parts[2].parse().unwrap_or(0),
        height: parts[3].parse().unwrap_or(0),
        top: parts[4].parse().unwrap_or(0),
        left: parts[5].parse().unwrap_or(0),
        active: parts[6] == "1",
        current_command,
        current_path,
        start_command,
        pane_pid,
        claude_account: None,
    })
}

/// Parse multiple lines using a line parser, skipping empty/unparseable lines.
fn parse_lines<T, F>(output: &str, parser: F) -> Vec<T>
where
    F: Fn(&str) -> Option<T>,
{
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| parser(line))
        .collect()
}

// ---------------------
// Query IPC Commands
// ---------------------

/// Cross-session "all panes running Claude" query.
/// Returns a flat list of panes across every tmux session whose
/// current_command or start_command identifies them as a Claude session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivePane {
    pub id: String,
    pub session_name: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_index: u32,
    pub current_command: String,
    pub current_path: String,
    pub start_command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pane_pid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_account: Option<String>,
}

/// Fill in `claude_account` for every claude-alive pane in the slice
/// by calling [`detect_claude_account`] on its `pane_pid`. The helper
/// is cached process-wide so this is cheap after the first call per
/// pane. Panes that aren't running Claude are left with `None`.
pub async fn populate_claude_accounts(panes: &mut [TmuxPane]) {
    for pane in panes.iter_mut() {
        if pane.claude_account.is_some() {
            continue;
        }
        if !pane_is_claude(&pane.current_command, &pane.start_command) {
            continue;
        }
        if pane.pane_pid.is_empty() {
            continue;
        }
        pane.claude_account = detect_claude_account(&pane.pane_pid).await;
    }
}

/// Return true if the pane is "running Claude" — either the foreground
/// command is `claude`/`claude-b` directly, or the pane was started with
/// the claude binary and the current foreground is not a plain shell
/// (i.e., Claude is alive and waiting on a child tool invocation).
pub fn pane_is_claude(current_command: &str, start_command: &str) -> bool {
    let cur = current_command.to_ascii_lowercase();
    if cur.contains("claude") {
        return true;
    }
    let start = start_command.to_ascii_lowercase();
    if !start.contains("claude") {
        return false;
    }
    let is_shell = matches!(
        cur.as_str(),
        "bash" | "zsh" | "sh" | "fish" | "dash" | "-" | ""
    );
    !is_shell
}

#[tauri::command]
pub async fn list_active_claude_panes() -> Result<Vec<ActivePane>, String> {
    let script = "tmux list-panes -a -F \
        '#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_pid}|#{pane_current_command}|#{pane_current_path}|#{pane_start_command}' 2>/dev/null";
    let out = run_tmux_command_async(script.to_string()).await?;
    let mut result = Vec::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 8 {
            continue;
        }
        let session = parts[0].to_string();
        let window_index: u32 = parts[1].parse().unwrap_or(0);
        let window_name = parts[2].to_string();
        let pane_index: u32 = parts[3].parse().unwrap_or(0);
        let pane_pid = parts[4].to_string();
        let current_command = parts[5].to_string();
        // Path can contain literal '|' — start_command is always last.
        let start_command = parts[parts.len() - 1].to_string();
        let current_path = parts[6..parts.len() - 1].join("|");
        if !pane_is_claude(&current_command, &start_command) {
            continue;
        }
        result.push(ActivePane {
            id: format!("{}:{}.{}", session, window_index, pane_index),
            session_name: session,
            window_index,
            window_name,
            pane_index,
            current_command,
            current_path,
            start_command,
            pane_pid,
            claude_account: None,
        });
    }
    // Populate claude_account for each entry — cached helper so repeat
    // calls are cheap.
    for entry in result.iter_mut() {
        if entry.pane_pid.is_empty() {
            continue;
        }
        entry.claude_account = detect_claude_account(&entry.pane_pid).await;
    }
    Ok(result)
}

#[tauri::command]
pub async fn list_tmux_sessions() -> Result<Vec<TmuxSession>, String> {
    let script =
        "tmux list-sessions -F '#{session_name}|#{session_windows}|#{session_attached}' 2>/dev/null";
    let output = run_tmux_command_async(script.to_string()).await?;
    if output.is_empty() {
        return Ok(vec![]);
    }
    Ok(parse_lines(&output, parse_session_line))
}

#[tauri::command]
pub async fn list_tmux_windows(session_name: String) -> Result<Vec<TmuxWindow>, String> {
    let script = format!(
        "tmux list-windows -t '{}' -F '#{{window_index}}|#{{window_name}}|#{{window_panes}}|#{{window_active}}' 2>/dev/null",
        session_name
    );
    let output = run_tmux_command_async(script).await?;
    if output.is_empty() {
        return Ok(vec![]);
    }
    Ok(parse_lines(&output, parse_window_line))
}

#[tauri::command]
pub async fn list_tmux_panes(
    session_name: String,
    window_index: u32,
) -> Result<Vec<TmuxPane>, String> {
    let script = format!(
        "tmux list-panes -t '{}:{}' -F '#{{pane_index}}|#{{pane_id}}|#{{pane_width}}|#{{pane_height}}|#{{pane_top}}|#{{pane_left}}|#{{pane_active}}|#{{pane_pid}}|#{{pane_current_command}}|#{{pane_current_path}}|#{{pane_start_command}}' 2>/dev/null",
        session_name, window_index
    );
    let output = run_tmux_command_async(script).await?;
    if output.is_empty() {
        return Ok(vec![]);
    }
    let mut panes = parse_lines(&output, parse_pane_line);
    populate_claude_accounts(&mut panes).await;
    Ok(panes)
}

#[tauri::command]
pub async fn get_tmux_state(
    session_name: String,
    window_index: u32,
) -> Result<TmuxState, String> {
    // Batched query: single wsl.exe call with marker-separated output
    let script = format!(
        concat!(
            "echo '---SESSIONS---'; ",
            "tmux list-sessions -F '#{{session_name}}|#{{session_windows}}|#{{session_attached}}' 2>/dev/null; ",
            "echo '---WINDOWS---'; ",
            "tmux list-windows -t '{}' -F '#{{window_index}}|#{{window_name}}|#{{window_panes}}|#{{window_active}}' 2>/dev/null; ",
            "echo '---PANES---'; ",
            "tmux list-panes -t '{}:{}' -F '#{{pane_index}}|#{{pane_id}}|#{{pane_width}}|#{{pane_height}}|#{{pane_top}}|#{{pane_left}}|#{{pane_active}}|#{{pane_pid}}|#{{pane_current_command}}|#{{pane_current_path}}|#{{pane_start_command}}' 2>/dev/null; ",
            "echo '---END---'"
        ),
        session_name, session_name, window_index
    );

    let output = run_tmux_command_async(script).await?;

    // Parse by splitting on marker lines
    let mut sessions = vec![];
    let mut windows = vec![];
    let mut panes = vec![];

    let mut current_section = "";
    for line in output.lines() {
        let trimmed = line.trim();
        match trimmed {
            "---SESSIONS---" => {
                current_section = "sessions";
                continue;
            }
            "---WINDOWS---" => {
                current_section = "windows";
                continue;
            }
            "---PANES---" => {
                current_section = "panes";
                continue;
            }
            "---END---" => break,
            _ => {}
        }
        if trimmed.is_empty() {
            continue;
        }
        match current_section {
            "sessions" => {
                if let Some(s) = parse_session_line(trimmed) {
                    sessions.push(s);
                }
            }
            "windows" => {
                if let Some(w) = parse_window_line(trimmed) {
                    windows.push(w);
                }
            }
            "panes" => {
                if let Some(p) = parse_pane_line(trimmed) {
                    panes.push(p);
                }
            }
            _ => {}
        }
    }

    populate_claude_accounts(&mut panes).await;

    Ok(TmuxState {
        sessions,
        windows,
        panes,
    })
}

// ---------------------
// Mutation IPC Commands
// ---------------------

#[tauri::command]
pub async fn create_pane(
    session_name: String,
    window_index: u32,
    direction: String,
) -> Result<Vec<TmuxPane>, String> {
    // direction is "h" or "v" for horizontal/vertical split
    let dir_flag = match direction.as_str() {
        "h" => "-h",
        "v" => "-v",
        _ => return Err(format!("Invalid direction '{}': must be 'h' or 'v'", direction)),
    };

    let script = format!(
        "tmux split-window {} -t '{}:{}'",
        dir_flag, session_name, window_index
    );
    run_tmux_command_async(script).await?;

    // Re-query panes and return updated list
    list_tmux_panes(session_name, window_index).await
}

#[tauri::command]
pub async fn apply_layout(
    session_name: String,
    window_index: u32,
    layout: String,
) -> Result<Vec<TmuxPane>, String> {
    // Validate layout name
    let valid_layouts = [
        "even-horizontal",
        "even-vertical",
        "main-horizontal",
        "main-vertical",
        "tiled",
    ];
    if !valid_layouts.contains(&layout.as_str()) {
        return Err(format!(
            "Invalid layout '{}': must be one of {:?}",
            layout, valid_layouts
        ));
    }

    let script = format!(
        "tmux select-layout -t '{}:{}' '{}'",
        session_name, window_index, layout
    );
    run_tmux_command_async(script).await?;

    // Re-query panes and return updated list
    list_tmux_panes(session_name, window_index).await
}

#[tauri::command]
pub async fn send_to_pane(
    session_name: String,
    window_index: u32,
    pane_index: u32,
    command: String,
) -> Result<(), String> {
    // Escape single quotes in command by replacing ' with '\''
    let escaped = command.replace('\'', "'\\''");
    let script = format!(
        "tmux send-keys -t '{}:{}.{}' '{}' Enter",
        session_name, window_index, pane_index, escaped
    );
    eprintln!("[send_to_pane] target={}:{}.{} command={}", session_name, window_index, pane_index, command);
    run_tmux_command_async(script).await?;
    Ok(())
}

/// Send Ctrl-C twice to a pane to interrupt and exit the current process.
/// Includes a brief sleep between signals so the process has time to respond.
#[tauri::command]
pub async fn cancel_pane_command(
    session_name: String,
    window_index: u32,
    pane_index: u32,
) -> Result<(), String> {
    let target = format!("{}:{}.{}", session_name, window_index, pane_index);
    let script = format!(
        "tmux send-keys -t '{t}' C-c; sleep 0.3; tmux send-keys -t '{t}' C-c",
        t = target
    );
    eprintln!("[cancel_pane_command] target={}", target);
    run_tmux_command_async(script).await?;
    Ok(())
}

#[tauri::command]
pub async fn kill_pane(
    session_name: String,
    window_index: u32,
    pane_index: u32,
) -> Result<Vec<TmuxPane>, String> {
    let script = format!(
        "tmux kill-pane -t '{}:{}.{}'",
        session_name, window_index, pane_index
    );
    run_tmux_command_async(script).await?;

    // Re-query panes and return updated list
    list_tmux_panes(session_name, window_index).await
}

#[tauri::command]
pub async fn create_window(session_name: String) -> Result<Vec<TmuxWindow>, String> {
    let script = format!(
        "tmux new-window -t '{}'",
        session_name
    );
    run_tmux_command_async(script).await?;

    // Re-query windows and return updated list
    list_tmux_windows(session_name).await
}

#[tauri::command]
pub async fn kill_window(
    session_name: String,
    window_index: u32,
) -> Result<Vec<TmuxWindow>, String> {
    let script = format!(
        "tmux kill-window -t '{}:{}'",
        session_name, window_index
    );
    run_tmux_command_async(script).await?;

    // Re-query windows and return updated list
    list_tmux_windows(session_name).await
}

/// Trigger tmux-resurrect save (snapshot all sessions/windows/panes).
#[tauri::command]
pub async fn tmux_resurrect_save() -> Result<String, String> {
    let script = "bash ~/.tmux/plugins/tmux-resurrect/scripts/save.sh";
    let result = run_tmux_command_async(format!("tmux run-shell '{}'", script)).await?;
    Ok(result)
}

/// Trigger tmux-resurrect restore (restore last saved snapshot).
#[tauri::command]
pub async fn tmux_resurrect_restore() -> Result<String, String> {
    let script = "bash ~/.tmux/plugins/tmux-resurrect/scripts/restore.sh";
    let result = run_tmux_command_async(format!("tmux run-shell '{}'", script)).await?;
    Ok(result)
}

/// Swap two tmux panes within a window.
#[tauri::command]
pub async fn swap_tmux_pane(
    session_name: String,
    window_index: u32,
    source_pane: u32,
    target_pane: u32,
) -> Result<Vec<TmuxPane>, String> {
    let script = format!(
        "tmux swap-pane -s '{}:{}.{}' -t '{}:{}.{}'",
        session_name, window_index, source_pane,
        session_name, window_index, target_pane
    );
    run_tmux_command_async(script).await?;
    list_tmux_panes(session_name, window_index).await
}

/// Swap two tmux windows within a session.
#[tauri::command]
pub async fn swap_tmux_window(
    session_name: String,
    source_index: u32,
    target_index: u32,
) -> Result<Vec<TmuxWindow>, String> {
    let script = format!(
        "tmux swap-window -s '{}:{}' -t '{}:{}'",
        session_name, source_index, session_name, target_index
    );
    run_tmux_command_async(script).await?;
    list_tmux_windows(session_name).await
}

/// Switch the attached tmux client to a different session.
#[tauri::command]
pub async fn switch_tmux_session(session_name: String) -> Result<(), String> {
    // switch-client changes which session an attached client is viewing.
    // If no client is attached, this is a no-op (we still succeed).
    let script = format!(
        "tmux switch-client -t '{}' 2>/dev/null || true",
        session_name
    );
    run_tmux_command_async(script).await?;
    Ok(())
}

/// Switch the active window within a tmux session.
#[tauri::command]
pub async fn select_tmux_window(session_name: String, window_index: u32) -> Result<(), String> {
    let script = format!(
        "tmux select-window -t '{}:{}'",
        session_name, window_index
    );
    run_tmux_command_async(script).await?;
    Ok(())
}

#[tauri::command]
pub async fn rename_session(old_name: String, new_name: String) -> Result<(), String> {
    let script = format!(
        "tmux rename-session -t '{}' '{}'",
        old_name, new_name
    );
    run_tmux_command_async(script).await?;
    Ok(())
}

#[tauri::command]
pub async fn rename_window(
    session_name: String,
    window_index: u32,
    new_name: String,
) -> Result<(), String> {
    let script = format!(
        "tmux rename-window -t '{}:{}' '{}'",
        session_name, window_index, new_name
    );
    run_tmux_command_async(script).await?;
    Ok(())
}

#[tauri::command]
pub async fn create_session(session_name: String) -> Result<Vec<TmuxSession>, String> {
    // -d = detached so it doesn't steal focus from the current terminal
    let script = format!(
        "tmux new-session -d -s '{}'",
        session_name
    );
    run_tmux_command_async(script).await?;

    list_tmux_sessions().await
}

/// Set up a pane grid with exactly `cols` columns and `rows` rows.
/// Kills excess panes, creates columns via horizontal splits, equalizes,
/// then splits each column vertically. Produces the correct wide-monitor
/// layout (e.g. 3 wide × 2 tall for 6 panes) instead of tmux's default
/// `tiled` which tends toward more rows than columns.
#[tauri::command]
pub async fn setup_pane_grid(
    session_name: String,
    window_index: u32,
    cols: u32,
    rows: u32,
) -> Result<Vec<TmuxPane>, String> {
    if cols == 0 || rows == 0 {
        return Err("cols and rows must be > 0".to_string());
    }

    let target = format!("{}:{}", session_name, window_index);

    if cols == 1 && rows == 1 {
        // Just kill all panes except the active one
        let script = format!("tmux kill-pane -a -t '{}' 2>/dev/null || true", target);
        run_tmux_command_async(script).await?;
        return list_tmux_panes(session_name, window_index).await;
    }

    let mut script = String::new();

    // 1. Kill all panes except one
    script.push_str(&format!(
        "tmux kill-pane -a -t '{}' 2>/dev/null || true; ",
        target
    ));

    // 2. Create (cols-1) horizontal splits → cols columns
    for _ in 0..(cols - 1) {
        script.push_str(&format!("tmux split-window -h -t '{}'; ", target));
    }

    // 3. Equalize columns
    if cols > 1 {
        script.push_str(&format!(
            "tmux select-layout -t '{}' even-horizontal; ",
            target
        ));
    }

    // 4. Split each column vertically (rows-1) times using stable pane IDs
    if rows > 1 {
        // Capture the pane IDs of the current columns
        script.push_str(&format!(
            "PIDS=$(tmux list-panes -t '{}' -F '#{{pane_id}}'); ",
            target
        ));
        for _ in 0..(rows - 1) {
            script.push_str(
                "for pid in $PIDS; do tmux split-window -v -t \"$pid\"; done; ",
            );
        }
    }

    run_tmux_command_async(script).await?;

    list_tmux_panes(session_name, window_index).await
}

/// Pick a tmux `select-layout` preset name for a given (cols, rows) shape.
fn layout_preset_for(cols: u32, rows: u32) -> &'static str {
    match (cols, rows) {
        (_, 1) => "even-horizontal",
        (1, _) => "even-vertical",
        _ => "tiled",
    }
}

/// Reflow the pane grid to match the target shape **without killing any panes**.
/// Same count → `select-layout <preset>`. Grow → split (`t - n`) times off the
/// last pane (inheriting its cwd), then `select-layout`. Reduce is rejected —
/// use `reduce_pane_grid` after explicit kill confirmation.
#[tauri::command]
pub async fn reflow_pane_grid(
    session_name: String,
    window_index: u32,
    cols: u32,
    rows: u32,
) -> Result<Vec<TmuxPane>, String> {
    if cols == 0 || rows == 0 {
        return Err("cols and rows must be > 0".to_string());
    }
    let target = format!("{}:{}", session_name, window_index);
    let current = list_tmux_panes(session_name.clone(), window_index).await?;
    let n = current.len() as u32;
    let t = cols * rows;

    if t < n {
        return Err(format!(
            "reflow cannot reduce pane count ({} → {}); use reduce_pane_grid",
            n, t
        ));
    }

    let preset = layout_preset_for(cols, rows);

    if t == n {
        if cols == 1 && rows == 1 {
            return Ok(current);
        }
        let script = format!("tmux select-layout -t '{}' {}", target, preset);
        run_tmux_command_async(script).await?;
        return list_tmux_panes(session_name, window_index).await;
    }

    // Grow: append (t - n) panes off the last pane so the new splits inherit
    // its working directory, then apply the preset to even out the geometry.
    let base = current
        .last()
        .ok_or_else(|| "cannot grow pane grid: no existing panes to split from".to_string())?;
    let base_id = base.pane_id.clone();
    let mut script = String::new();
    script.push_str(&format!(
        "CWD=$(tmux display-message -p -t '{}' '#{{pane_current_path}}'); ",
        base_id
    ));
    for _ in 0..(t - n) {
        script.push_str(&format!(
            "tmux split-window -h -t '{}' -c \"$CWD\"; ",
            base_id
        ));
    }
    script.push_str(&format!("tmux select-layout -t '{}' {}; ", target, preset));

    run_tmux_command_async(script).await?;
    list_tmux_panes(session_name, window_index).await
}

/// List panes at `pane_index >= keep_count` — the ones that would be killed
/// by a reduce-to-`keep_count` operation. Frontend uses this to populate a
/// confirmation modal before calling `reduce_pane_grid`.
#[tauri::command]
pub async fn list_kill_targets(
    session_name: String,
    window_index: u32,
    keep_count: u32,
) -> Result<Vec<TmuxPane>, String> {
    let panes = list_tmux_panes(session_name, window_index).await?;
    Ok(panes
        .into_iter()
        .filter(|p| p.pane_index >= keep_count)
        .collect())
}

/// Reduce the pane grid by killing only the excess panes (indices >= `cols*rows`),
/// then reflowing survivors to the target preset. Low-index panes and their
/// running processes are preserved — the opposite of `setup_pane_grid`'s
/// `kill-pane -a` behavior.
#[tauri::command]
pub async fn reduce_pane_grid(
    session_name: String,
    window_index: u32,
    cols: u32,
    rows: u32,
) -> Result<Vec<TmuxPane>, String> {
    if cols == 0 || rows == 0 {
        return Err("cols and rows must be > 0".to_string());
    }
    let target = format!("{}:{}", session_name, window_index);
    let current = list_tmux_panes(session_name.clone(), window_index).await?;
    let n = current.len() as u32;
    let t = cols * rows;

    if t >= n {
        return Err(format!(
            "reduce_pane_grid requires a smaller target ({} → {}); use reflow_pane_grid",
            n, t
        ));
    }

    let victims: Vec<String> = current
        .iter()
        .filter(|p| p.pane_index >= t)
        .map(|p| p.pane_id.clone())
        .collect();
    if victims.is_empty() {
        return list_tmux_panes(session_name, window_index).await;
    }

    let mut script = String::new();
    for pid in &victims {
        script.push_str(&format!("tmux kill-pane -t '{}'; ", pid));
    }
    let preset = layout_preset_for(cols, rows);
    if !(cols == 1 && rows == 1) {
        script.push_str(&format!("tmux select-layout -t '{}' {}; ", target, preset));
    }

    run_tmux_command_async(script).await?;
    list_tmux_panes(session_name, window_index).await
}

/// Check all panes across all windows in a session for approval prompts.
/// Returns a map of window_index -> WindowPaneStatus (has_active + waiting_panes).
/// Designed to run on a slower poll cycle (15s) to avoid excessive capture-pane calls.
///
/// **Pattern fragility note:** Detection relies on string-matching Claude Code's
/// approval prompt text. If Claude Code changes its prompt format, this will need
/// patching. See BACKLOG.md F-53.
#[tauri::command]
pub async fn check_pane_statuses(
    session_name: String,
) -> Result<HashMap<String, WindowPaneStatus>, String> {
    // Single batched script:
    // 1. List all panes across all windows with their commands (-s flag)
    // 2. For panes running claude, capture last 10 lines and check for approval prompts
    let script = format!(
        concat!(
            "echo '---PANES---'; ",
            "tmux list-panes -s -t '{sess}' -F '#{{window_index}}|#{{pane_index}}|#{{pane_current_command}}|#{{pane_current_path}}|#{{pane_start_command}}' 2>/dev/null; ",
            "echo '---CAPTURES---'; ",
            "tmux list-panes -s -t '{sess}' -F '#{{window_index}}|#{{pane_index}}|#{{pane_current_command}}|#{{pane_start_command}}' 2>/dev/null | while IFS='|' read -r win idx cmd scmd; do ",
            "  case \"$cmd$scmd\" in *[Cc]laude*|*node*) ",
            "    echo \"===W${{win}}P${{idx}}===\"; ",
            "    tmux capture-pane -t '{sess}:'\"$win\".\"$idx\" -p -S -10 2>/dev/null || true; ",
            "    ;; esac; ",
            "done; ",
            "echo '---END---'"
        ),
        sess = session_name
    );

    let output = run_tmux_command_async(script).await?;
    if output.is_empty() {
        return Ok(HashMap::new());
    }

    // Parse: collect pane info per window, then check captures for approval patterns
    let mut window_panes: HashMap<String, Vec<(u32, bool, String)>> = HashMap::new(); // win -> [(pane_idx, is_claude, path)]
    let mut waiting: HashMap<String, Vec<u32>> = HashMap::new(); // win -> [waiting pane indices]

    let mut section = "";
    let mut current_win = String::new();
    let mut current_pane: u32 = 0;
    let mut capture_lines = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        match trimmed {
            "---PANES---" => { section = "panes"; continue; }
            "---CAPTURES---" => { section = "captures"; continue; }
            "---END---" => {
                // Flush last capture
                if !current_win.is_empty() && !capture_lines.is_empty() {
                    if is_approval_prompt(&capture_lines) {
                        waiting.entry(current_win.clone()).or_default().push(current_pane);
                    }
                }
                break;
            }
            _ => {}
        }

        match section {
            "panes" => {
                if trimmed.is_empty() { continue; }
                let parts: Vec<&str> = trimmed.split('|').collect();
                if parts.len() >= 3 {
                    let win_idx = parts[0].to_string();
                    let pane_idx: u32 = parts[1].parse().unwrap_or(0);
                    let cur_cmd = parts[2].to_string();
                    // Path can contain '|'. start_command is always the LAST
                    // field (added in Phase 3); if absent, fall back to path-only.
                    let (path, start_cmd) = if parts.len() >= 5 {
                        (parts[3..parts.len() - 1].join("|"), parts[parts.len() - 1].to_string())
                    } else if parts.len() == 4 {
                        (parts[3].to_string(), String::new())
                    } else {
                        (String::new(), String::new())
                    };
                    let is_claude = pane_is_claude(&cur_cmd, &start_cmd);
                    window_panes.entry(win_idx).or_default().push((pane_idx, is_claude, path));
                }
            }
            "captures" => {
                // Check for capture marker
                if trimmed.starts_with("===W") && trimmed.ends_with("===") {
                    // Flush previous capture
                    if !current_win.is_empty() && !capture_lines.is_empty() {
                        if is_approval_prompt(&capture_lines) {
                            waiting.entry(current_win.clone()).or_default().push(current_pane);
                        }
                    }
                    // Parse ===W{win}P{pane}===
                    let inner = &trimmed[3..trimmed.len()-3]; // strip === ===
                    if let Some(p_pos) = inner.find('P') {
                        current_win = inner[1..p_pos].to_string(); // skip 'W'
                        current_pane = inner[p_pos+1..].parse().unwrap_or(0);
                    }
                    capture_lines.clear();
                } else {
                    capture_lines.push(trimmed.to_string());
                }
            }
            _ => {}
        }
    }

    // Build result
    let mut result = HashMap::new();
    for (win_idx, panes) in &window_panes {
        let active_panes: Vec<u32> = panes.iter()
            .filter(|(_, is_claude, _)| *is_claude)
            .map(|(idx, _, _)| *idx)
            .collect();
        let active_paths: Vec<String> = panes.iter()
            .filter(|(_, is_claude, _)| *is_claude)
            .map(|(_, _, path)| path.clone())
            .filter(|p| !p.is_empty())
            .collect();
        let has_active = !active_panes.is_empty();
        let waiting_panes = waiting.get(win_idx).cloned().unwrap_or_default();
        result.insert(win_idx.clone(), WindowPaneStatus {
            has_active,
            active_panes,
            active_paths,
            waiting_panes,
        });
    }

    Ok(result)
}

/// Check captured pane content for Claude Code's selection prompt.
///
/// Claude Code presents choices with a ❯ (U+276F) cursor before the
/// selected option: "❯ 1. ...", "❯ 2. ...", etc. This character only
/// appears in this context — normal conversation output never contains it.
///
/// We check for ❯ followed by a space and a digit+period (e.g. "❯ 1.").
///
/// **Pattern fragility:** If Claude Code changes the selection cursor, update here.
fn is_approval_prompt(lines: &[String]) -> bool {
    for line in lines {
        // Look for ❯ followed by " N." where N is a digit
        if let Some(pos) = line.find('\u{276F}') {
            let after = &line[pos + '\u{276F}'.len_utf8()..];
            let after_trimmed = after.trim_start();
            if after_trimmed.len() >= 2 {
                let bytes = after_trimmed.as_bytes();
                if bytes[0].is_ascii_digit() && bytes[1] == b'.' {
                    return true;
                }
            }
        }
    }
    false
}

#[tauri::command]
pub async fn kill_session(session_name: String) -> Result<Vec<TmuxSession>, String> {
    let script = format!(
        "tmux kill-session -t '{}'",
        session_name
    );
    run_tmux_command_async(script).await?;

    // Re-query sessions and return updated list
    list_tmux_sessions().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_session_line_valid() {
        let line = "workspace|3|1";
        let session = parse_session_line(line).unwrap();
        assert_eq!(session.name, "workspace");
        assert_eq!(session.windows, 3);
        assert!(session.attached);
    }

    #[test]
    fn test_parse_session_line_not_attached() {
        let line = "myproject|1|0";
        let session = parse_session_line(line).unwrap();
        assert_eq!(session.name, "myproject");
        assert_eq!(session.windows, 1);
        assert!(!session.attached);
    }

    #[test]
    fn test_parse_session_line_too_few_parts() {
        let line = "workspace|3";
        assert!(parse_session_line(line).is_none());
    }

    #[test]
    fn test_parse_session_line_empty() {
        let line = "";
        assert!(parse_session_line(line).is_none());
    }

    #[test]
    fn test_parse_window_line_valid() {
        let line = "0|editor|2|1";
        let window = parse_window_line(line).unwrap();
        assert_eq!(window.index, 0);
        assert_eq!(window.name, "editor");
        assert_eq!(window.panes, 2);
        assert!(window.active);
    }

    #[test]
    fn test_parse_window_line_inactive() {
        let line = "1|terminal|1|0";
        let window = parse_window_line(line).unwrap();
        assert_eq!(window.index, 1);
        assert_eq!(window.name, "terminal");
        assert_eq!(window.panes, 1);
        assert!(!window.active);
    }

    #[test]
    fn test_parse_window_line_too_few_parts() {
        let line = "0|editor";
        assert!(parse_window_line(line).is_none());
    }

    #[test]
    fn test_parse_pane_line_valid() {
        let line = "0|%5|80|24|0|0|1|bash|/home/user";
        let pane = parse_pane_line(line).unwrap();
        assert_eq!(pane.pane_index, 0);
        assert_eq!(pane.pane_id, "%5");
        assert_eq!(pane.width, 80);
        assert_eq!(pane.height, 24);
        assert_eq!(pane.top, 0);
        assert_eq!(pane.left, 0);
        assert!(pane.active);
        assert_eq!(pane.current_command, "bash");
        assert_eq!(pane.current_path, "/home/user");
    }

    #[test]
    fn test_parse_pane_line_inactive() {
        let line = "1|%6|40|24|0|80|0|vim|/tmp";
        let pane = parse_pane_line(line).unwrap();
        assert_eq!(pane.pane_index, 1);
        assert_eq!(pane.pane_id, "%6");
        assert_eq!(pane.width, 40);
        assert_eq!(pane.left, 80);
        assert!(!pane.active);
        assert_eq!(pane.current_command, "vim");
        assert_eq!(pane.current_path, "/tmp");
    }

    #[test]
    fn test_parse_pane_line_too_few_parts() {
        let line = "0|%5|80|24";
        assert!(parse_pane_line(line).is_none());
    }

    #[test]
    fn test_parse_lines_empty_output() {
        let output = "";
        let result: Vec<TmuxSession> = parse_lines(output, parse_session_line);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_lines_multiple_sessions() {
        let output = "workspace|3|1\ndev|1|0\ntest|2|0";
        let result = parse_lines(output, parse_session_line);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "workspace");
        assert_eq!(result[1].name, "dev");
        assert_eq!(result[2].name, "test");
    }

    #[test]
    fn test_parse_lines_skips_empty_lines() {
        let output = "workspace|3|1\n\ndev|1|0\n   \n";
        let result = parse_lines(output, parse_session_line);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_lines_skips_invalid_lines() {
        let output = "workspace|3|1\nbad-line\ndev|1|0";
        let result = parse_lines(output, parse_session_line);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_batched_output_parsing() {
        // Simulate the batched get_tmux_state output format
        let output = concat!(
            "---SESSIONS---\n",
            "workspace|3|1\n",
            "dev|1|0\n",
            "---WINDOWS---\n",
            "0|editor|2|1\n",
            "1|terminal|1|0\n",
            "---PANES---\n",
            "0|%5|80|24|0|0|1|bash|/home/user\n",
            "1|%6|40|24|0|80|0|vim|/tmp\n",
            "---END---\n"
        );

        let mut sessions = vec![];
        let mut windows = vec![];
        let mut panes = vec![];

        let mut current_section = "";
        for line in output.lines() {
            let trimmed = line.trim();
            match trimmed {
                "---SESSIONS---" => {
                    current_section = "sessions";
                    continue;
                }
                "---WINDOWS---" => {
                    current_section = "windows";
                    continue;
                }
                "---PANES---" => {
                    current_section = "panes";
                    continue;
                }
                "---END---" => break,
                _ => {}
            }
            if trimmed.is_empty() {
                continue;
            }
            match current_section {
                "sessions" => {
                    if let Some(s) = parse_session_line(trimmed) {
                        sessions.push(s);
                    }
                }
                "windows" => {
                    if let Some(w) = parse_window_line(trimmed) {
                        windows.push(w);
                    }
                }
                "panes" => {
                    if let Some(p) = parse_pane_line(trimmed) {
                        panes.push(p);
                    }
                }
                _ => {}
            }
        }

        assert_eq!(sessions.len(), 2);
        assert_eq!(windows.len(), 2);
        assert_eq!(panes.len(), 2);
        assert_eq!(sessions[0].name, "workspace");
        assert_eq!(windows[0].name, "editor");
        assert_eq!(panes[0].pane_id, "%5");
    }
}
