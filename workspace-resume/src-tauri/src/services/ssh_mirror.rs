//! Identifying SSH-mirror panes — local tmux panes whose first process
//! is `ssh -t <alias> tmux attach-session -t <session>`, used as a
//! WezTerm-visible viewport into a remote tmux server.
//!
//! Why this needs first-class support:
//! Without it, the cwd-fallback project lookup in the poller picks up
//! the SSH process's `current_path` (`/home/andrea` or
//! `/mnt/c/Users/Andrea`) and matches it to whatever WSL project lives
//! at that path — typically a generic `andrea` entry — so every mirror
//! pane gets mistagged as that project across all clients (desktop
//! grid title, APK list row, status chips). The desktop frontend
//! works around it with a local `isSshMirror()` regex, but the wire
//! is still wrong, and the APK has no equivalent guard so it shows
//! the wrong label.
//!
//! Single-source-of-truth fix: stamp `mirror_target: Some(...)` on
//! the DTO when we recognise the pattern, skip the project lookup,
//! and let every client read one unambiguous field.

use serde::{Deserialize, Serialize};

/// The remote `<alias>/<session>` a local SSH-mirror pane points at.
/// Carried on `TmuxPane` (Tauri wire) and `PaneDto` (HTTP wire);
/// `None` for ordinary panes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorTarget {
    pub alias: String,
    pub session: String,
}

/// Parse `start_command` and return `Some(MirrorTarget)` iff it matches
/// `ssh -t <alias> tmux attach-session -t <session>` (with or without
/// surrounding tmux-format quotes). Pure; no I/O.
///
/// Tolerates the various ways the command can appear in `pane_start_command`:
/// * Bare:    `ssh -t mac tmux attach-session -t foo`
/// * Quoted:  `"ssh -t mac tmux attach-session -t foo"`
/// * With extra args (verbose, port, cipher etc.) between `ssh` and `-t alias`.
///
/// Returns `None` for anything else, including:
/// * Non-mirror commands (claude, bash, vim, ...)
/// * SSH commands that don't end in `tmux attach-session -t <session>`
///   (e.g. `ssh mac` for a normal interactive shell).
pub fn parse_mirror_target(start_command: &str) -> Option<MirrorTarget> {
    let s = start_command.trim().trim_matches(|c| c == '"' || c == '\'');
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if !lower.starts_with("ssh ") && !lower.starts_with("ssh\t") {
        return None;
    }
    if !lower.contains("tmux attach-session") {
        return None;
    }

    // Walk tokens, finding the *first* `-t <alias>` after `ssh`,
    // then the *next* `-t <session>` after `tmux attach-session`.
    // tokens() splits on whitespace which is fine — neither alias
    // nor session can contain whitespace (tmux session names reject
    // spaces; SSH aliases reject them too).
    let mut tokens = s.split_whitespace();
    if tokens.next()?.to_ascii_lowercase() != "ssh" {
        return None;
    }

    // Phase 1: find the alias as the value after the first standalone `-t`.
    // Skip any other `ssh` flags before it.
    let mut alias = None;
    while let Some(tok) = tokens.next() {
        if tok == "-t" {
            alias = tokens.next().map(strip_quotes_owned);
            break;
        }
    }
    let alias = alias?;
    if alias.is_empty() {
        return None;
    }

    // Phase 2: scan ahead for `tmux`, then `attach-session`, then `-t <session>`.
    let mut saw_tmux = false;
    let mut saw_attach = false;
    let mut session = None;
    for tok in tokens {
        if !saw_tmux {
            if tok.eq_ignore_ascii_case("tmux") {
                saw_tmux = true;
            }
            continue;
        }
        if !saw_attach {
            if tok.eq_ignore_ascii_case("attach-session") || tok.eq_ignore_ascii_case("attach") {
                saw_attach = true;
            }
            continue;
        }
        if tok == "-t" {
            session = None; // reset on duplicate -t to take the most recent value
            continue;
        }
        // First non-flag token after `-t` wins.
        session = Some(strip_quotes_owned(tok));
        break;
    }
    let session = session?;
    if session.is_empty() {
        return None;
    }

    Some(MirrorTarget { alias, session })
}

fn strip_quotes_owned(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(input: &str, alias: &str, session: &str) {
        let got = parse_mirror_target(input).unwrap_or_else(|| {
            panic!("expected Some for: {input:?}");
        });
        assert_eq!(got.alias, alias, "alias mismatch for {input:?}");
        assert_eq!(got.session, session, "session mismatch for {input:?}");
    }

    fn n(input: &str) {
        let got = parse_mirror_target(input);
        assert!(got.is_none(), "expected None for {input:?}, got {got:?}");
    }

    #[test]
    fn bare_form() {
        t(
            "ssh -t mac tmux attach-session -t akamai-v3-bestbuy",
            "mac",
            "akamai-v3-bestbuy",
        );
    }

    #[test]
    fn surrounding_double_quotes() {
        // tmux's `pane_start_command` formats with surrounding quotes.
        t(
            r#""ssh -t mac tmux attach-session -t siteforge-module-2""#,
            "mac",
            "siteforge-module-2",
        );
    }

    #[test]
    fn surrounding_single_quotes() {
        t(
            r"'ssh -t mac tmux attach-session -t foo'",
            "mac",
            "foo",
        );
    }

    #[test]
    fn extra_ssh_flags_before_alias() {
        t(
            "ssh -v -p 2222 -t mac tmux attach-session -t foo",
            "mac",
            "foo",
        );
    }

    #[test]
    fn attach_short_form() {
        // `tmux attach` is a synonym for `tmux attach-session` — accept both.
        t("ssh -t mac tmux attach -t foo", "mac", "foo");
    }

    #[test]
    fn alias_with_dash() {
        t(
            "ssh -t linux-box tmux attach-session -t my-session",
            "linux-box",
            "my-session",
        );
    }

    #[test]
    fn rejects_plain_ssh_shell() {
        // No tmux attach — just an interactive ssh.
        n("ssh mac");
        n("ssh -t mac");
    }

    #[test]
    fn rejects_local_commands() {
        n("claude");
        n("bash");
        n("");
        n("nvim ~/.tmux.conf");
    }

    #[test]
    fn rejects_ssh_to_other_command() {
        n("ssh -t mac htop");
        n("ssh -t mac vim file");
    }

    #[test]
    fn rejects_no_session_arg() {
        n("ssh -t mac tmux attach-session");
        n("ssh -t mac tmux attach-session -t");
    }

    #[test]
    fn empty_alias_or_session_rejected() {
        n("ssh -t '' tmux attach-session -t foo");
        n("ssh -t mac tmux attach-session -t ''");
    }
}
