use std::collections::HashMap;
use std::process::Child;
use std::sync::Mutex;

use serde::Serialize;

use crate::models::settings::{
    ErrorLogEntry, ResumeResult, TerminalBackend, TerminalSettings,
};
use crate::services::terminal;

/// Tracks active terminal sessions launched by the app.
/// Managed via `app.manage()` in lib.rs.
pub struct SessionTracker {
    active: Mutex<HashMap<String, TrackedSession>>,
}

struct TrackedSession {
    pid: Option<u32>,
    child: Option<Child>,
    terminal: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveSession {
    pub session_id: String,
    pub pid: Option<u32>,
    pub terminal: String,
    pub is_alive: bool,
}

impl SessionTracker {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
        }
    }

    pub fn track(&self, session_id: String, pid: Option<u32>, child: Option<Child>, terminal: String) {
        let mut active = self.active.lock().unwrap();
        active.insert(session_id, TrackedSession { pid, child, terminal });
    }

    pub fn get_active(&self) -> Vec<ActiveSession> {
        let mut active = self.active.lock().unwrap();
        let mut result = Vec::new();
        let mut dead_keys = Vec::new();

        for (session_id, tracked) in active.iter_mut() {
            let is_alive = check_alive(tracked);
            if !is_alive {
                dead_keys.push(session_id.clone());
            } else {
                result.push(ActiveSession {
                    session_id: session_id.clone(),
                    pid: tracked.pid,
                    terminal: tracked.terminal.clone(),
                    is_alive,
                });
            }
        }

        // Clean up dead entries
        for key in dead_keys {
            active.remove(&key);
        }

        result
    }
}

fn check_alive(tracked: &mut TrackedSession) -> bool {
    // If we own the Child handle, use try_wait
    if let Some(ref mut child) = tracked.child {
        match child.try_wait() {
            Ok(Some(_)) => return false, // Process exited
            Ok(None) => return true,     // Still running
            Err(_) => return false,      // Error checking
        }
    }

    // For URI-launched processes (Warp), check PID via Win32
    if let Some(pid) = tracked.pid {
        return is_pid_alive_check(pid);
    }

    // No PID and no Child -- can't determine, assume dead
    false
}

#[cfg(windows)]
fn is_pid_alive_check(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::Foundation::CloseHandle;

    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(not(windows))]
fn is_pid_alive_check(_pid: u32) -> bool {
    false
}

// ---------------------
// IPC Commands
// ---------------------

#[tauri::command]
pub async fn resume_session(
    encoded_project: String,
    session_id: String,
    project_path: String,
    state: tauri::State<'_, SessionTracker>,
    app: tauri::AppHandle,
) -> Result<ResumeResult, String> {
    // Read terminal settings from store
    let settings = load_terminal_settings(&app)?;

    // Create launcher for the selected backend (passes tmux_session_name for tmux)
    let launcher = terminal::create_launcher_from_settings(&settings);

    // If selected launcher isn't available, try fallbacks in order: tmux → powershell
    let (launcher, _fallback_used) = if !launcher.is_available() {
        let fallback_order = match settings.backend {
            TerminalBackend::Tmux => vec![TerminalBackend::Powershell],
            TerminalBackend::Warp => vec![TerminalBackend::Tmux, TerminalBackend::Powershell],
            TerminalBackend::Powershell => vec![TerminalBackend::Tmux],
        };
        let mut found = None;
        for fb in fallback_order {
            let mut fallback_settings = settings.clone();
            fallback_settings.backend = fb;
            let fallback = terminal::create_launcher_from_settings(&fallback_settings);
            if fallback.is_available() {
                found = Some(fallback);
                break;
            }
        }
        match found {
            Some(fb) => (fb, true),
            None => return Err("No terminal backend is available. Install WSL/tmux or ensure PowerShell is accessible.".into()),
        }
    } else {
        (launcher, false)
    };

    // Build the claude command
    let command = if session_id.is_empty() {
        "claude -r".to_string()
    } else {
        format!("claude -r {}", session_id)
    };

    // Launch the terminal
    match launcher.launch(&project_path, Some(&command)) {
        Ok(result) => {
            let resume_result = ResumeResult {
                pid: result.pid,
                terminal: launcher.name().to_string(),
                session_id: if session_id.is_empty() {
                    encoded_project.clone()
                } else {
                    session_id.clone()
                },
            };

            // Track the session
            let track_id = if session_id.is_empty() {
                format!("{}:latest", encoded_project)
            } else {
                format!("{}:{}", encoded_project, session_id)
            };
            state.track(track_id, result.pid, result.child, launcher.name().to_string());

            Ok(resume_result)
        }
        Err(e) => {
            // Log error to store
            let error_entry = ErrorLogEntry {
                timestamp: chrono_now(),
                terminal: launcher.name().to_string(),
                error: e.to_string(),
                project_path: project_path.clone(),
            };
            let _ = append_error_log(&app, error_entry);

            Err(format!("Failed to launch terminal: {}", e))
        }
    }
}

