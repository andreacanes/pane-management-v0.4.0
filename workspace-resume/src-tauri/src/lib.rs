use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

mod commands;
mod companion;
mod models;
mod services;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::discovery::list_projects,
            commands::discovery::list_sessions,
            commands::discovery::delete_session,
            commands::discovery::check_continuity_exists,
            commands::discovery::open_directory,
            commands::discovery::create_project_folder,
            commands::discovery::get_inode,
            commands::discovery::find_inode_in_tree,
            commands::launcher::get_terminal_settings,
            commands::launcher::update_tmux_session_name,
            commands::launcher::launch_in_pane,
            commands::launcher::build_launch_command,
            commands::mac_sync::sync_project_to_mac,
            commands::mac_sync::list_remote_hosts,
            commands::mac_sync::get_remote_hosts,
            commands::mac_sync::set_remote_hosts,
            commands::mac_sync::check_remote_path_exists,
            commands::mac_sync::check_ssh_master,
            commands::mac_sync::launch_project_session_on,
            commands::usage::get_project_usage,
            commands::usage::get_all_usage,
            commands::usage::get_usage_summary,
            commands::git::get_git_info,
            commands::git::create_worktree,
            commands::companion_admin::get_companion_config,
            commands::companion_admin::get_companion_qr,
            commands::companion_admin::rotate_companion_token,
            commands::launcher::get_error_log,
            commands::launcher::clear_error_log,
            commands::tmux::list_active_claude_panes,
            commands::tmux::list_tmux_sessions,
            commands::tmux::list_tmux_sessions_on,
            commands::tmux::list_tmux_windows,
            commands::tmux::list_tmux_windows_on,
            commands::tmux::list_tmux_panes,
            commands::tmux::list_tmux_panes_on,
            commands::tmux::list_tmux_panes_all_on,
            commands::tmux::get_tmux_state,
            commands::tmux::create_pane,
            commands::tmux::create_pane_on,
            commands::tmux::apply_layout,
            commands::tmux::send_to_pane,
            commands::tmux::send_to_pane_on_host,
            commands::tmux::cancel_pane_command,
            commands::tmux::cancel_pane_command_on,
            commands::tmux::kill_pane,
            commands::tmux::kill_pane_on,
            commands::tmux::create_window,
            commands::tmux::kill_window,
            commands::tmux::swap_tmux_pane,
            commands::tmux::tmux_resurrect_save,
            commands::tmux::tmux_resurrect_restore,
            commands::tmux::swap_tmux_window,
            commands::tmux::switch_tmux_session,
            commands::tmux::select_tmux_window,
            commands::tmux::rename_session,
            commands::tmux::rename_window,
            commands::tmux::create_session,
            commands::tmux::create_session_on,
            commands::tmux::kill_session,
            commands::tmux::attach_remote_session,
            commands::tmux::setup_pane_grid,
            commands::tmux::reflow_pane_grid,
            commands::tmux::reduce_pane_grid,
            commands::tmux::list_kill_targets,
            commands::tmux::check_pane_statuses,
            commands::tmux::check_pane_statuses_on,
            commands::project_meta::get_session_order,
            commands::project_meta::set_session_order,
            commands::project_meta::get_pinned_order,
            commands::project_meta::set_pinned_order,
            commands::project_meta::get_all_project_meta,
            commands::project_meta::set_project_tier,
            commands::project_meta::set_display_name,
            commands::project_meta::set_session_binding,
            commands::project_meta::update_project_inode,
            commands::project_meta::get_pane_presets,
            commands::project_meta::save_pane_preset,
            commands::project_meta::delete_pane_preset,
            commands::project_meta::get_pane_assignments_raw,
            commands::project_meta::get_pane_assignments_full,
            commands::project_meta::get_all_pane_assignments_full,
            commands::project_meta::set_pane_assignment,
            commands::project_meta::set_pane_assignment_meta,
            commands::project_meta::get_session_names,
            commands::project_meta::set_session_names,
        ])
        .setup(|app| {
            // Register Ctrl+Space global hotkey to toggle window visibility.
            // If the string ever fails to parse (e.g. a future shortcut-plugin
            // change tightens the grammar) we log and move on rather than
            // panicking the whole setup — the app is still usable without
            // a global hotkey.
            match "ctrl+space".parse::<Shortcut>() {
                Ok(shortcut) => {
                    let handle = app.handle().clone();
                    app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
                        if event.state == ShortcutState::Pressed {
                            if let Some(window) = handle.get_webview_window("main") {
                                if window.is_minimized().unwrap_or(false) {
                                    let _ = window.unminimize();
                                    let _ = window.set_focus();
                                } else {
                                    let _ = window.minimize();
                                }
                            }
                        }
                    })?;
                }
                Err(e) => {
                    eprintln!("[lib] failed to parse Ctrl+Space shortcut, skipping hotkey: {}", e);
                }
            }

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match services::watcher::start_watcher(app_handle).await {
                    Ok(watcher) => {
                        // Keep watcher alive for the app's lifetime.
                        // std::mem::forget is acceptable for Phase 1;
                        // a cleaner approach (app.manage with State) in a later phase.
                        std::mem::forget(watcher);
                    }
                    Err(e) => eprintln!("Failed to start file watcher: {}", e),
                }
            });

            // Spawn the companion HTTP/WS service (mobile API over Tailscale).
            let app_handle_companion = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = companion::spawn(app_handle_companion).await {
                    eprintln!("Companion service error: {}", e);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
