import { createSignal, Show, onMount, onCleanup } from "solid-js";
import { AppProvider, useApp } from "./contexts/AppContext";
import {
  DragDropProvider,
  DragDropSensors,
  DragOverlay,
  useDragDropContext,
} from "@thisbeyond/solid-dnd";
import type { DragEvent as SolidDragEvent } from "@thisbeyond/solid-dnd";
import { TopBar } from "./components/layout/TopBar";
import { Sidebar } from "./components/layout/Sidebar";
import { MainArea } from "./components/layout/MainArea";
import { ProjectDetailModal } from "./components/project/ProjectDetailModal";
import { deriveName } from "./lib/path";
import { assignToPane } from "./lib/launch";
import { setProjectTier } from "./lib/tauri-commands";
import { PIN_PREFIX } from "./components/layout/QuickLaunch";
import { SESSION_TAB_PREFIX, WINDOW_TAB_PREFIX } from "./components/layout/TopBar";
import { PANE_SLOT_PREFIX } from "./components/pane/PaneSlot";
import { NEW_PROJECT_PREFIX, NEW_PROJECT_CONTINUITY_PREFIX } from "./components/project/NewProjectFlow";
import { swapTmuxPane, setPaneAssignment, sendToPane } from "./lib/tauri-commands";
import { toWslPath } from "./lib/path";
import { ToastHost } from "./components/ui/Toast";

/**
 * Inner component that consumes AppContext (must be rendered inside AppProvider).
 * Houses DragDropProvider with onDragEnd handler for project-to-pane assignment.
 */
