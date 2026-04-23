//! Tauri commands for Mac-side project integration.
//!
//! Two concerns live here:
//! 1. Kicking off Mutagen syncs from WSL when a pane targets the Mac for
//!    a project not yet mirrored there ([`sync_project_to_mac`]).
//! 2. Enumerating known remote hosts for the UI host-picker
//!    ([`list_remote_hosts`]).
//!
//! Error dialect: `Result<T, String>` per `.claude/rules/error-handling-dialects.md`.

/// Spawn `sync-add-project <encoded_project>` inside WSL so Mutagen
/// starts mirroring `~/<project>` ↔ `mac:/Users/admin/projects/<project>`.
///
/// The helper script lives at `/home/andrea/bin/sync-add-project`
/// (see `mac_studio_bridge` memory) and is idempotent — re-running for
/// an already-synced project is a no-op. Returns the helper's combined
/// stdout so the UI can display the outcome ("creating session",
/// "already exists", etc.).
///
/// Relies on `bash -lc` so PATH includes `~/bin` via the user's login
/// shell profile; a bare `bash -c` would miss it on fresh shells.
#[tauri::command]
pub async fn sync_project_to_mac(encoded_project: String) -> Result<String, String> {
    // Basic sanitization: encoded_project is passed straight to a shell
    // arg so we single-quote it and escape embedded singles. The typical
    // encoded_project format is a safe subset (alphanum + dashes), but
    // defense-in-depth against pathological project names is cheap.
    let safe = encoded_project.replace('\'', r"'\''");
    let script = format!("sync-add-project '{}' 2>&1", safe);
    crate::commands::tmux::run_tmux_command_async(
        format!("bash -lc {}", shell_quote(&script)),
    )
    .await
}

/// Return the SSH aliases of currently-supported remote hosts. MVP
/// returns a static `["mac"]`; future work will read this from user
/// settings so additional hosts (a Linux box, a second Mac) can be
/// added without a code change.
#[tauri::command]
pub async fn list_remote_hosts() -> Result<Vec<String>, String> {
    // TODO(follow-up): read from a `remote_hosts` Tauri store key so
    // users can register additional SSH aliases without rebuilding.
    Ok(vec!["mac".to_string()])
}

/// Single-quote a bash script fragment for safe nesting inside another
/// bash `-c` invocation. Mirrors [`crate::services::host_target::ssh_shell_quote`]
/// but kept local to avoid a dependency inversion — this module is a
/// `commands/*` leaf, the other is `services/*`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_plain() {
        assert_eq!(shell_quote("foo"), "'foo'");
    }

    #[test]
    fn shell_quote_with_single_quote() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }
}
