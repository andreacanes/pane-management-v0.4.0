import { Show } from "solid-js";
import {
  accountForPane,
  detectAccount,
  ACCOUNT_LABELS,
  ACCOUNT_COLORS,
  type ClaudeAccount,
} from "../../lib/account";

export interface AccountBadgeProps {
  /** Pre-resolved account — takes precedence over everything else. */
  account?: ClaudeAccount;
  /**
   * Preferred source: pass the full pane DTO so the badge uses the
   * server-detected `claude_account` field when available and falls
   * back to the command-string regex otherwise.
   */
  pane?: {
    claude_account?: "andrea" | "bravura" | null;
    start_command?: string | null;
    current_command?: string | null;
  };
  /** Legacy path — raw command strings. Ignored if `pane` is set. */
  startCommand?: string | null;
  currentCommand?: string | null;
  compact?: boolean;
}

/**
 * Small chip showing whether a pane is running under the primary
 * account (Andrea / `~/.claude`) or the secondary (Bravura / `~/.claude-b`).
 * Renders nothing when the pane isn't running Claude.
 */
export function AccountBadge(props: AccountBadgeProps) {
  const account = (): ClaudeAccount => {
    if (props.account) return props.account;
    if (props.pane) return accountForPane(props.pane);
    return detectAccount(props.startCommand, props.currentCommand);
  };

  return (
    <Show when={account()}>
      {(acct) => (
        <span
          class={`ui-account-badge${props.compact ? " ui-account-badge-compact" : ""}`}
          style={{
            "--account-color": ACCOUNT_COLORS[acct()],
          }}
          title={`Claude account: ${ACCOUNT_LABELS[acct()]}`}
        >
          <span class="ui-account-dot" />
          <Show when={!props.compact}>
            <span class="ui-account-label">{ACCOUNT_LABELS[acct()]}</span>
          </Show>
        </span>
      )}
    </Show>
  );
}