#[tauri::command]
pub async fn get_active_sessions(
    state: tauri::State<'_, SessionTracker>,
) -> Result<Vec<ActiveSession>, String> {
    Ok(state.get_active())
}

#[tauri::command]
pub async fn get_terminal_settings(
    app: tauri::AppHandle,
) -> Result<TerminalSettings, String> {
    load_terminal_settings(&app)
}

#[tauri::command]
pub async fn update_terminal_settings(
    backend: String,
    app: tauri::AppHandle,
) -> Result<TerminalSettings, String> {
    let backend_enum: TerminalBackend = serde_json::from_value(
        serde_json::Value::String(backend),
    )
    .map_err(|e| format!("Invalid backend value: {}", e))?;

    // Preserve existing tmux_session_name when only changing backend
    let existing = load_terminal_settings(&app).unwrap_or_default();
    let settings = TerminalSettings {
        backend: backend_enum,
        tmux_session_name: existing.tmux_session_name,
    };

    save_terminal_settings(&app, &settings)?;
    Ok(settings)
}

#[tauri::command]
pub async fn update_tmux_session_name(
    session_name: String,
    app: tauri::AppHandle,
) -> Result<TerminalSettings, String> {
    let trimmed = session_name.trim().to_string();
    if trimmed.is_empty() {
        return Err("Session name cannot be empty".into());
    }
    // tmux session names can't contain dots, colons, or whitespace
    if trimmed.contains('.') || trimmed.contains(':') || trimmed.contains(char::is_whitespace) {
        return Err("Session name cannot contain dots, colons, or whitespace".into());
    }

    let existing = load_terminal_settings(&app).unwrap_or_default();
    let settings = TerminalSettings {
        backend: existing.backend,
        tmux_session_name: trimmed,
    };

    save_terminal_settings(&app, &settings)?;
    Ok(settings)
}

#[tauri::command]
pub async fn get_error_log(
    app: tauri::AppHandle,
) -> Result<Vec<ErrorLogEntry>, String> {
    load_error_log(&app)
}

#[tauri::command]
pub async fn clear_error_log(
    app: tauri::AppHandle,
) -> Result<(), String> {
    save_error_log(&app, &Vec::<ErrorLogEntry>::new())
}

// ---------------------
// Store helpers
// ---------------------

fn load_terminal_settings(app: &tauri::AppHandle) -> Result<TerminalSettings, String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("terminal_settings") {
        Some(value) => {
            serde_json::from_value::<TerminalSettings>(value.clone())
                .map_err(|e| format!("Failed to parse terminal settings: {}", e))
        }
        None => Ok(TerminalSettings::default()),
    }
}

fn save_terminal_settings(
    app: &tauri::AppHandle,
    settings: &TerminalSettings,
) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    store.set("terminal_settings", value);
    Ok(())
}

fn load_error_log(app: &tauri::AppHandle) -> Result<Vec<ErrorLogEntry>, String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("error_log") {
        Some(value) => {
            serde_json::from_value::<Vec<ErrorLogEntry>>(value.clone())
                .map_err(|e| format!("Failed to parse error log: {}", e))
        }
        None => Ok(vec![]),
    }
}

fn save_error_log(
    app: &tauri::AppHandle,
    log: &Vec<ErrorLogEntry>,
) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(log)
        .map_err(|e| format!("Failed to serialize error log: {}", e))?;

    store.set("error_log", value);
    Ok(())
}

fn append_error_log(
    app: &tauri::AppHandle,
    entry: ErrorLogEntry,
) -> Result<(), String> {
    let mut log = load_error_log(app)?;
    log.push(entry);
    // Keep last 100 entries
    if log.len() > 100 {
        log = log.split_off(log.len() - 100);
    }
    save_error_log(app, &log)
}