function AppInner() {
  const { state, refreshTmuxState, refreshProjects, reorderSessions, reorderWindows, reorderPinned, settingsProject, closeProjectSettings } = useApp();

  const [sidebarWidth, setSidebarWidth] = createSignal(280);
  const [pendingPaneDrop, setPendingPaneDrop] = createSignal<{
    encodedProject: string;
    projectPath: string;
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
   * Handle drag-end: when a project card is dropped onto a pane slot,
   * persist the assignment, send a tmux command to launch claude, and refresh state.
   */
  async function handleDragEnd(event: SolidDragEvent) {
    const { draggable, droppable } = event;
    if (!draggable || !droppable) return;

    const rawId = draggable.id as string;
    const dropId = droppable.id as string;

    // Session tab reorder
    if (rawId.startsWith(SESSION_TAB_PREFIX) && dropId.startsWith(SESSION_TAB_PREFIX)) {
      const fromName = rawId.slice(SESSION_TAB_PREFIX.length);
      const toName = dropId.slice(SESSION_TAB_PREFIX.length);
      reorderSessions(fromName, toName);
      return;
    }

    // Window tab reorder
    if (rawId.startsWith(WINDOW_TAB_PREFIX) && dropId.startsWith(WINDOW_TAB_PREFIX)) {
      const fromIndex = parseInt(rawId.slice(WINDOW_TAB_PREFIX.length), 10);
      const toIndex = parseInt(dropId.slice(WINDOW_TAB_PREFIX.length), 10);
      reorderWindows(fromIndex, toIndex);
      return;
    }

    // Pinned pill reorder (drag a pin: onto another pin:)
    if (rawId.startsWith(PIN_PREFIX) && dropId.startsWith(PIN_PREFIX)) {
      const fromName = rawId.slice(PIN_PREFIX.length);
      const toName = dropId.slice(PIN_PREFIX.length);
      reorderPinned(fromName, toName);
      return;
    }

    // Pane slot swap (drag a pane-slot: draggable onto a numeric pane droppable)
    if (rawId.startsWith(PANE_SLOT_PREFIX) && !dropId.startsWith(PANE_SLOT_PREFIX)) {
      const fromPane = parseInt(rawId.slice(PANE_SLOT_PREFIX.length), 10);
      const toPane = parseInt(dropId, 10);
      if (!isNaN(toPane) && fromPane !== toPane && state.selectedTmuxSession && state.selectedTmuxWindow != null) {
        try {
          // Swap pane assignments in the store
          const fromAssign = state.paneAssignments[fromPane.toString()] ?? null;
          const toAssign = state.paneAssignments[toPane.toString()] ?? null;
          await setPaneAssignment(state.selectedTmuxSession, state.selectedTmuxWindow, fromPane, toAssign);
          await setPaneAssignment(state.selectedTmuxSession, state.selectedTmuxWindow, toPane, fromAssign);
          // Swap the actual tmux panes
          await swapTmuxPane(state.selectedTmuxSession, state.selectedTmuxWindow, fromPane, toPane);
          refreshTmuxState();
        } catch (e) {
          console.error("[App] pane swap error:", e);
        }
      }
      return;
    }

    // New project ghost card dropped onto a pane (with or without Continuity)
    const isContinuity = rawId.startsWith(NEW_PROJECT_CONTINUITY_PREFIX);
    const isNewProject = rawId.startsWith(NEW_PROJECT_PREFIX) || isContinuity;
    if (isNewProject) {
      const prefix = isContinuity ? NEW_PROJECT_CONTINUITY_PREFIX : NEW_PROJECT_PREFIX;
      const winPath = rawId.slice(prefix.length).replace(/"/g, "");
      const paneIndex = parseInt(dropId, 10);
      if (!isNaN(paneIndex) && state.selectedTmuxSession && state.selectedTmuxWindow != null) {
        const wslPath = toWslPath(winPath);
        try {
          await sendToPane(state.selectedTmuxSession, state.selectedTmuxWindow, paneIndex, `cd '${wslPath}' && claude`);
          if (isContinuity) {
            // Wait for Claude to initialize, then send /continuity
            await new Promise((r) => setTimeout(r, 7000));
            await sendToPane(state.selectedTmuxSession, state.selectedTmuxWindow, paneIndex, "/continuity");
          }
          refreshTmuxState();
          document.dispatchEvent(new CustomEvent("new-project-dropped"));
        } catch (e) {
          console.error("[App] new project drop error:", e);
        }
      }
      return;
    }

    // Strip the pin: prefix if dragged from the pin bar
    const encodedProject = rawId.startsWith(PIN_PREFIX)
      ? rawId.slice(PIN_PREFIX.length)
      : rawId;
    // Dropped on QuickLaunch → pin the project
    if (dropId === "quick-launch-pin") {
      try {
        await setProjectTier(encodedProject, "pinned");
        refreshProjects();
      } catch (e) {
        console.error("[App] pin error:", e);
      }
      return;
    }

    // Dropped on unpin zone → set back to active
    if (dropId === "quick-launch-unpin" || dropId === "sidebar-unpin") {
      try {
        await setProjectTier(encodedProject, "active");
        refreshProjects();
      } catch (e) {
        console.error("[App] unpin error:", e);
      }
      return;
    }

    // Otherwise it's a pane drop (droppable ID is the pane index)
    const paneIndex = parseInt(dropId, 10);

    const project = state.projects.find(
      (p) => p.encoded_name === encodedProject,
    );
    if (!project) return;

    const session = state.selectedTmuxSession;
    const window = state.selectedTmuxWindow;
    if (!session || window == null) return;

    // Check if the pane already has an active project — confirm before replacing
    const existingAssignment = state.paneAssignments[paneIndex.toString()];
    const pane = state.tmuxPanes.find((p) => p.pane_index === paneIndex);
    const paneHasActivity = existingAssignment || (pane && pane.current_command !== "bash" && pane.current_command !== "zsh" && pane.current_command !== "sh" && pane.current_command !== "-");

    if (paneHasActivity) {
      const existingProject = existingAssignment
        ? state.projects.find((p) => p.encoded_name === existingAssignment)
        : null;
      const existingName = existingProject
        ? (existingProject.meta.display_name || existingProject.actual_path.split(/[\\/]/).pop() || "unknown")
        : (pane?.current_command || "active process");
      setPendingPaneDrop({ encodedProject, projectPath: project.actual_path, paneIndex, existingName });
      return;
    }

    await executePaneDrop(encodedProject, project.actual_path, paneIndex);
  }

  async function executePaneDrop(encodedProject: string, projectPath: string, paneIndex: number) {
    const session = state.selectedTmuxSession;
    const window = state.selectedTmuxWindow;
    if (!session || window == null) return;
    try {
      await assignToPane({
        tmuxSession: session,
        tmuxWindow: window,
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject,
        projectPath,
        targetPaneIndex: paneIndex,
      });
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[App] onDragEnd error:", e);
    }
  }

  /** Detects if a sidebar project card (not a pin or tab) is being dragged. */
  function AppShell(props: { children?: any }) {
    const { pendingLaunch, cancelPanePick } = useApp();
    const dndCtx = useDragDropContext();
    const isDraggingProject = () => {
      const id = dndCtx?.[0]?.active?.draggable?.id;
      if (!id) return false;
      const sid = String(id);
      return !sid.startsWith(PIN_PREFIX)
        && !sid.startsWith(SESSION_TAB_PREFIX)
        && !sid.startsWith(WINDOW_TAB_PREFIX)
        && !sid.startsWith(PANE_SLOT_PREFIX)
        && !sid.startsWith(NEW_PROJECT_PREFIX)
        && !sid.startsWith(NEW_PROJECT_CONTINUITY_PREFIX);
    };

    const isPaneSelectMode = () => pendingLaunch() != null;
    const dimActive = () => isDraggingProject() || isPaneSelectMode();

    const paneSelectLabel = () => {
      const pl = pendingLaunch();
      if (!pl) return "";
      const name = pl.project.meta.display_name || deriveName(pl.project.actual_path);
      return `Select a pane for ${name}`;
    };

    // Escape key cancels pane-select mode
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape" && isPaneSelectMode()) {
        cancelPanePick();
      }
    }

    onMount(() => document.addEventListener("keydown", handleKeyDown));
    onCleanup(() => document.removeEventListener("keydown", handleKeyDown));

    return (
      <div class={`app-shell ${dimActive() ? "dragging-project" : ""}`}>
        <TopBar />
        <div class="app-body">
          <Sidebar width={sidebarWidth()} />
          <div class="sidebar-resize-handle" onMouseDown={handleResizeStart} />
          <MainArea />
        </div>
        {/* Pane select hint — no blocking overlay, pane clicks go through naturally */}
        <Show when={isPaneSelectMode()}>
          <div class="pane-select-hint">
            {paneSelectLabel()}
            <button class="pane-select-cancel" onClick={cancelPanePick}>{"\u2715"}</button>
          </div>
        </Show>
        {props.children}
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
          // New project ghost card (check continuity prefix first — it's longer)
          if (rawId.startsWith(NEW_PROJECT_CONTINUITY_PREFIX)) {
            const path = rawId.slice(NEW_PROJECT_CONTINUITY_PREFIX.length);
            return <div class="drag-overlay-card new-project">{deriveName(path)}</div>;
          }
          if (rawId.startsWith(NEW_PROJECT_PREFIX)) {
            const path = rawId.slice(NEW_PROJECT_PREFIX.length);
            return <div class="drag-overlay-card new-project">{deriveName(path)}</div>;
          }
          const encodedName = rawId.startsWith(PIN_PREFIX)
            ? rawId.slice(PIN_PREFIX.length)
            : rawId;
          const project = state.projects.find(
            (p) => p.encoded_name === encodedName,
          );
          const name = project
            ? project.meta.display_name || deriveName(project.actual_path)
            : encodedName;
          return <div class="drag-overlay-card">{name}</div>;
        }}
      </DragOverlay>

      {/* Confirm replace active pane modal */}
      <Show when={pendingPaneDrop()}>
        <div class="modal-backdrop" onClick={() => setPendingPaneDrop(null)}>
          <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
            <p class="confirm-message">
              <strong>Pane {pendingPaneDrop()!.paneIndex}</strong> already has an active project:
              <br /><em>{pendingPaneDrop()!.existingName}</em>
            </p>
            <p class="confirm-warning">
              Replacing it will send Ctrl+C and reassign the pane.
            </p>
            <div class="confirm-actions">
              <button class="modal-btn" onClick={() => setPendingPaneDrop(null)}>
                Cancel
              </button>
              <button
                class="modal-btn danger"
                onClick={async () => {
                  const drop = pendingPaneDrop()!;
                  setPendingPaneDrop(null);
                  await executePaneDrop(drop.encodedProject, drop.projectPath, drop.paneIndex);
                }}
              >
                Replace
              </button>
            </div>
          </div>
        </div>
      </Show>

      {/* Central project settings modal — opened from any component via context */}
      <Show when={settingsProject()}>
        <ProjectDetailModal
          project={settingsProject()!}
          onClose={closeProjectSettings}
        />
      </Show>

      {/* Toast stack — globally accessible via showToast()/toastError() */}
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
