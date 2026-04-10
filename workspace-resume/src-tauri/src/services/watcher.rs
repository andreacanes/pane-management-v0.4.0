use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use tauri::Emitter;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

/// Start a file watcher on ~/.claude/projects/ that emits "session-changed"
/// Tauri events whenever .jsonl files are created or modified.
///
/// Events are debounced: changes are collected for 3 seconds of quiet before
/// a batch is emitted. This prevents event floods during active Claude sessions.
///
/// The returned watcher handle MUST be kept alive (not dropped) or watching stops.
pub async fn start_watcher(
    app_handle: tauri::AppHandle,
) -> Result<RecommendedWatcher, String> {
    let (tx, mut rx) = mpsc::channel::<Event>(256);

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| format!("Failed to create watcher: {}", e))?;

    // Watch WSL home projects (primary — where active sessions live)
    let wsl_projects = crate::services::wsl::wsl_info().map(|info| info.claude_projects_unc());
    let mut wsl_watched = false;
    if let Some(ref path) = wsl_projects {
        if path.exists() {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|e| format!("Failed to watch WSL directory: {}", e))?;
            wsl_watched = true;
        }
    }

    // Also watch Windows home projects (secondary — legacy sessions)
    let win_projects = dirs::home_dir()
        .ok_or("Cannot find home directory")?
        .join(".claude")
        .join("projects");
    let mut win_watched = false;
    if win_projects.exists() {
        watcher
            .watch(&win_projects, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to watch Windows directory: {}", e))?;
        win_watched = true;
    }

    if !wsl_watched && !win_watched {
        eprintln!("Warning: no Claude projects directory found to watch");
    }

    // Debounced event processing task
    tokio::spawn(async move {
        let mut pending: HashSet<String> = HashSet::new();
        let mut last_event = Instant::now();
        let debounce_duration = Duration::from_secs(3);

        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Some(e) => {
                            for path in &e.paths {
                                if path.extension().map(|ext| ext == "jsonl").unwrap_or(false) {
                                    pending.insert(path.to_string_lossy().to_string());
                                }
                            }
                            last_event = Instant::now();
                        }
                        None => break, // Channel closed, watcher was dropped
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if !pending.is_empty() && last_event.elapsed() >= debounce_duration {
                        let paths: Vec<String> = pending.drain().collect();
                        if let Err(e) = app_handle.emit("session-changed", &paths) {
                            eprintln!("Failed to emit session-changed event: {}", e);
                        }
                    }
                }
            }
        }
    });

    Ok(watcher)
}
