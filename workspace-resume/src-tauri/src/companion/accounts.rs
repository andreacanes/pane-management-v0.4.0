//! Dynamic account registry.
//!
//! Single source of truth for Claude Code accounts. Adding a third
//! account is a one-line change here — no other files need updating.

/// Describes one Claude Code account (OAuth identity + config directory).
pub struct AccountDef {
    /// Wire key: the string stored in `PaneDto.claude_account` and sent
    /// over the companion API (`"andrea"`, `"bravura"`).
    pub key: &'static str,
    /// Human-readable label for the mobile UI.
    pub label: &'static str,
    /// WSL-relative config directory name under `$HOME` — e.g. `.claude`
    /// resolves to `/home/<user>/.claude/`.
    pub config_dir: &'static str,
}

/// All known accounts. Order determines display order on the mobile UI.
pub const ACCOUNTS: &[AccountDef] = &[
    AccountDef {
        key: "andrea",
        label: "Andrea",
        config_dir: ".claude",
    },
    AccountDef {
        key: "bravura",
        label: "Bravura",
        config_dir: ".claude-b",
    },
    AccountDef {
        key: "sully",
        label: "Sully",
        config_dir: ".claude-c",
    },
];
