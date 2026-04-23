/**
 * Which Claude Code account a pane was launched under. `andrea` is the
 * primary; `bravura` points at `~/.claude-b`; `sully` points at
 * `~/.claude-c`. The Rust registry at
 * `workspace-resume/src-tauri/src/companion/accounts.rs` is the single
 * source of truth — mirror any changes here, in the mobile `Dtos.kt`,
 * and in `ui/theme/StatusColors.kt` per `.claude/rules/account-key-mirror.md`.
 */
export type ClaudeAccount = "andrea" | "bravura" | "sully" | null;

export const ACCOUNT_LABELS: Record<Exclude<ClaudeAccount, null>, string> = {
  andrea: "Andrea",
  bravura: "Bravura",
  sully: "Sully",
};

export const ACCOUNT_COLORS: Record<Exclude<ClaudeAccount, null>, string> = {
  andrea: "#818cf8",   // accent indigo — primary
  bravura: "#f59e0b",  // amber — secondary
  sully: "#14b8a6",    // teal — tertiary
};

/** Detect the account from a pane's start_command and current_command. */
export function detectAccount(
  startCommand: string | undefined | null,
  currentCommand: string | undefined | null = null,
): ClaudeAccount {
  const s = (startCommand ?? "").toLowerCase();
  const c = (currentCommand ?? "").toLowerCase();
  // Start command is authoritative; check config-dir suffixes in order
  // of specificity so "claude-c" and "claude-b" bind before the bare
  // "claude" fallback.
  if (s.includes("claude-c")) return "sully";
  if (c === "claude-c") return "sully";
  if (s.includes("claude-b")) return "bravura";
  if (c === "claude-b") return "bravura";
  if (s.includes("claude") || c.includes("claude")) return "andrea";
  return null;
}

/**
 * Authoritative per-pane account resolution. Prefers the server's
 * `claude_account` field (populated by walking `/proc/<pid>/environ`
 * to find `CLAUDE_CONFIG_DIR` on local panes, or synthesized from the
 * pane assignment for remote panes), falls back to the command-string
 * regex for older servers or panes without a detected account.
 *
 * The regex fallback cannot see Bravura/Sully panes launched via shell
 * functions that inline `CLAUDE_CONFIG_DIR=~/.claude-b claude ...` —
 * for those the server-side field is the only reliable source.
 */
export function accountForPane(pane: {
  claude_account?: ClaudeAccount;
  start_command?: string | null;
  current_command?: string | null;
}): ClaudeAccount {
  if (
    pane.claude_account === "andrea" ||
    pane.claude_account === "bravura" ||
    pane.claude_account === "sully"
  ) {
    return pane.claude_account;
  }
  return detectAccount(pane.start_command, pane.current_command);
}

export function accountLabel(acct: ClaudeAccount): string | null {
  return acct ? ACCOUNT_LABELS[acct] : null;
}
