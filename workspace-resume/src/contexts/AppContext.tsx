import {
  createContext,
  createSignal,
  useContext,
  onMount,
  onCleanup,
  batch,
} from "solid-js";
import type { JSX } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import { fromWslPath } from "../lib/path";
import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  listProjects,
  getAllProjectMeta,
  getActiveSessions,
  listTmuxSessions,
  listTmuxWindows,
  getTmuxState,
  getPaneAssignments,
  getPanePresets,
  getInode,
  findInodeInTree,
  updateProjectInode,
  switchTmuxSession,
  selectTmuxWindowCmd,
  getSessionOrder,
  setSessionOrder as setSessionOrderCmd,
  getPinnedOrder,
  setPinnedOrder as setPinnedOrderCmd,
  swapTmuxWindow,
  checkPaneStatuses,
} from "../lib/tauri-commands";
import type {
  ProjectWithMeta,
  ProjectMeta,
  ProjectTier,
  TmuxSession,
  TmuxWindow,
  TmuxPane,
  PanePreset,
  ActiveSession,
  WindowPaneStatus,
} from "../lib/types";

// ---------------------------------------------------------------------------
// State & context types
// ---------------------------------------------------------------------------

interface AppState {
  projects: ProjectWithMeta[];
  selectedTmuxSession: string | null;
  selectedTmuxWindow: number | null;
  tmuxSessions: TmuxSession[];
  tmuxWindows: TmuxWindow[];
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  /** Full pane-slot assignment records keyed by pane index (as string).
   *  Populated alongside `paneAssignments`. Carries the host/account
   *  fields that the flat map elides. Slice A introduced the struct
   *  server-side; Slice C surfaces it to the UI for dropdowns. */
  paneAssignmentsFull: Record<string, import("../lib/types").PaneAssignment>;
  panePresets: PanePreset[];
  activeSessions: ActiveSession[];
  sessionOrder: string[];
  pinnedOrder: string[];
  windowStatuses: Record<string, WindowPaneStatus>;
}

/** Config for a launch that's waiting for the user to pick a pane. */
export interface PendingLaunch {
  project: ProjectWithMeta;
  mode: "resume" | "new";
  yolo?: boolean;
  continuity?: boolean;
  sessionId?: string | null;
}

interface AppContextValue {
  state: AppState;
  // Selection
  selectTmuxSession: (name: string) => void;
  selectTmuxWindow: (index: number) => void;
  // Refresh
  refreshProjects: () => void;
  refreshTmuxState: () => void;
  refreshPanePresets: () => void;
  // Polling control
  pausePolling: () => void;
  resumePolling: () => void;
  // Tab reordering
  reorderSessions: (fromName: string, toName: string) => void;
  reorderWindows: (fromIndex: number, toIndex: number) => void;
  reorderPinned: (fromName: string, toName: string) => void;
  // Project settings modal
  openProjectSettings: (project: ProjectWithMeta, fromPane?: number) => void;
  closeProjectSettings: () => void;
  settingsProject: () => ProjectWithMeta | null;
  settingsFromPane: () => number | null;
  // Pane picker
  pendingLaunch: () => PendingLaunch | null;
  startPanePick: (launch: PendingLaunch) => void;
  cancelPanePick: () => void;
  // Notification muting
  mutePane: (sessionName: string, windowIndex: number, paneIndex: number) => void;
  unmutePane: (sessionName: string, windowIndex: number, paneIndex: number) => void;
  isPaneMuted: (sessionName: string, windowIndex: number, paneIndex: number) => boolean;
  // Derived getters
  projectsByTier: (tier: ProjectTier) => ProjectWithMeta[];
  getProjectMeta: (encodedName: string) => ProjectMeta;
  isProjectActive: (encodedName: string) => boolean;
  isProjectActiveInSession: (encodedName: string) => boolean;
  isProjectWaitingInSession: (encodedName: string) => boolean;
  findProjectWindow: (encodedName: string) => number | null;
  activeProjectCount: () => number;
}

const DEFAULT_META: ProjectMeta = {
  display_name: null,
  tier: "active",
  bound_session: null,
};

const AppContext = createContext<AppContextValue>();

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

