//! Pane-management companion service.
//!
//! An embedded axum HTTP+WebSocket server that exposes tmux panes and
//! Claude Code state to a mobile companion app over Tailscale. Runs as
//! a tokio task spawned from the Tauri `setup()` closure.
//!
//! Port: 8833 (fixed).
//! Bind: `0.0.0.0` in dev so ADB reverse-port-forward works from a
//! USB-connected phone; production can restrict to the Tailscale IP.
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

pub mod auth;
pub mod error;
pub mod hook_sink;
pub mod http;
pub mod models;
pub mod ntfy_server;
pub mod state;
pub mod tmux_poller;
pub mod ws;

use tauri::AppHandle;

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

    let state = state::AppState::load_or_init(&app).await?;
    let bind = format!("{}:{}", BIND_ADDR, COMPANION_PORT);
    tracing::info!(%bind, "starting companion service");

    // Background workers
    let poller_state = state.clone();
    tokio::spawn(async move {
        tmux_poller::run(poller_state).await;
    });

    // Serve HTTP
    let app_router = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("companion listening on http://{bind}");
    axum::serve(listener, app_router).await?;
    Ok(())
}