/// Simple timestamp without pulling in chrono crate
fn chrono_now() -> String {
    // Use std::time for a basic ISO-ish timestamp
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Convert to a readable format (basic, no chrono dependency)
    format!("{}Z", secs)
}

// ---------------------
// Host-aware pane launching (Mac Studio integration)
// ---------------------

/// Assemble the Claude launch command for a given host/account/project.
/// Exposed primarily as a debug / test surface — the frontend typically
/// goes straight to [`launch_in_pane`] which bundles the assembly plus
/// the tmux send-keys side effect. Pure; no side effects.
#[tauri::command]
pub async fn build_launch_command(
    host: String,
    account: String,
    project_path: String,
    resume_sid: Option<String>,
    yolo: bool,
) -> Result<String, String> {
    let host_target = crate::services::host_target::HostTarget::from_str(Some(&host));
    let params = crate::services::launch_cmd::LaunchParams {
        host: &host_target,
        account: &account,
        project_path: &project_path,
        resume_sid: resume_sid.as_deref(),
        yolo,
    };
    Ok(crate::services::launch_cmd::build_launch_command(&params))
}

/// Launch (or re-launch) Claude inside a tmux pane on a specific host
/// under a specific account. Replaces the frontend's previous pattern
/// of building the `cd ... && ncld -r ...` string in TypeScript and
/// calling `send_to_pane` — all the vocab knowledge now lives in
/// `services::launch_cmd::build_launch_command` and the host dispatch
/// lives in `commands::tmux::send_to_pane_on`.
///
/// For `host = "mac"`, the final shell command is sent via
/// `ssh mac -- tmux send-keys ...`, so the resolved tmux pane lives
/// on the Mac. `$HOME` expansion happens inside the pane's own shell
/// — `CLAUDE_CONFIG_DIR="$HOME/.claude-c"` resolves to
/// `/Users/admin/.claude-c` on the Mac, not the calling Windows/WSL
/// user's home.
#[tauri::command]
pub async fn launch_in_pane(
    session: String,
    window: u32,
    pane: u32,
    host: String,
    account: String,
    project_path: String,
    resume_sid: Option<String>,
    yolo: bool,
) -> Result<(), String> {
    let host_target = crate::services::host_target::HostTarget::from_str(Some(&host));
    let params = crate::services::launch_cmd::LaunchParams {
        host: &host_target,
        account: &account,
        project_path: &project_path,
        resume_sid: resume_sid.as_deref(),
        yolo,
    };
    let command = crate::services::launch_cmd::build_launch_command(&params);
    crate::commands::tmux::send_to_pane_on(
        &host_target,
        &session,
        window,
        pane,
        &command,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_tracker_new_is_empty() {
        let tracker = SessionTracker::new();
        let active = tracker.get_active();
        assert!(active.is_empty());
    }

    #[test]
    fn test_session_tracker_track_and_retrieve() {
        let tracker = SessionTracker::new();
        // Track a session with no child handle (simulates URI-launched)
        tracker.track(
            "test-session".to_string(),
            None,
            None,
            "Warp".to_string(),
        );

        // Since there's no PID and no child, it should be cleaned up as dead
        let active = tracker.get_active();
        // With no PID and no child, check_alive returns false, so it gets cleaned up
        assert!(active.is_empty());
    }

    #[test]
    fn test_session_tracker_multiple_sessions() {
        let tracker = SessionTracker::new();
        tracker.track("session-1".to_string(), None, None, "Warp".to_string());
        tracker.track("session-2".to_string(), None, None, "PowerShell".to_string());

        // Both have no PID/child, so both cleaned up
        let active = tracker.get_active();
        assert!(active.is_empty());
    }

    #[test]
    fn test_active_session_serialize() {
        let session = ActiveSession {
            session_id: "test".to_string(),
            pid: Some(1234),
            terminal: "Warp".to_string(),
            is_alive: true,
        };
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("\"session_id\":\"test\""));
        assert!(json.contains("\"pid\":1234"));
        assert!(json.contains("\"terminal\":\"Warp\""));
        assert!(json.contains("\"is_alive\":true"));
    }

    #[test]
    fn test_chrono_now_returns_string() {
        let ts = chrono_now();
        assert!(ts.ends_with('Z'));
        // Should be a numeric timestamp
        let numeric_part = &ts[..ts.len() - 1];
        assert!(numeric_part.parse::<u64>().is_ok());
    }
}
