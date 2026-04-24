//! Tauri commands for remote-host integration (currently Mac-only).
//!
//! Four concerns live here:
//! 1. Kicking off Mutagen syncs from WSL when a pane targets a remote host
//!    for a project not yet mirrored there ([`sync_project_to_mac`]).
//! 2. Enumerating known remote hosts for the UI host-picker
//!    ([`list_remote_hosts`]).
//! 3. Pre-flight checks before enabling a remote host on a pane
//!    ([`check_remote_path_exists`]).
//! 4. SSH ControlMaster health visibility ([`check_ssh_master`]).
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

/// Return the SSH aliases of currently-supported remote hosts. Reads
/// from the Tauri store key `remote_hosts` so the user can add / remove
/// hosts via the Settings panel. Defaults to `["mac"]` when the key is
/// absent (first-run) or malformed — guarantees a Mac-ready baseline
/// without a Settings visit.
#[tauri::command]
pub async fn list_remote_hosts(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let hosts = get_remote_hosts(app).await?;
    if hosts.is_empty() {
        Ok(vec!["mac".to_string()])
    } else {
        Ok(hosts)
    }
}

/// Raw accessor: read the persisted `remote_hosts` array from the
/// Tauri store. Unlike [`list_remote_hosts`] this returns whatever is
/// stored — including an empty list — so the Settings UI can render
/// "no hosts configured" without the [`list_remote_hosts`] default
/// papering over it.
#[tauri::command]
pub async fn get_remote_hosts(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    crate::services::store::load_store_or_default::<Vec<String>>(&app, "remote_hosts")
}

