use crate::models::settings::{ErrorLogEntry, TerminalSettings};
use crate::services::store::{load_store_or_default, save_store};

// ---------------------
// IPC Commands
// ---------------------

#[tauri::command]
pub async fn get_terminal_settings(
    app: tauri::AppHandle,
) -> Result<TerminalSettings, String> {
    load_terminal_settings(&app)
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

    let settings = TerminalSettings {
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
    load_store_or_default(app, "terminal_settings")
}

fn save_terminal_settings(
    app: &tauri::AppHandle,
    settings: &TerminalSettings,
) -> Result<(), String> {
    save_store(app, "terminal_settings", settings)
}

fn load_error_log(app: &tauri::AppHandle) -> Result<Vec<ErrorLogEntry>, String> {
    load_store_or_default(app, "error_log")
}

fn save_error_log(
    app: &tauri::AppHandle,
    log: &Vec<ErrorLogEntry>,
) -> Result<(), String> {
    save_store(app, "error_log", log)
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

