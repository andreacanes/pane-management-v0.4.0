/**
 * Which Claude Code account a pane was launched under. `claude` is the
 * primary (Andrea); `claude-b` is the secondary symlink (Bravura). We
 * detect which by looking at the tmux pane's start_command, which
 * survives even while Claude spawns child processes.
 */
export type ClaudeAccount = "andrea" | "bravura" | null;

export const ACCOUNT_LABELS: Record<Exclude<ClaudeAccount, null>, string> = {
  andrea: "Andrea",
  bravura: "Bravura",
};

export const ACCOUNT_COLORS: Record<Exclude<ClaudeAccount, null>, string> = {
  andrea: "#818cf8",   // accent indigo — primary
  bravura: "#f59e0b",  // amber — secondary
};

/** Detect the account from a pane's start_command and current_command. */
export function detectAccount(
  startCommand: string | undefined | null,
  currentCommand: string | undefined | null = null,
): ClaudeAccount {
  const s = (startCommand ?? "").toLowerCase();
  const c = (currentCommand ?? "").toLowerCase();
  // Start command is authoritative; check for the -b suffix specifically.
  // `\bclaude-b\b` would be ideal but regex is overkill here.
  if (s.includes("claude-b")) return "bravura";
  if (c === "claude-b") return "bravura";
  if (s.includes("claude") || c.includes("claude")) return "andrea";
  return null;
}

/**
 * Authoritative per-pane account resolution. Prefers the server's
 * `claude_account` field (populated by walking `/proc/<pid>/environ`
 * to find `CLAUDE_CONFIG_DIR`), falls back to the command-string
 * regex for older servers or panes without a detected account.
 *
 * The regex fallback cannot see Bravura panes launched via shell
 * functions like `cld2` / `ncld2` / `claude2` that inline
 * `CLAUDE_CONFIG_DIR=~/.claude-b claude ...` — for those the
 * server-side field is the only reliable source.
 */
export function accountForPane(pane: {
  claude_account?: "andrea" | "bravura" | null;
  start_command?: string | null;
  current_command?: string | null;
}): ClaudeAccount {
  if (pane.claude_account === "andrea" || pane.claude_account === "bravura") {
    return pane.claude_account;
  }
  return detectAccount(pane.start_command, pane.current_command);
}

export function accountLabel(acct: ClaudeAccount): string | null {
  return acct ? ACCOUNT_LABELS[acct] : null;
}