/// Persist `hosts` to the Tauri store's `remote_hosts` key. Each entry
/// must be a non-empty SSH alias (no `/` or `|`) — those separators are
/// reserved by the 4-segment pane_assignment key format. Returns the
/// normalized list actually persisted (de-duplicated, trimmed).
#[tauri::command]
pub async fn set_remote_hosts(
    hosts: Vec<String>,
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in hosts {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('/') || trimmed.contains('|') {
            return Err(format!(
                "host alias '{}' contains reserved separator (/ or |) — use a clean SSH alias",
                trimmed
            ));
        }
        if trimmed == "local" {
            return Err("'local' is reserved for the WSL side and cannot be a remote alias".into());
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    crate::services::store::save_store(&app, "remote_hosts", &out)?;
    Ok(out)
}

/// Start a new detached tmux session on a remote host, targeting a
/// specific project directory with a specific Claude account, and
/// launch `mncld` inside it. Mirrors what `~/bin/cc <project> <account>`
/// does on the Mac but stays detached so SSH without a TTY still works.
///
/// Parameters:
/// * `host` — SSH alias (e.g. `"mac"`). `"local"` is accepted but the
///   `ncld` family is the more natural entry point for local launches.
/// * `session_name` — the tmux session name. By `cc` convention this
///   matches the project basename.
/// * `project_path` — absolute path on the remote host's filesystem.
///   For Mac, this is typically `/Users/admin/projects/<name>` — the
///   caller (CreatePaneModal) derives it via `toMacPath`.
/// * `account` — `"andrea"` | `"bravura"` | `"sully"`. Picks the
///   `CLAUDE_CONFIG_DIR` that mncld runs under.
///
/// Returns the created `(session, window, pane)` coord so the UI can
/// refresh and drop an assignment on the new slot.
#[tauri::command]
pub async fn launch_project_session_on(
    host: String,
    session_name: String,
    project_path: String,
    account: String,
) -> Result<(String, u32, u32), String> {
    use crate::services::host_target::HostTarget;
    if session_name.is_empty() || project_path.is_empty() {
        return Err("session_name and project_path are required".into());
    }
    // Always route through `env` — even when the account needs no
    // CLAUDE_CONFIG_DIR override. Otherwise tmux's `/bin/sh -c
    // '<shell-command>'` wrapping leaves a `zsh` / `sh` process as the
    // pane's top-of-tree, which masks the patched Claude binary from
    // `pane_is_claude` (the check looks for "claude" / "cld" in
    // `pane_current_command`). `env` is a C program that sets any
    // given vars then exec-replaces itself with the remaining argv, so
    // the chain collapses to tmux → env → mncld → cli-mncld-118.bin
    // and tmux reports `cli-mncld-118.b` as `current_command`.
    let env_prefix = match account.as_str() {
        "bravura" => "env CLAUDE_CONFIG_DIR=\"$HOME/.claude-b\" ",
        "sully" => "env CLAUDE_CONFIG_DIR=\"$HOME/.claude-c\" ",
        _ => "env ",
    };
    // Idempotent create-or-attach. Can't use `tmux new-session -A -d`:
    // when the session already exists, `-A` falls back to
    // `attach-session -d` which requires a PTY, and over SSH without
    // `-t` tmux errors out with "open terminal failed: not a terminal".
    // `has-session` returns 0 when present (then we no-op) and non-zero
    // when missing (then we create). Either path is TTY-free.
    let sess_esc = session_name.replace('\'', r"'\''");
    let path_esc = project_path.replace('\'', r"'\''");
    let script = format!(
        "tmux has-session -t '{sess}' 2>/dev/null || \
         tmux new-session -d -s '{sess}' -c '{path}' -- {env}mncld",
        sess = sess_esc,
        path = path_esc,
        env = env_prefix,
    );
    let host_t = HostTarget::from_str(Some(&host));
    crate::commands::tmux::run_tmux_command_async_on(host_t, script).await?;
    Ok((session_name, 0, 0))
}

/// Check whether `path` exists as a directory on `host`. Used by the UI
/// to gate the Host=Mac dropdown: if the project hasn't been mirrored to
/// the Mac yet, the launch would silently fail with "no such file or
/// directory" inside the pane. Better to disable the option up front
/// with a tooltip.
///
/// The trick: run `test -d <path> && echo yes || echo no` so the exit
/// status is always 0 and our caller can read the result from stdout
/// without muddling genuine SSH/transport errors (which still surface
/// as `Err`).
///
/// `host` accepts the same values as [`HostTarget::from_str`]:
/// `"local"` / `""` / `None` → local WSL, anything else → SSH alias.
#[tauri::command]
pub async fn check_remote_path_exists(
    host: String,
    path: String,
) -> Result<bool, String> {
    use crate::services::host_target::HostTarget;
    let script = format!(
        "test -d {} && echo yes || echo no",
        shell_quote(&path),
    );
    let host_t = HostTarget::from_str(Some(&host));
    let out = crate::commands::tmux::run_tmux_command_async_on(host_t, script).await?;
    Ok(out.trim() == "yes")
}

/// Poll the OpenSSH ControlMaster for `alias` and return whether the
/// multiplexed socket is live. Uses `ssh -O check` which exits 0 when
/// the master is running and non-zero otherwise — we translate to a
/// bool and swallow the stderr so a dead master isn't reported as a
/// hard error up the Tauri bridge.
///
/// The frontend polls this every ~15 s to show a health dot next to
/// the host dropdown. A dead master means the *next* remote tmux call
/// will pay the full SSH handshake cost (~200-500 ms) rather than the
/// multiplexed ~10-30 ms, so this is a soft indicator, not a blocker.
#[tauri::command]
pub async fn check_ssh_master(alias: String) -> Result<bool, String> {
    let script = format!(
        "ssh -O check {} >/dev/null 2>&1 && echo live || echo dead",
        shell_quote(&alias),
    );
    let out = crate::commands::tmux::run_tmux_command_async(script).await?;
    Ok(out.trim() == "live")
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

    // check_remote_path_exists / check_ssh_master are integration-tested by
    // running the real commands during the end-to-end verification — they
    // are pure wrappers around a small shell script + the existing
    // run_tmux_command_async helpers, which have their own tests.
}