export function AppProvider(props: { children: JSX.Element }) {
  const [state, setState] = createStore<AppState>({
    projects: [],
    selectedTmuxSession: null,
    selectedTmuxWindow: null,
    tmuxSessions: [],
    tmuxWindows: [],
    tmuxPanes: [],
    paneAssignments: {},
    paneAssignmentsFull: {},
    panePresets: [],
    activeSessions: [],
    sessionOrder: [],
    pinnedOrder: [],
    windowStatuses: {},
  });

  // -- Helpers ---------------------------------------------------------------

  async function loadProjectsWithMeta() {
    try {
      const [projectList, metaMap] = await Promise.all([
        listProjects(),
        getAllProjectMeta(),
      ]);
      const merged: ProjectWithMeta[] = projectList.map((p) => ({
        ...p,
        meta: metaMap[p.encoded_name] ?? { ...DEFAULT_META },
      }));
      // Use `reconcile` with `encoded_name` as the identity key so SolidJS
      // diffs the new list against existing store entries. Without this,
      // every poll replaced the array reference, causing <For> in the
      // sidebar to unmount/remount every ProjectCard — which re-triggered
      // getProjectUsage for every project each cycle and re-parsed the
      // JSONL session files from scratch (visible to the user as the
      // cost/tokens/msgs counters flickering on every refresh).
      setState("projects", reconcile(merged, { key: "encoded_name" }));
    } catch (e) {
      console.error("[AppContext] loadProjectsWithMeta error:", e);
    }
  }

  /**
   * F-61: Backfill inodes for projects that don't have them yet,
   * and scan for orphaned projects (path_exists=false) to re-link.
   * Runs once after initial project load.
   */
  async function reconcileProjectInodes() {
    const projects = state.projects;
    if (projects.length === 0) return;

    for (const project of projects) {
      const meta = project.meta;

      if (!meta.inode) {
        // Backfill: no inode stored yet — try via WSL stat
        try {
          const inode = await getInode(project.actual_path);
          if (inode) {
            await updateProjectInode(project.encoded_name, inode, null);
          }
          // If inode is null, path truly doesn't exist — but we don't mark as
          // unlinked until we have a PREVIOUS inode to compare against
        } catch (e) {
          console.warn(`[F-61] Failed to backfill inode for ${project.encoded_name}:`, e);
        }
        continue;
      }

      // We have a stored inode — verify the path still resolves
      try {
        const currentInode = await getInode(project.actual_path);
        if (currentInode) {
          // Path still exists — all good, no action needed
          continue;
        }
      } catch (_) {}

      // Path no longer resolves — this is a genuine orphan. Run escalating scan.
      try {
        const parentPath = project.actual_path.replace(/\/[^/]+\/?$/, "");
        let found = await findInodeInTree(parentPath, meta.inode, 1);

        // Escalating scan: derive search roots from the project's own path
        // instead of hardcoding user-specific directories
        if (!found) {
          const grandparent = parentPath.replace(/\/[^/]+\/?$/, "");
          if (grandparent && grandparent !== parentPath) {
            found = await findInodeInTree(grandparent, meta.inode, 5);
          }
        }

        if (!found) {
          const greatGrandparent = parentPath.replace(/\/[^/]+\/?$/, "").replace(/\/[^/]+\/?$/, "");
          if (greatGrandparent && greatGrandparent.length > 6) {
            found = await findInodeInTree(greatGrandparent, meta.inode, 6);
          }
        }

        if (found) {
          const existingDirs = meta.claude_project_dirs ?? [];
          if (!existingDirs.includes(project.encoded_name)) {
            existingDirs.push(project.encoded_name);
          }
          await updateProjectInode(project.encoded_name, meta.inode, existingDirs);
        } else {
          // Mark as confirmed unlinked by setting claude_project_dirs to empty
          // (distinct from null/undefined which means "never checked")
          await updateProjectInode(project.encoded_name, meta.inode, []);
        }
      } catch (e) {
        console.warn(`[F-61] Scan failed for ${project.encoded_name}:`, e);
      }
    }

    // Refresh projects to pick up any changes
    loadProjectsWithMeta();
  }

  async function loadTmuxSessions() {
    try {
      const sessions = await listTmuxSessions();
      setState("tmuxSessions", sessions);

      if (sessions.length === 0) return;

      // Auto-select the attached session if any; otherwise fall back to the first
      const attached = sessions.find((s) => s.attached);
      const target = attached ?? sessions[0];

      await selectTmuxSessionInternal(target.name);
    } catch (e) {
      // "no tmux server running" is not an error, just empty state
      console.warn("[AppContext] loadTmuxSessions:", e);
      setState("tmuxSessions", []);
    }
  }

  async function selectTmuxSessionInternal(name: string) {
    setState("selectedTmuxSession", name);
    try {
      const windows = await listTmuxWindows(name);
      setState("tmuxWindows", windows);
      if (windows.length > 0) {
        const firstWindow = windows[0];
        setState("selectedTmuxWindow", firstWindow.index);
        await loadTmuxPanes(name, firstWindow.index);
      } else {
        batch(() => {
          setState("selectedTmuxWindow", null);
          setState("tmuxPanes", []);
        });
      }
    } catch (e) {
      console.error("[AppContext] selectTmuxSession error:", e);
      batch(() => {
        setState("tmuxWindows", []);
        setState("selectedTmuxWindow", null);
        setState("tmuxPanes", []);
      });
    }
  }

  async function loadTmuxPanes(sessionName: string, windowIndex: number) {
    try {
      const tmuxState = await getTmuxState(sessionName, windowIndex);

      // F-52: Auto-follow active tmux window.
      // If the user switched windows in tmux, sync the dashboard to match.
      // Suppressed for 2s after a user-initiated tab click to avoid race conditions.
      const activeWin = tmuxState.windows.find((w) => w.active);
      if (activeWin && activeWin.index !== windowIndex && Date.now() > autoFollowSuppressedUntil) {
        // tmux's active window differs — switch to it and load its panes
        setState("tmuxSessions", tmuxState.sessions);
        setState("tmuxWindows", tmuxState.windows);
        setState("selectedTmuxWindow", activeWin.index);
        // Re-fetch panes for the newly active window
        const freshState = await getTmuxState(sessionName, activeWin.index);
        batch(() => {
          setState("tmuxPanes", freshState.panes);
        });
        loadPaneAssignments();
        pollPaneStatuses();
        return;
      }

      batch(() => {
        setState("tmuxSessions", tmuxState.sessions);
        setState("tmuxWindows", tmuxState.windows);
        setState("tmuxPanes", tmuxState.panes);
      });
    } catch (e) {
      console.error("[AppContext] loadTmuxPanes error:", e);
      setState("tmuxPanes", []);
    }
  }

  async function loadPaneAssignments() {
    const session = state.selectedTmuxSession;
    const window = state.selectedTmuxWindow;
    if (session == null || window == null) {
      setState("paneAssignments", reconcile({}));
      setState("paneAssignmentsFull", reconcile({}));
      return;
    }
    try {
      const assignments = await getPaneAssignments(session, window);
      // reconcile replaces the entire object — without it, removed keys
      // (e.g. after unassign) would persist because setState merges by default.
      setState("paneAssignments", reconcile(assignments));
    } catch (e) {
      console.error("[AppContext] loadPaneAssignments error:", e);
    }
    // Fetch the struct-shape sibling so PaneSlot can read host/account.
    // Separate round-trip keeps existing callers of `paneAssignments`
    // untouched and lets the struct load fail independently.
    try {
      const { getPaneAssignmentsFull } = await import("../lib/tauri-commands");
      const full = await getPaneAssignmentsFull(session, window);
      setState("paneAssignmentsFull", reconcile(full));
    } catch (e) {
      console.error("[AppContext] loadPaneAssignmentsFull error:", e);
      setState("paneAssignmentsFull", reconcile({}));
    }
  }

  async function loadPanePresets() {
    try {
      const presets = await getPanePresets();
      setState("panePresets", presets);
    } catch (e) {
      console.error("[AppContext] loadPanePresets error:", e);
    }
  }

  async function pollActiveSessions() {
    try {
      const result = await getActiveSessions();
      setState("activeSessions", result);
    } catch (e) {
      console.error("[AppContext] pollActiveSessions error:", e);
    }
  }

  // -- Notification muting ---------------------------------------------------
  // Muted panes are tracked by key "session|window|pane_index".
  // Muted panes are filtered out of waiting_panes before storing in state,
  // so window tabs, pinned pills, and pane slots all naturally see "not waiting".
  // Auto-unmutes when the pane stops waiting (agent resumed).

  const mutedPanes = new Set<string>();
  const prevWaiting = new Map<string, boolean>(); // track previous waiting state for auto-unmute

  function muteKey(session: string, window: number, pane: number): string {
    return `${session}|${window}|${pane}`;
  }

  function mutePane(session: string, window: number, pane: number) {
    mutedPanes.add(muteKey(session, window, pane));
    // Re-filter current state immediately
    pollPaneStatuses();
  }

  function unmutePane(session: string, window: number, pane: number) {
    mutedPanes.delete(muteKey(session, window, pane));
    pollPaneStatuses();
  }

  function isPaneMuted(session: string, window: number, pane: number): boolean {
    return mutedPanes.has(muteKey(session, window, pane));
  }

  async function pollPaneStatuses() {
    if (pollingPaused) return;
    const session = state.selectedTmuxSession;
    if (!session) return;
    try {
      const statuses = await checkPaneStatuses(session);

      // Auto-unmute panes that are no longer waiting
      for (const key of mutedPanes) {
        const [sess, win, pane] = key.split("|");
        const winStatus = statuses[win];
        const stillWaiting = winStatus?.waiting_panes?.includes(Number(pane)) ?? false;
        if (!stillWaiting) {
          mutedPanes.delete(key);
        }
      }

      // Filter muted panes out of waiting_panes before storing
      for (const [winIdx, status] of Object.entries(statuses)) {
        status.waiting_panes = status.waiting_panes.filter(
          (paneIdx) => !mutedPanes.has(muteKey(session, Number(winIdx), paneIdx))
        );
      }

      setState("windowStatuses", reconcile(statuses));
    } catch (e) {
      // Silently ignore — tmux may not be running
    }
  }

  // -- Polling control -------------------------------------------------------

  let pollingPaused = false;

  function pausePolling() {
    pollingPaused = true;
  }

  function resumePolling() {
    pollingPaused = false;
  }

  // -- Pane picker (select a pane to launch into) ----------------------------

  const [pendingLaunch, setPendingLaunch] = createSignal<PendingLaunch | null>(null);

  function startPanePick(launch: PendingLaunch) {
    // Close settings modal if open so the pane grid is visible
    setSettingsProject(null);
    setPendingLaunch(launch);
    pausePolling();
  }

  function cancelPanePick() {
    setPendingLaunch(null);
    resumePolling();
  }

  // -- Project settings modal ------------------------------------------------

  const [settingsProject, setSettingsProject] = createSignal<ProjectWithMeta | null>(null);
  const [settingsFromPane, setSettingsFromPane] = createSignal<number | null>(null);

  function openProjectSettings(project: ProjectWithMeta, fromPane?: number) {
    setSettingsProject(project);
    setSettingsFromPane(fromPane ?? null);
    pausePolling();
  }

  function closeProjectSettings() {
    setSettingsProject(null);
    setSettingsFromPane(null);
    resumePolling();
  }

  // -- Public actions -------------------------------------------------------

  function refreshProjects() {
    loadProjectsWithMeta();
  }

  function refreshTmuxState() {
    if (pollingPaused) return;
    const session = state.selectedTmuxSession;
    const window = state.selectedTmuxWindow;
    if (session != null && window != null) {
      loadTmuxPanes(session, window);
      loadPaneAssignments();
    }
  }

  function refreshPanePresets() {
    loadPanePresets();
  }

  async function loadSessionOrder() {
    try {
      const order = await getSessionOrder();
      setState("sessionOrder", order);
    } catch (e) {
      console.error("[AppContext] loadSessionOrder error:", e);
    }
  }

  function reorderSessions(fromName: string, toName: string) {
    // Build current display order (stored order first, then any new sessions)
    const known = new Set(state.sessionOrder);
    const currentOrder = [
      ...state.sessionOrder.filter((n) => state.tmuxSessions.some((s) => s.name === n)),
      ...state.tmuxSessions.filter((s) => !known.has(s.name)).map((s) => s.name),
    ];
    const fromIdx = currentOrder.indexOf(fromName);
    const toIdx = currentOrder.indexOf(toName);
    if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return;
    const updated = [...currentOrder];
    const [moved] = updated.splice(fromIdx, 1);
    updated.splice(toIdx, 0, moved);
    setState("sessionOrder", updated);
    setSessionOrderCmd(updated).catch((e) =>
      console.error("[AppContext] setSessionOrder:", e),
    );
  }

  async function loadPinnedOrder() {
    try {
      const order = await getPinnedOrder();
      setState("pinnedOrder", order);
    } catch (e) {
      console.error("[AppContext] loadPinnedOrder error:", e);
    }
  }

  function reorderPinned(fromName: string, toName: string) {
    const pinned = projectsByTier("pinned");
    const known = new Set(state.pinnedOrder);
    const currentOrder = [
      ...state.pinnedOrder.filter((n) => pinned.some((p) => p.encoded_name === n)),
      ...pinned.filter((p) => !known.has(p.encoded_name)).map((p) => p.encoded_name),
    ];
    const fromIdx = currentOrder.indexOf(fromName);
    const toIdx = currentOrder.indexOf(toName);
    if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return;
    const updated = [...currentOrder];
    const [moved] = updated.splice(fromIdx, 1);
    updated.splice(toIdx, 0, moved);
    setState("pinnedOrder", updated);
    setPinnedOrderCmd(updated).catch((e) =>
      console.error("[AppContext] setPinnedOrder:", e),
    );
  }

  function reorderWindows(fromIndex: number, toIndex: number) {
    const sess = state.selectedTmuxSession;
    if (!sess || fromIndex === toIndex) return;
    swapTmuxWindow(sess, fromIndex, toIndex)
      .then(() => refreshTmuxState())
      .catch((e) => console.error("[AppContext] swapTmuxWindow:", e));
  }

  function selectTmuxSession(name: string) {
    selectTmuxSessionInternal(name);
    // Also switch the actual tmux client to this session
    switchTmuxSession(name).catch((e) =>
      console.warn("[AppContext] switchTmuxSession:", e),
    );
  }

  // Grace period after user-initiated window switch — suppresses auto-follow
  // so the poll doesn't snap back before tmux catches up.
  let autoFollowSuppressedUntil = 0;

  function selectTmuxWindow(index: number) {
    setState("selectedTmuxWindow", index);
    autoFollowSuppressedUntil = Date.now() + 2000; // suppress auto-follow for 2s
    const session = state.selectedTmuxSession;
    if (session != null) {
      loadTmuxPanes(session, index);
      loadPaneAssignments();
      pollPaneStatuses();
      // Also switch the active window in tmux
      selectTmuxWindowCmd(session, index).catch((e) =>
        console.warn("[AppContext] selectTmuxWindow:", e),
      );
    }
  }

  function projectsByTier(tier: ProjectTier): ProjectWithMeta[] {
    return state.projects.filter((p) => p.meta.tier === tier);
  }

  function getProjectMeta(encodedName: string): ProjectMeta {
    const found = state.projects.find((p) => p.encoded_name === encodedName);
    return found?.meta ?? { ...DEFAULT_META };
  }

  function isProjectActive(encodedName: string): boolean {
    for (const [paneIdx, assignedProject] of Object.entries(state.paneAssignments)) {
      if (assignedProject !== encodedName) continue;
      const pane = state.tmuxPanes.find((p) => p.pane_index === Number(paneIdx));
      if (pane && pane.current_command.toLowerCase().includes("claude")) {
        return true;
      }
    }
    return false;
  }

  /** Match a project's path against an active pane path (handles WSL/Windows formats). */
  function pathMatchesProject(activePath: string, actualPath: string): boolean {
    const actualLower = actualPath.toLowerCase().replace(/[\\/]+$/, "");
    const pathLower = activePath.toLowerCase().replace(/\/+$/, "");
    if (actualLower === pathLower) return true;
    if (actualLower === fromWslPath(activePath).toLowerCase().replace(/[\\/]+$/, "")) return true;
    return false;
  }

  /** Check if a project has Claude running in ANY window of the session. */
  function isProjectActiveInSession(encodedName: string): boolean {
    const proj = state.projects.find((p) => p.encoded_name === encodedName);
    if (!proj) return false;
    for (const status of Object.values(state.windowStatuses)) {
      if ((status.active_paths ?? []).some((p) => pathMatchesProject(p, proj.actual_path))) return true;
    }
    return false;
  }

  /** Check if a project has a pane waiting for approval in ANY window. */
  function isProjectWaitingInSession(encodedName: string): boolean {
    const proj = state.projects.find((p) => p.encoded_name === encodedName);
    if (!proj) return false;
    for (const status of Object.values(state.windowStatuses)) {
      const paths = status.active_paths ?? [];
      const panes = status.active_panes ?? [];
      const waiting = status.waiting_panes ?? [];
      for (let i = 0; i < paths.length; i++) {
        if (pathMatchesProject(paths[i], proj.actual_path) && waiting.includes(panes[i])) return true;
      }
    }
    return false;
  }

  /** Find the window index where a project is active. Returns the first match, preferring waiting windows. */
  function findProjectWindow(encodedName: string): number | null {
    const proj = state.projects.find((p) => p.encoded_name === encodedName);
    if (!proj) return null;
    let firstActive: number | null = null;
    for (const [winIdx, status] of Object.entries(state.windowStatuses)) {
      const paths = status.active_paths ?? [];
      const panes = status.active_panes ?? [];
      const waiting = status.waiting_panes ?? [];
      for (let i = 0; i < paths.length; i++) {
        if (pathMatchesProject(paths[i], proj.actual_path)) {
          if (waiting.includes(panes[i])) return Number(winIdx); // prefer waiting window
          if (firstActive == null) firstActive = Number(winIdx);
        }
      }
    }
    return firstActive;
  }

  function activeProjectCount(): number {
    let total = 0;
    for (const status of Object.values(state.windowStatuses)) {
      total += status.active_panes?.length ?? 0;
    }
    return total;
  }

  // -- Lifecycle ------------------------------------------------------------

  let tmuxPollInterval: ReturnType<typeof setInterval> | undefined;
  let activeSessionInterval: ReturnType<typeof setInterval> | undefined;
  let statusPollInterval: ReturnType<typeof setInterval> | undefined;
  let unlistenSessionChanged: UnlistenFn | undefined;

  onMount(async () => {
    // Load initial data in parallel
    await Promise.all([
      loadProjectsWithMeta(),
      loadTmuxSessions(),
      loadPaneAssignments(),
      loadPanePresets(),
      loadSessionOrder(),
      loadPinnedOrder(),
    ]);

    // F-61: Backfill inodes + scan for orphaned projects (runs in background)
    reconcileProjectInodes();

    // Main tmux poll — combines refresh + status check + project reload in
    // one interval. Cadence bumped from 3s to 10s after observing that
    // triple-polling every 3s saturated the Rust backend with wsl.exe
    // invocations (each ~150-200ms of CPU on Windows) and starved the
    // companion's HTTP runtime under concurrent mobile load. Live updates
    // on the desktop come primarily from tmux-poller's WebSocket broadcast;
    // this interval is now a 10s fallback.
    const runPollCycle = () => {
      refreshTmuxState();
      loadProjectsWithMeta();
      pollPaneStatuses();
    };
    runPollCycle();
    tmuxPollInterval = setInterval(runPollCycle, 10000);

    // Fast status poll — pane active/waiting detection runs every 1.5s so
    // running/waiting chips update near-instantly. The heavy project/tmux
    // reload still happens on the 10s cycle above.
    statusPollInterval = setInterval(() => {
      pollPaneStatuses();
    }, 1500);

    // Listen for session file changes from Tauri file watcher
    unlistenSessionChanged = await listen<string[]>("session-changed", () => {
      loadProjectsWithMeta();
    });
  });

  onCleanup(() => {
    if (tmuxPollInterval) clearInterval(tmuxPollInterval);
    if (statusPollInterval) clearInterval(statusPollInterval);
    if (unlistenSessionChanged) unlistenSessionChanged();
  });

  // -- Context value --------------------------------------------------------

  const contextValue: AppContextValue = {
    state,
    selectTmuxSession,
    selectTmuxWindow,
    refreshProjects,
    refreshTmuxState,
    refreshPanePresets,
    pausePolling,
    resumePolling,
    openProjectSettings,
    closeProjectSettings,
    settingsProject,
    settingsFromPane,
    pendingLaunch,
    startPanePick,
    cancelPanePick,
    mutePane,
    unmutePane,
    isPaneMuted,
    reorderSessions,
    reorderWindows,
    reorderPinned,
    projectsByTier,
    getProjectMeta,
    isProjectActive,
    isProjectActiveInSession,
    isProjectWaitingInSession,
    findProjectWindow,
    activeProjectCount,
  };

  return (
    <AppContext.Provider value={contextValue}>
      {props.children}
    </AppContext.Provider>
  );
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp must be used within AppProvider");
  return ctx;
}
