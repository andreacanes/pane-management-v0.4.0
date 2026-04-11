import { createResource, For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { listActiveClaudePanes, getUsageSummary } from "../../lib/tauri-commands";
import type { ActivePane, UsageSummary } from "../../lib/types";

/**
 * Cross-project "all my active Claudes" panel.
 * Polls every 3s while visible. Shows:
 *  - total USD + token counts (from get_usage_summary)
 *  - a flat list of every tmux pane currently running claude, with path
 * The user can click a row to switch to that pane via tmux select-window.
 */
export function GlobalActivePanel(props: { onClose: () => void }) {
  const [panes, setPanes] = createSignal<ActivePane[]>([]);
  const [summary, setSummary] = createSignal<UsageSummary | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  async function refresh() {
    try {
      setPanes(await listActiveClaudePanes());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  // Usage summary is expensive — load it once on mount.
  const [_usageResource] = createResource(async () => {
    try {
      const s = await getUsageSummary();
      setSummary(s);
      return s;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return null;
    }
  });

  onMount(() => {
    refresh();
    const timer = setInterval(refresh, 3000);
    onCleanup(() => clearInterval(timer));
  });

  function fmtTokens(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return String(n);
  }
  function fmtUSD(n: number): string {
    if (n >= 100) return `$${n.toFixed(0)}`;
    if (n >= 10) return `$${n.toFixed(1)}`;
    return `$${n.toFixed(2)}`;
  }

  return (
    <div class="global-active-panel">
      <div class="global-active-header">
        <strong>All active Claudes</strong>
        <button class="modal-btn" onClick={props.onClose}>✕</button>
      </div>

      <Show when={summary()}>
        <div class="global-active-summary">
          <span>{summary()!.projects} projects · {summary()!.sessions} sessions</span>
          <span style={{ "margin-left": "12px" }}>
            {fmtUSD(summary()!.total_cost_usd)} total
          </span>
          <span style={{ "margin-left": "12px" }}>
            {fmtTokens(
              summary()!.total_input +
                summary()!.total_output +
                summary()!.total_cache_write +
                summary()!.total_cache_read,
            )} tokens
          </span>
        </div>
      </Show>

      <Show when={error()}>
        <div class="error" style={{ "padding": "8px 12px", "color": "#e66" }}>
          {error()}
        </div>
      </Show>

      <div class="global-active-list">
        <Show
          when={panes().length > 0}
          fallback={
            <div style={{ "padding": "16px", "opacity": 0.7 }}>No active Claude panes.</div>
          }
        >
          <For each={panes()}>
            {(p) => (
              <div class="global-active-row">
                <div class="global-active-id">
                  <span class="dot" />
                  {p.id}
                </div>
                <div class="global-active-window">{p.window_name}</div>
                <div class="global-active-path" title={p.current_path}>
                  {p.current_path}
                </div>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}
