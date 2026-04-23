//! Pure assembly of the shell command that launches Claude inside a
//! tmux pane. Takes structured parameters (host, account, project_path,
//! resume session id, yolo flag) and returns a single bash-friendly
//! string suitable for piping into `tmux send-keys`.
//!
//! The output is identical for local and remote — the remote transport
//! wrapper (`ssh <alias> -- 'tmux send-keys ... "<CMD>" Enter'`) lives
//! one layer up in `commands::tmux::send_to_pane_on`. This module does
//! not know or care whether the string will run in WSL or on the Mac;
//! environment-variable expansion (`$HOME`) happens inside whichever
//! shell tmux forks, so `$HOME/.claude-b` resolves correctly on either
//! side.

use crate::services::host_target::HostTarget;

/// Inputs to [`build_launch_command`]. Borrows everywhere — nothing
/// allocates except the returned string.
pub struct LaunchParams<'a> {
    pub host: &'a HostTarget,
    /// `"andrea"` | `"bravura"` | `"sully"` — the single source-of-truth
    /// identity vocabulary from `companion::accounts`. Unknown values
    /// fall through to the no-env-prefix path (treated as primary).
    pub account: &'a str,
    /// POSIX path to `cd` into before launching. WSL paths use
    /// `/home/...`; Mac paths use `/Users/admin/projects/...`.
    pub project_path: &'a str,
    /// Claude session UUID for `-r <sid>`. `None` starts a new session.
    pub resume_sid: Option<&'a str>,
    pub yolo: bool,
}

/// Build the full `cd "<path>" && [env_prefix]{ncld|mcld}[ -r sid][ --yolo]` string.
pub fn build_launch_command(p: &LaunchParams) -> String {
    let base = match p.host {
        HostTarget::Local => "ncld",
        HostTarget::Remote { .. } => "mcld",
    };

    // Config-dir selection. Map from identity key → env prefix. The
    // mapping mirrors `companion::accounts::ACCOUNTS.config_dir`; we
    // inline it here rather than depending on that registry to keep
    // this module a pure leaf with no cross-module coupling beyond
    // `HostTarget`.
    let env_prefix = match p.account {
        "bravura" => "CLAUDE_CONFIG_DIR=\"$HOME/.claude-b\" ",
        "sully" => "CLAUDE_CONFIG_DIR=\"$HOME/.claude-c\" ",
        _ => "",
    };

    let resume = match p.resume_sid {
        Some(sid) => format!(" -r {}", sid),
        None => String::new(),
    };

    let yolo = if p.yolo { " --dangerously-skip-permissions" } else { "" };

    format!(
        "cd \"{path}\" && {prefix}{base}{resume}{yolo}",
        path = p.project_path,
        prefix = env_prefix,
        base = base,
        resume = resume,
        yolo = yolo,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> HostTarget {
        HostTarget::Remote { alias: "mac".into() }
    }

    // ---------- Local × 3 accounts × {new, resume} = 6 cases ----------

    #[test]
    fn local_andrea_new() {
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "andrea",
            project_path: "/home/andrea/foo",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(s, r#"cd "/home/andrea/foo" && ncld"#);
    }

    #[test]
    fn local_andrea_resume() {
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "andrea",
            project_path: "/home/andrea/foo",
            resume_sid: Some("abc-123"),
            yolo: false,
        });
        assert_eq!(s, r#"cd "/home/andrea/foo" && ncld -r abc-123"#);
    }

    #[test]
    fn local_bravura_new() {
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "bravura",
            project_path: "/home/andrea/foo",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/home/andrea/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-b" ncld"#
        );
    }

    #[test]
    fn local_bravura_resume_yolo() {
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "bravura",
            project_path: "/home/andrea/foo",
            resume_sid: Some("abc-123"),
            yolo: true,
        });
        assert_eq!(
            s,
            r#"cd "/home/andrea/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-b" ncld -r abc-123 --dangerously-skip-permissions"#
        );
    }

    #[test]
    fn local_sully_new() {
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "sully",
            project_path: "/home/andrea/foo",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/home/andrea/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-c" ncld"#
        );
    }

    #[test]
    fn local_sully_resume() {
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "sully",
            project_path: "/home/andrea/foo",
            resume_sid: Some("xyz-789"),
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/home/andrea/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-c" ncld -r xyz-789"#
        );
    }

    // ---------- Mac × 3 accounts × {new, resume} = 6 cases ----------

    #[test]
    fn mac_andrea_new() {
        let s = build_launch_command(&LaunchParams {
            host: &mac(),
            account: "andrea",
            project_path: "/Users/admin/projects/foo",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(s, r#"cd "/Users/admin/projects/foo" && mcld"#);
    }

    #[test]
    fn mac_andrea_resume_yolo() {
        let s = build_launch_command(&LaunchParams {
            host: &mac(),
            account: "andrea",
            project_path: "/Users/admin/projects/foo",
            resume_sid: Some("abc-123"),
            yolo: true,
        });
        assert_eq!(
            s,
            r#"cd "/Users/admin/projects/foo" && mcld -r abc-123 --dangerously-skip-permissions"#
        );
    }

    #[test]
    fn mac_bravura_new() {
        let s = build_launch_command(&LaunchParams {
            host: &mac(),
            account: "bravura",
            project_path: "/Users/admin/projects/foo",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/Users/admin/projects/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-b" mcld"#
        );
    }

    #[test]
    fn mac_bravura_resume() {
        let s = build_launch_command(&LaunchParams {
            host: &mac(),
            account: "bravura",
            project_path: "/Users/admin/projects/foo",
            resume_sid: Some("abc-123"),
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/Users/admin/projects/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-b" mcld -r abc-123"#
        );
    }

    #[test]
    fn mac_sully_new() {
        let s = build_launch_command(&LaunchParams {
            host: &mac(),
            account: "sully",
            project_path: "/Users/admin/projects/foo",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/Users/admin/projects/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-c" mcld"#
        );
    }

    #[test]
    fn mac_sully_resume() {
        let s = build_launch_command(&LaunchParams {
            host: &mac(),
            account: "sully",
            project_path: "/Users/admin/projects/foo",
            resume_sid: Some("xyz-789"),
            yolo: false,
        });
        assert_eq!(
            s,
            r#"cd "/Users/admin/projects/foo" && CLAUDE_CONFIG_DIR="$HOME/.claude-c" mcld -r xyz-789"#
        );
    }

    // ---------- Edge cases ----------

    #[test]
    fn unknown_account_treated_as_primary() {
        // Defensive: an unexpected account string (e.g. from a pre-change
        // assignment or a typo) should still produce a runnable command
        // using the default config dir.
        let s = build_launch_command(&LaunchParams {
            host: &HostTarget::Local,
            account: "made-up",
            project_path: "/tmp/x",
            resume_sid: None,
            yolo: false,
        });
        assert_eq!(s, r#"cd "/tmp/x" && ncld"#);
    }
}
