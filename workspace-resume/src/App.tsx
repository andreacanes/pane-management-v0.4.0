import { createSignal, Show, onMount, onCleanup } from "solid-js";
import { AppProvider, useApp } from "./contexts/AppContext";
import {
  DragDropProvider,
  DragDropSensors,
  DragOverlay,
} from "@thisbeyond/solid-dnd";
import type { DragEvent as SolidDragEvent } from "@thisbeyond/solid-dnd";
import { TopBar } from "./components/layout/TopBar";
import { Sidebar } from "./components/layout/Sidebar";
import { MainArea } from "./components/layout/MainArea";
import { ProjectDetailModal } from "./components/project/ProjectDetailModal";
import { deriveName } from "./lib/path";
import { assignToPane } from "./lib/launch";
import { PIN_PREFIX } from "./components/layout/QuickLaunch";
import { WINDOW_TAB_PREFIX } from "./components/layout/TopBar";
import { setProjectTier } from "./lib/tauri-commands";
import { ToastHost } from "./components/ui/Toast";

/**
 * Inner component that consumes AppContext (must be rendered inside AppProvider).
 * Hosts the DragDropProvider with a simplified drag-end handler.
 */
function AppInner() {
  const { state, refreshTmuxState, refreshProjects, reorderWindows, settingsProject, closeProjectSettings } = useApp();

  const [sidebarWidth, setSidebarWidth] = createSignal(280);
  const [pendingPaneDrop, setPendingPaneDrop] = createSignal<{
    encodedProject: string;
    projectPath: string;
    host: string;
    session: string;
    windowIndex: number;
    paneIndex: number;
    existingName: string;
  } | null>(null);

  function handleResizeStart(e: MouseEvent) {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = sidebarWidth();

    function onMove(ev: MouseEvent) {
      const newWidth = Math.max(180, Math.min(500, startWidth + ev.clientX - startX));
      setSidebarWidth(newWidth);
    }

    function onUp() {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    }

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  /**
   * Simplified drag-end: only handles window tab reorder and project-to-pane/pin drops.
   * Session tab reorder, pane-slot swap, and pin reorder have been removed.
   */
  async function handleDragEnd(event: SolidDragEvent) {
    const { draggable, droppable } = event;
    if (!draggable || !droppable) return;

    const rawId = draggable.id as string;
    const dropId = droppable.id as string;

    // Window tab reorder (the only remaining tab reorder)
    if (rawId.startsWith(WINDOW_TAB_PREFIX) && dropId.startsWith(WINDOW_TAB_PREFIX)) {
      const fromIndex = parseInt(rawId.slice(WINDOW_TAB_PREFIX.length), 10);
      const toIndex = parseInt(dropId.slice(WINDOW_TAB_PREFIX.length), 10);
      reorderWindows(fromIndex, toIndex);
      return;
    }

    // Pin drop: project dragged onto quick-launch-pin zone
    const encodedProject = rawId.startsWith(PIN_PREFIX)
      ? rawId.slice(PIN_PREFIX.length)
      : rawId;

    if (dropId === "quick-launch-pin") {
      try {
        await setProjectTier(encodedProject, "pinned");
        refreshProjects();
      } catch (e) {
        console.error("[App] pin drop error:", e);
      }
      return;
    }

    // Project dropped onto a pane slot. Slot drop ids are the full
    // 4-segment coord `host|session|window|pane` after the host-aware
    // refactor (PaneSlot.tsx::assignmentKey). Legacy numeric-only ids
    // aren't supported anymore.
    const parts = dropId.split("|");
    if (parts.length !== 4) return;
    const [dropHost, dropSession, dropWinStr, dropPaneStr] = parts;
    const dropWindowIndex = parseInt(dropWinStr, 10);
    const dropPaneIndex = parseInt(dropPaneStr, 10);
    if (!dropHost || !dropSession || Number.isNaN(dropWindowIndex) || Number.isNaN(dropPaneIndex)) {
      return;
    }

    const project = state.projects.find((p) => p.encoded_name === encodedProject);
    if (!project) return;

    // Look up the assignment + target pane using the full coord so a
    // Local main:0.3 drop doesn't alias against a Mac main:0.3 slot.
    const existingAssignment = state.paneAssignments[dropId];
    const pane = state.tmuxPanes.find(
      (p) =>
        (p.host || "local") === dropHost &&
        p.session_name === dropSession &&
        p.window_index === dropWindowIndex &&
        p.pane_index === dropPaneIndex,
    );
    const paneHasActivity =
      existingAssignment ||
      (pane && !["bash", "zsh", "sh", "-"].includes(pane.current_command));

    if (paneHasActivity) {
      const existingProject = existingAssignment
        ? state.projects.find((p) => p.encoded_name === existingAssignment)
        : null;
      const existingName = existingProject
        ? existingProject.meta.display_name ||
          existingProject.actual_path.split(/[\\/]/).pop() ||
          "unknown"
        : pane?.current_command || "active process";
      setPendingPaneDrop({
        encodedProject,
        projectPath: project.actual_path,
        host: dropHost,
        session: dropSession,
        windowIndex: dropWindowIndex,
        paneIndex: dropPaneIndex,
        existingName,
      });
      return;
    }

    await executePaneDrop(
      encodedProject,
      project.actual_path,
      dropHost,
      dropSession,
      dropWindowIndex,
      dropPaneIndex,
    );
  }

  async function executePaneDrop(
    encodedProject: string,
    projectPath: string,
    host: string,
    session: string,
    windowIndex: number,
    paneIndex: number,
  ) {
    try {
      await assignToPane({
        tmuxSession: session,
        tmuxWindow: windowIndex,
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject,
        projectPath,
        targetPaneIndex: paneIndex,
        host,
      });
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[App] executePaneDrop error:", e);
    }
  }

  function AppShell() {
    const { pendingLaunch, cancelPanePick } = useApp();
    const isPaneSelectMode = () => pendingLaunch() != null;

    const paneSelectLabel = () => {
      const pl = pendingLaunch();
      if (!pl) return "";
      const name = pl.project.meta.display_name || deriveName(pl.project.actual_path);
      return `Select a pane for ${name}`;
    };

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape" && isPaneSelectMode()) cancelPanePick();
    }
    onMount(() => document.addEventListener("keydown", handleKeyDown));
    onCleanup(() => document.removeEventListener("keydown", handleKeyDown));

    return (
      <div class={`app-shell ${isPaneSelectMode() ? "dragging-project" : ""}`}>
        <TopBar />
        <div class="app-body">
          <Sidebar width={sidebarWidth()} />
          <div class="sidebar-resize-handle" onMouseDown={handleResizeStart} />
          <MainArea />
        </div>
        <Show when={isPaneSelectMode()}>
          <div class="pane-select-hint">
            {paneSelectLabel()}
            <button class="pane-select-cancel" onClick={cancelPanePick}>{"\u2715"}</button>
          </div>
        </Show>
      </div>
    );
  }

  return (
    <DragDropProvider onDragEnd={handleDragEnd}>
      <DragDropSensors />
      <AppShell />
      <DragOverlay>
        {(draggable) => {
          if (!draggable) return <div class="drag-overlay-card" />;
          const rawId = String(draggable.id);
          const encodedName = rawId.startsWith(PIN_PREFIX) ? rawId.slice(PIN_PREFIX.length) : rawId;
          const project = state.projects.find((p) => p.encoded_name === encodedName);
          const name = project ? project.meta.display_name || deriveName(project.actual_path) : encodedName;
          return <div class="drag-overlay-card">{name}</div>;
        }}
      </DragOverlay>

      {/* Confirm replace modal */}
      <Show when={pendingPaneDrop()}>
        <div class="modal-backdrop" onClick={() => setPendingPaneDrop(null)}>
          <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
            <p class="confirm-message">
              <strong>
                {pendingPaneDrop()!.host === "local" ? "" : `${pendingPaneDrop()!.host} `}
                Pane {pendingPaneDrop()!.session}:{pendingPaneDrop()!.windowIndex}.{pendingPaneDrop()!.paneIndex}
              </strong>{" "}already has:
              <br /><em>{pendingPaneDrop()!.existingName}</em>
            </p>
            <p class="confirm-warning">Replacing it will send Ctrl+C and reassign.</p>
            <div class="confirm-actions">
              <button class="modal-btn" onClick={() => setPendingPaneDrop(null)}>Cancel</button>
              <button class="modal-btn danger" onClick={async () => {
                const drop = pendingPaneDrop()!;
                setPendingPaneDrop(null);
                await executePaneDrop(
                  drop.encodedProject,
                  drop.projectPath,
                  drop.host,
                  drop.session,
                  drop.windowIndex,
                  drop.paneIndex,
                );
              }}>Replace</button>
            </div>
          </div>
        </div>
      </Show>

      <Show when={settingsProject()}>
        <ProjectDetailModal project={settingsProject()!} onClose={closeProjectSettings} />
      </Show>

      <ToastHost />
    </DragDropProvider>
  );
}

function App() {
  return (
    <AppProvider>
      <AppInner />
    </AppProvider>
  );
}

export default App;
