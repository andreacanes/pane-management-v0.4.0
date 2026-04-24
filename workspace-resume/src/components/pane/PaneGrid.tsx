import { createMemo, createSignal, For, Show } from "solid-js";
import { useApp } from "../../contexts/AppContext";
import { PaneSlot } from "./PaneSlot";
import { CreatePaneModal } from "./CreatePaneModal";
import type { TmuxPane } from "../../lib/types";
import { Plus } from "../ui/icons";

/**
 * Compute inline grid style STRING based on pane count and geometry.
 * Uses a raw string (not an object) to guarantee the browser receives
 * the exact CSS we intend — avoids any SolidJS style-object edge cases.
 *
 * Host-aware note: with cross-host panes the `top`/`left` values come
 * from different tmux servers and aren't commensurable. We sort panes
 * by host (local first, then alphabetical) before handing them to this
 * function, then fall back to geometric ordering within each host.
 */
function gridStyleString(panes: TmuxPane[]): string {
  const count = panes.length;
  if (count <= 1) {
    return "display:grid; grid-template-columns:1fr; grid-template-rows:1fr;";
  }
  if (count === 2) {
    const sameTop = panes[0].top === panes[1].top;
    return sameTop
      ? "display:grid; grid-template-columns:1fr 1fr; grid-template-rows:1fr;"
      : "display:grid; grid-template-columns:1fr; grid-template-rows:1fr 1fr;";
  }
  if (count === 3 || count === 4) {
    return "display:grid; grid-template-columns:1fr 1fr; grid-template-rows:1fr 1fr;";
  }
  const rows = Math.ceil(count / 3);
  const rowStr = Array(rows).fill("1fr").join(" ");
  return `display:grid; grid-template-columns:1fr 1fr 1fr; grid-template-rows:${rowStr};`;
}

/** Build the 4-segment `pane_assignments` key from a pane's coordinate.
 *  Same shape as `build_key` in the Rust side and `assignmentKey` in
 *  `PaneSlot`. Kept as a local helper because PaneGrid and PaneSlot
 *  are the only two places that need it. */
function assignmentKeyFor(pane: TmuxPane): string {
  const host = pane.host || "local";
  const session = pane.session_name || "";
  return `${host}|${session}|${pane.window_index}|${pane.pane_index}`;
}

export function PaneGrid() {
  const { state, refreshTmuxState } = useApp();
  const [createOpen, setCreateOpen] = createSignal(false);

  const hasSession = () => state.selectedTmuxSession != null;
  const hasWindow = () => state.selectedTmuxWindow != null;
  const hasPanes = () => state.tmuxPanes.length > 0;

  // Host-aware ordering: local first, then each remote host alphabetical;
  // within one host sort by session name, then by tmux's top/left so
  // physical layout still flows left-to-right / top-to-bottom per host.
  const sortedPanes = createMemo(() =>
    [...state.tmuxPanes].sort((a, b) => {
      const aHost = a.host || "local";
      const bHost = b.host || "local";
      if (aHost !== bHost) {
        if (aHost === "local") return -1;
        if (bHost === "local") return 1;
        return aHost.localeCompare(bHost);
      }
      const aSess = a.session_name || "";
      const bSess = b.session_name || "";
      if (aSess !== bSess) return aSess.localeCompare(bSess);
      return a.top - b.top || a.left - b.left;
    }),
  );

  return (
    <Show
      when={hasSession() && hasWindow()}
      fallback={
        <div class="pane-grid-empty">
          Select a tmux session and window above, then drag projects here
        </div>
      }
    >
      <div class="pane-grid-toolbar">
        <button
          class="pane-grid-toolbar__button"
          onClick={() => setCreateOpen(true)}
          title="Create a new tmux pane on WSL or the Mac"
        >
          <Plus size={12} /> Add Pane
        </button>
      </div>

      <Show
        when={hasPanes()}
        fallback={
          <div class="pane-grid-empty">No panes in this window</div>
        }
      >
        <div
          class="pane-grid"
          style={gridStyleString(sortedPanes())}
          title={`${state.tmuxPanes.length} panes (${state.tmuxPanes.filter((p) => (p.host || "local") !== "local").length} remote)`}
        >
          <For each={sortedPanes()}>
            {(pane) => (
              <PaneSlot
                pane={pane}
                assignment={state.paneAssignments[assignmentKeyFor(pane)] ?? null}
              />
            )}
          </For>
        </div>
      </Show>

      <CreatePaneModal
        open={createOpen()}
        onClose={() => setCreateOpen(false)}
        onCreated={() => {
          // The new pane / session will be picked up by the next poll —
          // trigger one now for immediate feedback instead of waiting
          // on the 10s cadence.
          refreshTmuxState();
        }}
      />
    </Show>
  );
}
