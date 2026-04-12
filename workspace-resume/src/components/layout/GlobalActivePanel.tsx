import { createMemo, createResource, For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { listActiveClaudePanes, getUsageSummary, getAllUsage } from "../../lib/tauri-commands";
import type { ActivePane, UsageSummary, ProjectUsage, ProjectWithMeta } from "../../lib/types";
import { useApp } from "../../contexts/AppContext";
import { fromWslPath, deriveName } from "../../lib/path";
import { Card } from "../ui/Card";
import { StatusChip } from "../ui/StatusChip";
import { AccountBadge } from "../ui/AccountBadge";
import { X, Activity, ChevronRight } from "../ui/icons";

interface ProjectGroup {
  key: string;
  name: string;
  project: ProjectWithMeta | null;
  panes: ActivePane[];
  usage: ProjectUsage | null;
}

/**
 * Cross-project "all my active Claudes" panel. Groups running panes by
 * project, shows per-project cost + token totals, and jumps to the
 * target session/window when a row is clicked.
 */
export function GlobalActivePanel(props: { onClose: () => void }) {
  const { state, selectTmuxSession, selectTmuxWindow } = useApp();

  const [panes, setPanes] = createSignal<ActivePane[]>([]);
  const [summary, setSummary] = createSignal<UsageSummary | null>(null);
  const [projectUsage, setProjectUsage] = createSignal<Record<string, ProjectUsage>>({});
  const [error, setError] = createSignal<string | null>(null);

  async function refresh() {
    try {
      setPanes(await listActiveClaudePanes());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  const [_usageResource] = createResource(async () => {
    try {
      const [s, all] = await Promise.all([getUsageSummary(), getAllUsage()]);
      setSummary(s);
      setProjectUsage(all);
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

  /** Match an active pane's current_path to a known project. */
  function matchProject(panePath: string): ProjectWithMeta | null {
    if (!panePath) return null;
    const paneAsWsl = panePath.toLowerCase().replace(/\/+$/, "");
    const paneAsWin = fromWslPath(panePath).toLowerCase().replace(/[\\/]+$/, "");
    return (
      state.projects.find((p) => {
        const actual = p.actual_path.toLowerCase().replace(/[\\/]+$/, "");
        return actual === paneAsWsl || actual === paneAsWin;
      }) ?? null
    );
  }

  /** Group active panes by resolved project (or "unknown" bucket). */
  const groups = createMemo<ProjectGroup[]>(() => {
    const list = panes();
    const usage = projectUsage();
    const byKey = new Map<string, ProjectGroup>();

    for (const pane of list) {
      const proj = matchProject(pane.current_path);
      const key = proj?.encoded_name ?? `__orphan__${pane.current_path || pane.id}`;
      const existing = byKey.get(key);
      if (existing) {
        existing.panes.push(pane);
      } else {
        const name = proj
          ? (proj.meta.display_name || deriveName(proj.actual_path))
          : (pane.current_path ? deriveName(pane.current_path) : "Unknown project");
        byKey.set(key, {
          key,
          name,
          project: proj,
          panes: [pane],
          usage: proj ? (usage[proj.encoded_name] ?? null) : null,
        });
      }
    }

    return Array.from(byKey.values()).sort((a, b) => {
      const ac = a.usage?.total_cost_usd ?? 0;
      const bc = b.usage?.total_cost_usd ?? 0;
      if (ac !== bc) return bc - ac;
      return a.name.localeCompare(b.name);
    });
  });

  function jumpToPane(pane: ActivePane) {
    selectTmuxSession(pane.session_name);
    selectTmuxWindow(pane.window_index);
    props.onClose();
  }

  function groupTokens(u: ProjectUsage): number {
    return u.total_input + u.total_output + u.total_cache_write + u.total_cache_read;
  }

  function summaryTokens(s: UsageSummary): number {
    return s.total_input + s.total_output + s.total_cache_write + s.total_cache_read;
  }

  return (
    <div class="global-active-panel">
      <div class="global-active-header">
        <div class="global-active-title">
          <Activity size={16} />
          <strong>All active Claudes</strong>
        </div>
        <button class="global-active-close" onClick={props.onClose} aria-label="Close">
          <X size={14} />
        </button>
      </div>

      <Show when={summary()}>
        <div class="global-active-summary">
          <div class="gap-stat">
            <span class="gap-stat-label">Projects</span>
            <span class="gap-stat-value">{summary()!.projects}</span>
          </div>
          <div class="gap-stat">
            <span class="gap-stat-label">Sessions</span>
            <span class="gap-stat-value">{summary()!.sessions}</span>
          </div>
          <div class="gap-stat">
            <span class="gap-stat-label">Cost</span>
            <span class="gap-stat-value">{fmtUSD(summary()!.total_cost_usd)}</span>
          </div>
          <div class="gap-stat">
            <span class="gap-stat-label">Tokens</span>
            <span class="gap-stat-value">{fmtTokens(summaryTokens(summary()!))}</span>
          </div>
        </div>
      </Show>

      <Show when={error()}>
        <div class="error global-active-error">{error()}</div>
      </Show>

      <div class="global-active-list">
        <Show
          when={groups().length > 0}
          fallback={
            <div class="global-active-empty">
              <Activity size={28} />
              <p>No Claude sessions are running.</p>
              <span>Start one from a project card or drop a project onto a pane.</span>
            </div>
          }
        >
          <For each={groups()}>
            {(group) => (
              <Card class="global-active-group">
                <div class="global-active-group-header">
                  <span class="global-active-group-name">{group.name}</span>
                  <Show when={group.usage}>
                    <span class="global-active-group-usage">
                      {fmtUSD(group.usage!.total_cost_usd)} · {fmtTokens(groupTokens(group.usage!))}
                    </span>
                  </Show>
                </div>
                <div class="global-active-group-rows">
                  <For each={group.panes}>
                    {(pane) => (
                      <button
                        class="global-active-row"
                        onClick={() => jumpToPane(pane)}
                        title={`${pane.session_name}:${pane.window_index}.${pane.pane_index} — jump`}
                      >
                        <StatusChip status="running" compact />
                        <div class="global-active-row-main">
                          <span class="global-active-row-name">
                            {pane.session_name}:{pane.window_name || `#${pane.window_index}`}
                          </span>
                          <span class="global-active-row-path" title={pane.current_path}>
                            {pane.current_path || "(no path)"}
                          </span>
                        </div>
                        <AccountBadge pane={pane} />
                        <ChevronRight size={14} />
                      </button>
                    )}
                  </For>
                </div>
              </Card>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}
