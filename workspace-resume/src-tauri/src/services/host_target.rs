//! Host abstraction for tmux invocations.
//!
//! Every tmux command ultimately runs on *some* machine. Pre-Mac
//! integration that machine was always local WSL. [`HostTarget`] makes
//! the choice explicit so the same command builders can route to either
//! `wsl.exe -e bash -c "<tmux ...>"` (Local) or
//! `wsl.exe -e bash -c "ssh <alias> -- '<tmux ...>'"` (Remote). SSH is
//! still invoked through the WSL bash envelope so the user's existing
//! `~/.ssh/config`, Tailscale MagicDNS, and `id_ed25519_*` key all work
//! without any Windows-side duplication.

/// Where a tmux command should execute.
///
/// * `Local` — current WSL tmux. The default for every call site that
///   hasn't been opted into the host-aware path.
/// * `Remote { alias }` — SSH to the named host. `alias` is a key
///   resolved by OpenSSH (`~/.ssh/config`), not a raw hostname.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HostTarget {
    Local,
    Remote { alias: String },
}

impl HostTarget {
    /// Parse from the `host` string stored on a pane assignment.
    /// Unknown / empty / `"local"` all map to [`HostTarget::Local`] so
    /// a malformed config never breaks launching — it falls back to
    /// the pre-change behaviour.
    pub fn from_str(s: Option<&str>) -> Self {
        match s {
            None | Some("") | Some("local") => Self::Local,
            Some(alias) => Self::Remote {
                alias: alias.to_string(),
            },
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// Symmetric complement of `is_local` — kept as part of the public
    /// surface even though most call sites use `!is_local()` today so
    /// callers reading the host discriminant have an affirmative form.
    #[allow(dead_code)]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    /// Value for wire fields (`PaneDto.host`, pane_assignment `host`).
    pub fn wire_str(&self) -> &str {
        match self {
            Self::Local => "local",
            Self::Remote { alias } => alias,
        }
    }
}

/// Wrap a bash script for safe inclusion in a single `ssh <alias> -- <quoted>`
/// argument. Wraps in single quotes and escapes any embedded single
/// quotes using the standard `'\''` trick.
///
/// Only used when the transport is SSH — `wsl.exe -e bash -c` handles
/// its own argv quoting and must not be double-wrapped.
pub fn ssh_shell_quote(script: &str) -> String {
    format!("'{}'", script.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_none_is_local() {
        assert_eq!(HostTarget::from_str(None), HostTarget::Local);
    }

    #[test]
    fn parse_empty_is_local() {
        assert_eq!(HostTarget::from_str(Some("")), HostTarget::Local);
    }

    #[test]
    fn parse_local_is_local() {
        assert_eq!(HostTarget::from_str(Some("local")), HostTarget::Local);
    }

    #[test]
    fn parse_alias_is_remote() {
        assert_eq!(
            HostTarget::from_str(Some("mac")),
            HostTarget::Remote { alias: "mac".into() }
        );
    }

    #[test]
    fn wire_str_round_trip() {
        assert_eq!(HostTarget::Local.wire_str(), "local");
        assert_eq!(
            HostTarget::Remote { alias: "mac".into() }.wire_str(),
            "mac"
        );
    }

    #[test]
    fn quote_plain_script() {
        assert_eq!(ssh_shell_quote("tmux list-panes"), "'tmux list-panes'");
    }

    #[test]
    fn quote_script_with_single_quotes() {
        // Embedded ' becomes '\'' — close quote, escaped literal, reopen.
        let out = ssh_shell_quote("echo 'hi'");
        assert_eq!(out, r"'echo '\''hi'\'''");
    }

    #[test]
    fn quote_empty_string() {
        assert_eq!(ssh_shell_quote(""), "''");
    }

    #[test]
    fn quote_script_with_many_quotes() {
        let out = ssh_shell_quote("'a' 'b'");
        assert_eq!(out, r"''\''a'\'' '\''b'\'''");
    }
}
