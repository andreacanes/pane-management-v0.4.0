import { createSignal, createMemo, For, Show, onMount, onCleanup } from "solid-js";
import { createDroppable } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import { setProjectTier, sendToPaneOn } from "../../lib/tauri-commands";
import { deriveName, pathMatchesProject } from "../../lib/path";
import type { ProjectWithMeta } from "../../lib/types";

/** Prefix for pin-bar draggable IDs to avoid collisions with sidebar cards. */
export const PIN_PREFIX = "pin:";

function PinnedPill(props: {
  project: ProjectWithMeta;
  onContextMenu: (e: MouseEvent, project: ProjectWithMeta) => void;
}) {
  const { state, isProjectActiveInSession, isProjectWaitingInSession, findProjectWindow, selectTmuxSession, selectTmuxWindow } = useApp();

  const name = () =>
    props.project.meta.display_name || deriveName(props.project.actual_path);

  const isActive = () => isProjectActiveInSession(props.project.encoded_name);
  const isWaiting = () => isProjectWaitingInSession(props.project.encoded_name);

  const dotClass = () => {
    if (isWaiting()) return "window-status-dot waiting";
    if (isActive()) return "window-status-dot active";
    return "";
  };

  /** Locate a waiting-for-approval pane for this project across every
   *  host+session the app is tracking. Return carries the full
   *  coordinate so the double-click Enter-send below can route the
   *  keys to the correct tmux server (local or Mac). windowStatuses
   *  keys are "host|session|winIdx" after the multi-host refactor. */
  function findWaitingPane(): { host: string; session: string; windowIndex: number; paneIndex: number } | null {
    const proj = state.projects.find((p) => p.encoded_name === props.project.encoded_name);
    if (!proj) return null;
    for (const [key, status] of Object.entries(state.windowStatuses)) {
      const [host, session, winStr] = key.split("|");
      if (!host || !session || !winStr) continue;
      const windowIndex = Number(winStr);
      const paths = status.active_paths ?? [];
      const panes = status.active_panes ?? [];
      const waiting = status.waiting_panes ?? [];
      for (let i = 0; i < paths.length; i++) {
        if (pathMatchesProject(paths[i], proj.actual_path) && waiting.includes(panes[i])) {
          return { host, session, windowIndex, paneIndex: panes[i] };
        }
      }
    }
    return null;
  }

  let clickTimer: ReturnType<typeof setTimeout> | null = null;

  function handleClick(e: MouseEvent) {
    e.stopPropagation();
    if (clickTimer) {
      clearTimeout(clickTimer);
      clickTimer = null;
      const wp = findWaitingPane();
      if (wp) {
        // Empty command with Enter = "approve default / confirm" on the
        // Claude approval prompt. Routes to the pane's own host so a
        // Mac pane's approval is sent over SSH to the Mac's tmux, not
        // the local server.
        sendToPaneOn(wp.host, wp.session, wp.windowIndex, wp.paneIndex, "").catch(() => {});
      }
    } else {
      const win = findProjectWindow(props.project.encoded_name);
      // Only follow local windows — TopBar session/window tabs don't
      // yet expose remote-host sessions, so selecting a Mac session
      // would orphan the tab bar. Double-click send-Enter above still
      // works for Mac panes because it doesn't need UI navigation.
      if (win && win.host === "local") {
        selectTmuxSession(win.session);
        selectTmuxWindow(win.windowIndex);
      }
      clickTimer = setTimeout(() => { clickTimer = null; }, 300);
    }
  }

  return (
    <button
      class="quick-launch-btn"
      title={`Click to focus${isWaiting() ? " · Double-click to approve" : ""}`}
      onClick={handleClick}
      onContextMenu={(e) => props.onContextMenu(e, props.project)}
    >
      <Show when={dotClass()}>
        <span class={dotClass()} />
      </Show>
      {name()}
    </button>
  );
}

export function QuickLaunch() {
  const { state, projectsByTier, openProjectSettings, refreshProjects } = useApp();

  const pinDroppable = createDroppable("quick-launch-pin");
  const pinned = () => projectsByTier("pinned");

  const orderedPinned = createMemo(() => {
    const projects = [...pinned()];
    const order = state.pinnedOrder;
    if (order.length === 0) return projects;
    const orderMap = new Map(order.map((name, i) => [name, i]));
    return projects.sort((a, b) => (orderMap.get(a.encoded_name) ?? Infinity) - (orderMap.get(b.encoded_name) ?? Infinity));
  });

  // Status counts
  const statusCounts = createMemo(() => {
    let running = 0;
    let waiting = 0;
    let idle = 0;
    for (const ws of Object.values(state.windowStatuses)) {
      const actives = ws.active_panes ?? [];
      const waitings = ws.waiting_panes ?? [];
      waiting += waitings.length;
      running += actives.filter((p: string) => !waitings.includes(p)).length;
    }
    // Idle = total panes across all windows minus running minus waiting
    const totalPanes = state.tmuxSessions.reduce((sum, s) => {
      const wins = state.tmuxWindows;
      return sum + wins.reduce((ws, w) => ws + w.panes, 0);
    }, 0);
    idle = Math.max(0, totalPanes - running - waiting);
    return { running, waiting, idle };
  });

  // Context menu
  const [contextMenu, setContextMenu] = createSignal<{
    x: number; y: number; project: ProjectWithMeta;
  } | null>(null);

  function showContextMenu(e: MouseEvent, project: ProjectWithMeta) {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, project });
  }
  function closeContextMenu() { setContextMenu(null); }

  async function handleUnpin(project: ProjectWithMeta) {
    closeContextMenu();
    await setProjectTier(project.encoded_name, "active");
    refreshProjects();
  }

  onMount(() => document.addEventListener("click", closeContextMenu));
  onCleanup(() => document.removeEventListener("click", closeContextMenu));

  return (
    <div class="quick-launch-container">
      <div class="status-bar">
        {/* Left: status counts */}
        <div class="status-bar-counts">
          <Show when={statusCounts().running > 0}>
            <span class="status-count running">{statusCounts().running} Running</span>
          </Show>
          <Show when={statusCounts().waiting > 0}>
            <span class="status-count waiting">{statusCounts().waiting} Waiting</span>
          </Show>
          <Show when={statusCounts().idle > 0}>
            <span class="status-count idle">{statusCounts().idle} Idle</span>
          </Show>
        </div>

        {/* Right: pinned pills */}
        <div
          ref={(el) => pinDroppable(el)}
          class="status-bar-pins"
          classList={{ "drop-active": pinDroppable.isActiveDroppable }}
        >
          <Show
            when={pinned().length > 0}
            fallback={<span class="quick-launch-empty">Drop to pin</span>}
          >
            <For each={orderedPinned()}>
              {(project) => <PinnedPill project={project} onContextMenu={showContextMenu} />}
            </For>
          </Show>
        </div>
      </div>

      <Show when={contextMenu()}>
        <div
          class="tab-context-menu"
          style={{ left: `${contextMenu()!.x}px`, top: `${contextMenu()!.y}px` }}
        >
          <button class="tab-context-item" onClick={() => { openProjectSettings(contextMenu()!.project); closeContextMenu(); }}>
            Settings
          </button>
          <button class="tab-context-item" onClick={() => handleUnpin(contextMenu()!.project)}>
            Unpin
          </button>
        </div>
      </Show>
    </div>
  );
}
