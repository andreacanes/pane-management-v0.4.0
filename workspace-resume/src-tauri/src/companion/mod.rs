//! Pane-management companion service.
//!
//! An embedded axum HTTP+WebSocket server that exposes tmux panes and
//! Claude Code state to a mobile companion app over Tailscale. Runs as
//! a tokio task spawned from the Tauri `setup()` closure.
//!
//! Port: 8833 (fixed).
//! Bind: `0.0.0.0` always. The companion needs to accept connections
//! from (a) WSL via the Tailscale IP, (b) the Android phone via the
//! Tailscale IP, and (c) `adb reverse` for dev. The security boundary
//! is the bearer token (256-bit, constant-time compare in `auth.rs`)
//! plus the Windows Firewall — *not* the bind address. Don't switch
//! to `127.0.0.1` without confirming Tailscale-side reachability.
//!
//! Architecture:
//! - `http` — axum Router assembly + AppState
//! - `ws` — WebSocket broadcast for live pane/approval events
//! - `auth` — bearer-token middleware + hook-secret middleware
//! - `state` — shared in-memory state machine
//! - `models` — DTOs shared between HTTP handlers and tmux_poller/hook_sink
//! - `error` — AppError + IntoResponse
//! - `hook_sink` — Claude Code Notification hook receiver (park-and-wait)
//! - `ntfy_server` — embedded ntfy-compatible endpoint so the ntfy F-Droid
//!   app can subscribe for lock-screen push notifications
//! - `tmux_poller` — 2s loop scraping tmux state into `AppState.panes`

pub mod accounts;
pub mod audit_log;
pub mod auth;
pub mod error;
pub mod hook_sink;
pub mod http;
pub mod models;
pub mod ntfy_server;
pub mod pane_log;
pub mod rate_limit_poller;
pub mod state;
pub mod tmux_poller;
pub mod ws;

use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub const COMPANION_PORT: u16 = 8833;
pub const BIND_ADDR: &str = "0.0.0.0";

/// Spawn the companion service. Called from Tauri `setup()` via
/// `tauri::async_runtime::spawn(companion::spawn(app_handle))`.
/// Never returns while the app is running.
pub async fn spawn(app: AppHandle) -> anyhow::Result<()> {
    // Initialize a tracing subscriber once — noisy-ok since this is dev
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,companion=debug".into()),
        )
        .try_init();

    let mut state = state::AppState::load_or_init(&app).await?;

    // Ensure the pane-log directory exists. pipe-pane on each Claude pane
    // will write its raw terminal stream here; `/capture` reads from these
    // files as an authoritative "full history" source that survives
    // Claude Code's periodic `\x1b[3J` (erase scrollback) redraws.
    if let Err(e) = pane_log::ensure_log_dir() {
        tracing::warn!("failed to create pane-log dir: {e} — falling back to capture-pane only");
    }

    // Close any pipe-panes that may be attached from a prior companion
    // process — their target paths may be stale (old naming scheme) and we
    // want the poll loop to re-attach using the current session-keyed
    // scheme. Safe no-op if no pipes are currently active.
    //
    // We do NOT close pipes that THIS process already set up (first boot
    // per tmux server), but on every companion restart the pipes set up by
    // the previous companion need to be redirected.
    let close_all_pipes = "tmux list-panes -a -F '#{pane_id}' 2>/dev/null \
        | xargs -I{} tmux pipe-pane -t {} 2>/dev/null; true";
    if let Err(e) =
        crate::commands::tmux::run_tmux_command_async(close_all_pipes.to_string()).await
    {
        tracing::debug!("pre-boot pipe cleanup failed: {e}");
    }

    // Spin up the notification audit log writer in the Tauri app-data
    // directory (same root the store plugin uses for settings.json). On
    // Windows this resolves to Roaming\<identifier>; on macOS and Linux
    // it resolves to the platform-appropriate app-data root. If path
    // resolution fails or the directory doesn't exist yet (first launch
    // before the store has saved anything), audit logging stays off.
    match app.path().app_data_dir() {
        Ok(audit_dir) if audit_dir.exists() => {
            let audit = audit_log::AuditLog::spawn(audit_dir.clone());
            state.audit = Some(Arc::new(audit));
            state.audit_data_dir = Some(audit_dir);
        }
        Ok(audit_dir) => {
            tracing::debug!(
                dir = %audit_dir.display(),
                "app_data_dir does not exist yet — audit log will stay disabled until the store writes first"
            );
        }
        Err(e) => {
            tracing::warn!("failed to resolve app_data_dir — audit log disabled: {e}");
        }
    }

    // Register as Tauri managed state so commands (e.g. rotate_companion_token)
    // can reach the runtime bearer and ntfy backlog.
    app.manage(state.clone());
    let bind = format!("{}:{}", BIND_ADDR, COMPANION_PORT);
    tracing::info!(
        %bind,
        "starting companion service (bound to all interfaces; bearer + Windows Firewall are the security boundary)"
    );

    // Background workers
    let poller_state = state.clone();
    tokio::spawn(async move {
        tmux_poller::run(poller_state).await;
    });

    let rl_state = state.clone();
    tokio::spawn(async move {
        rate_limit_poller::run(rl_state).await;
    });

    // Serve HTTP
    let app_router = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("companion listening on http://{bind}");
    axum::serve(listener, app_router).await?;
    Ok(())
}
