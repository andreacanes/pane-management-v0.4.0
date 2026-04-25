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
    if encoded_project.is_empty() {
        return Err("encoded_project is required".into());
    }
    // `sync-add-project` expects the project BASENAME (e.g.
    // `akamai-v3-bestbuy`), not the Claude-encoded `.claude/projects/`
    // form (`-home-andrea-akamai-v3-bestbuy`). Encoded names start with
    // `-` which the helper's `usage: sync-add-project <project-name>
    // [source-dir]` positional parse trips over ("source dir does not
    // exist: /home/andrea/-home-andrea-..."). Resolve encoded → actual
    // path via the project scan, then take the path's basename.
    //
    // Both existing callers (PaneSlot "Sync to Mac" menu, ProjectCard
    // "Sync→Mac" button) pass `encoded_name` — the conversion lives
    // here so they need no changes. APK's /api/v1/sync-project-to-mac
    // endpoint also goes through this same command, so it benefits
    // automatically.
    let projects = crate::commands::discovery::list_projects()
        .await
        .map_err(|e| format!("failed to list projects for sync: {}", e))?;
    let project = projects
        .into_iter()
        .find(|p| p.encoded_name == encoded_project)
        .ok_or_else(|| {
            format!(
                "unknown project '{}' — not found in the Claude projects scan",
                encoded_project
            )
        })?;
    let basename = std::path::Path::new(&project.actual_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            format!(
                "cannot derive basename from project path '{}'",
                project.actual_path
            )
        })?;
    if basename.is_empty() {
        return Err(format!(
            "derived basename is empty for project '{}'",
            encoded_project
        ));
    }
    // Defense-in-depth: basename came from a filesystem scan so it's
    // already safe, but single-quote-escape anyway in case a project
    // directory ever has weird chars.
    let safe = basename.replace('\'', r"'\''");
    let script = format!("sync-add-project '{}' 2>&1", safe);
    crate::commands::tmux::run_tmux_command_async(
        format!("bash -lc {}", crate::services::host_target::ssh_shell_quote(&script)),
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
/// Start (or attach to) a per-project tmux session on a remote host
/// under the requested Claude account. Delegates to the host's `cc`
/// script — the single source of truth for remote Claude session
/// management across all frontends (desktop / APK / WSL shell).
///
/// `cc` on the Mac handles:
///   * account → session name suffixing (`<project>`, `<project>-b`,
///     `<project>-c`)
///   * `env CLAUDE_CONFIG_DIR=…` prefix for non-primary accounts
///   * mismatch detection — rejects if the target name exists but
///     runs under a different account (no silent cross-account
///     attach)
///   * headless mode — because we invoke without a TTY, cc's
///     `[ ! -t 1 ]` branch fires: it creates-if-missing, prints the
///     effective session name on stdout, exits 0
///
/// Parameters:
/// * `session_name` — project basename (cc derives the suffix by
///   itself; don't pre-suffix).
/// * `project_path` — retained for API compatibility and pre-flight
///   path checks on the caller side; not forwarded to cc because cc
///   uses `$HOME/projects/<name>` on Mac by its own convention.
/// * `account` — `"andrea"` / `"bravura"` / `"sully"`.
///
/// Returns the effective session name cc picked (suffixed as needed).
/// Window and pane indices are always `0,0` for freshly-created Mac
/// sessions — cc's single pane runs mncld.
#[tauri::command]
pub async fn launch_project_session_on(
    host: String,
    session_name: String,
    project_path: String,
    account: String,
) -> Result<(String, u32, u32), String> {
    use crate::services::host_target::HostTarget;
    let _ = project_path; // retained in API for pre-flight parity; cc derives its own DIR.
    if session_name.is_empty() {
        return Err("session_name is required".into());
    }
    let host_t = HostTarget::from_str(Some(&host));
    // Single-quote each positional so a project name with shell
    // metacharacters can't break out. cc validates the account itself
    // (invalid → non-zero exit with clear message).
    let proj_esc = session_name.replace('\'', r"'\''");
    let acct_esc = account.replace('\'', r"'\''");
    let script = format!("cc '{}' '{}'", proj_esc, acct_esc);
    let output = crate::commands::tmux::run_tmux_command_async_on(host_t, script).await?;
    // cc prints exactly one line — the effective session name — on
    // success. Any stderr ("not mirrored", "mismatch", etc.) surfaces
    // as the Err above via non-zero exit.
    let effective_session = output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .ok_or_else(|| format!("cc produced no session name (stdout: '{}')", output))?;
    Ok((effective_session, 0, 0))
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
        crate::services::host_target::ssh_shell_quote(&path),
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
        crate::services::host_target::ssh_shell_quote(&alias),
    );
    let out = crate::commands::tmux::run_tmux_command_async(script).await?;
    Ok(out.trim() == "live")
}

// Quoting now uses `services::host_target::ssh_shell_quote` directly —
// the previous local `shell_quote` duplicate was a dependency-inversion
// concern that turned out not to apply (commands/* may depend on
// services/*; the rule prohibits the reverse).
//
// check_remote_path_exists / check_ssh_master are integration-tested
// during the end-to-end verification — they're pure wrappers around a
// small shell script + the existing run_tmux_command_async helpers,
// which have their own tests.
