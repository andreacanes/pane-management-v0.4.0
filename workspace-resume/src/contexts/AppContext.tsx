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
import { pathMatchesProject } from "../lib/path";
import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  listProjects,
  getAllProjectMeta,
  listTmuxSessions,
  listTmuxWindows,
  getTmuxState,
  getAllPaneAssignmentsFull,
  listTmuxPanesAllOn,
  listRemoteHosts,
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
  /** Panes rendered in the grid. After the host-aware refactor this is
   *  a union of (1) the local WSL tmux session:window selected above
   *  and (2) any remote-host panes that are running Claude — one grid
   *  spans multiple tmux servers. Each pane self-describes its host,
   *  session_name, and window_index; consumers never infer coordinates
   *  from the outer `selectedTmux*` ambient context. */
  tmuxPanes: TmuxPane[];
  /** Encoded-project map keyed on the full 4-segment coord
   *  `"host|session|window|pane"`. Swapped from the old pane-index keys
   *  so a local `main:0.3` and a Mac `main:0.3` don't collide. */
  paneAssignments: Record<string, string>;
  /** Full assignment records keyed on the same 4-segment coord. Carries
   *  host/account alongside the encoded_project. */
  paneAssignmentsFull: Record<string, import("../lib/types").PaneAssignment>;
  /** SSH aliases we poll for remote panes. Seeded at mount from
   *  `listRemoteHosts` (Rust-side static for MVP: `["mac"]`). */
  remoteHosts: string[];
  panePresets: PanePreset[];
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
  /** Resolve a pending launch against the user-picked target pane.
   *  Pane carries host/session/window so the launch routes to the
   *  right tmux server (local or Mac). */
  completePanePick: (pane: TmuxPane) => void;
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
    remoteHosts: [],
    panePresets: [],
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

  /** Heuristic: does this pane's foreground or start command look like
   *  Claude Code? Mirrors the Rust `pane_is_claude` check — substring
   *  match on "claude"/"cld" covers the stock binary, the patched
   *  cli-{n,mn}cld-*.bin blobs, and every shell wrapper alias
   *  (cld/cld2/cld3/ncld/ncld2/ncld3/mcld/mncld). Used to filter
   *  remote panes before merging into `state.tmuxPanes`. */
  function looksLikeClaude(pane: TmuxPane): boolean {
    const cur = (pane.current_command ?? "").toLowerCase();
    const start = (pane.start_command ?? "").toLowerCase();
    return (
      cur.includes("claude") ||
      cur.includes("cld") ||
      start.includes("claude") ||
      start.includes("cld")
    );
  }

  async function loadTmuxPanes(sessionName: string, windowIndex: number) {
    try {
      const tmuxState = await getTmuxState(sessionName, windowIndex);

      // F-52: Auto-follow active tmux window.
      // If the user switched windows in tmux, sync the dashboard to match.
      // Suppressed for 2s after a user-initiated tab click to avoid race conditions.
      const activeWin = tmuxState.windows.find((w) => w.active);
      if (activeWin && activeWin.index !== windowIndex && Date.now() > autoFollowSuppressedUntil) {
        setState("tmuxSessions", tmuxState.sessions);
        setState("tmuxWindows", tmuxState.windows);
        setState("selectedTmuxWindow", activeWin.index);
        const freshState = await getTmuxState(sessionName, activeWin.index);
        const remote = await loadRemotePanes();
        batch(() => {
          setState("tmuxPanes", [...freshState.panes, ...remote]);
        });
        loadPaneAssignments();
        pollPaneStatuses();
        return;
      }

      // Render local panes immediately so the grid doesn't wait on SSH.
      batch(() => {
        setState("tmuxSessions", tmuxState.sessions);
        setState("tmuxWindows", tmuxState.windows);
        setState("tmuxPanes", tmuxState.panes);
      });

      // Then fetch remote panes and merge. Keeps the grid responsive
      // even when a Mac is slow to answer — the Mac section appears
      // ~50-500 ms later without blocking the local render path.
      const remote = await loadRemotePanes();
      if (remote.length > 0) {
        setState("tmuxPanes", [...tmuxState.panes, ...remote]);
      }
    } catch (e) {
      console.error("[AppContext] loadTmuxPanes error:", e);
      setState("tmuxPanes", []);
    }
  }

  // Per-host health tracker: did the last poll succeed or fail? Used to
  // detect ok→fail / fail→ok transitions so we can surface them once
  // (toast + error-log entry) rather than repeating every 10 s tick.
  const remoteHealth: Map<string, { ok: boolean }> = new Map();

  /** Fetch every pane on every remote host and filter to the ones that
   *  should appear in the grid: panes running Claude AND panes with an
   *  active assignment. Shell-only Mac sessions the user spun up for
   *  unrelated work stay hidden, matching the plan's "running Claude
   *  OR has assignment" discovery rule.
   *
   *  Surfaces connectivity changes: when a host transitions from
   *  reachable to unreachable we emit a toast + error_log entry so the
   *  user sees the Mac is offline rather than silently losing its
   *  panes from the grid. */
  async function loadRemotePanes(): Promise<TmuxPane[]> {
    const hosts = state.remoteHosts;
    if (hosts.length === 0) return [];
    const assigned = state.paneAssignmentsFull;
    const out: TmuxPane[] = [];
    for (const host of hosts) {
      try {
        const panes = await listTmuxPanesAllOn(host);
        for (const p of panes) {
          const assignKey = `${p.host}|${p.session_name}|${p.window_index}|${p.pane_index}`;
          if (looksLikeClaude(p) || assignKey in assigned) {
            out.push(p);
          }
        }
        const prev = remoteHealth.get(host);
        if (prev && !prev.ok) {
          const { toastSuccess } = await import("../components/ui/Toast");
          toastSuccess(`${host} reachable again`, "Remote panes restored");
        }
        remoteHealth.set(host, { ok: true });
      } catch (e) {
        const prev = remoteHealth.get(host);
        if (!prev || prev.ok) {
          const { toastError } = await import("../components/ui/Toast");
          toastError(
            `${host} unreachable`,
            "Remote panes hidden until the host is back",
          );
          console.warn(`[AppContext] loadRemotePanes ${host} failed:`, e);
        } else {
          // Still down — keep quiet to avoid toast spam.
          console.debug(`[AppContext] loadRemotePanes ${host} still failing:`, e);
        }
        remoteHealth.set(host, { ok: false });
      }
    }
    return out;
  }

  async function loadPaneAssignments() {
    try {
      // Fetch every assignment at once. 4-segment keys mean one flat map
      // covers every (host, session, window, pane) slot across the grid.
      // `reconcile` replaces the entire object so removed keys actually
      // disappear instead of lingering as stale entries.
      const full = await getAllPaneAssignmentsFull();
      const flat: Record<string, string> = {};
      for (const [key, entry] of Object.entries(full)) {
        flat[key] = entry.encoded_project;
      }
      batch(() => {
        setState("paneAssignments", reconcile(flat));
        setState("paneAssignmentsFull", reconcile(full));
      });
    } catch (e) {
      console.error("[AppContext] loadPaneAssignments error:", e);
      batch(() => {
        setState("paneAssignments", reconcile({}));
        setState("paneAssignmentsFull", reconcile({}));
      });
    }
  }

  /** Seed the remoteHosts state at mount so `loadRemotePanes` has targets
   *  to poll. Static for MVP (`["mac"]`); future work may migrate this
   *  into a user-editable `remote_hosts` Tauri store key. */
  async function loadRemoteHosts() {
    try {
      const hosts = await listRemoteHosts();
      setState("remoteHosts", hosts);
    } catch (e) {
      console.warn("[AppContext] loadRemoteHosts failed:", e);
      setState("remoteHosts", []);
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

  /**
   * Resolve a pending launch against a user-picked pane. Reads the
   * pending launch config (project + resume/new + optional sessionId/yolo),
   * routes to the correct helper (`launchToPane` for resume,
   * `newSessionInPane` for fresh) with the target pane's host/session/
   * window/pane_index — so Mac panes get `mncld` and Mac paths, local
   * panes get `ncld` and WSL paths. No-ops when pendingLaunch is null.
   *
   * This replaces the broken `launch.execute(paneIndex)` pattern that
   * referenced a property never defined on PendingLaunch.
   */
  async function completePanePick(pane: TmuxPane) {
    const launch = pendingLaunch();
    if (!launch) return;
    // Clear pending first — a slow launch shouldn't keep the grid in
    // "select mode" if the user clicks somewhere else in the meantime.
    setPendingLaunch(null);
    resumePolling();
    try {
      const { launchToPane, newSessionInPane } = await import("../lib/launch");
      const host = pane.host || "local";
      const session = pane.session_name || state.selectedTmuxSession || "";
      const windowIndex = pane.window_index ?? state.selectedTmuxWindow ?? 0;
      const common = {
        tmuxSession: session,
        tmuxWindow: windowIndex,
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject: launch.project.encoded_name,
        projectPath: launch.project.actual_path,
        targetPaneIndex: pane.pane_index,
        host,
        yolo: launch.yolo,
      };
      if (launch.mode === "resume") {
        await launchToPane({
          ...common,
          sessionId: launch.sessionId ?? null,
          boundSession: launch.project.meta?.bound_session ?? null,
        });
      } else {
        await newSessionInPane(common);
      }
      loadTmuxPanes(
        state.selectedTmuxSession ?? session,
        state.selectedTmuxWindow ?? windowIndex,
      );
      loadPaneAssignments();
    } catch (e) {
      console.error("[AppContext] completePanePick error:", e);
    }
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
    for (const [key, assignedProject] of Object.entries(state.paneAssignments)) {
      if (assignedProject !== encodedName) continue;
      // 4-segment key: "host|session|window|pane" — find the matching
      // pane by full coord so a local `main:0.3` and a Mac `main:0.3`
      // don't alias against each other.
      const parts = key.split("|");
      if (parts.length !== 4) continue;
      const [host, sessionName, winStr, paneStr] = parts;
      const windowIdx = Number(winStr);
      const paneIdx = Number(paneStr);
      if (Number.isNaN(windowIdx) || Number.isNaN(paneIdx)) continue;
      const pane = state.tmuxPanes.find(
        (p) =>
          p.pane_index === paneIdx &&
          (p.host || "local") === host &&
          p.session_name === sessionName &&
          p.window_index === windowIdx,
      );
      if (!pane) continue;
      const cmd = pane.current_command.toLowerCase();
      if (cmd.includes("claude") || cmd.includes("cld")) return true;
    }
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
  let statusPollInterval: ReturnType<typeof setInterval> | undefined;
  let unlistenSessionChanged: UnlistenFn | undefined;

  onMount(async () => {
    // Load initial data in parallel. `loadRemoteHosts` feeds
    // `loadTmuxPanes` → `loadRemotePanes`, so it must be one of the
    // first fetches — otherwise the first tmux poll sees
    // `state.remoteHosts = []` and skips Mac pane discovery for one
    // tick, flashing the grid as local-only.
    await Promise.all([
      loadProjectsWithMeta(),
      loadTmuxSessions(),
      loadPaneAssignments(),
      loadPanePresets(),
      loadSessionOrder(),
      loadPinnedOrder(),
      loadRemoteHosts(),
    ]);

    // F-61: Backfill inodes + scan for orphaned projects (runs in background)
    reconcileProjectInodes();

    // Main tmux poll — 10s fallback for tmux state + project list. Live
    // updates on the desktop come primarily from tmux-poller's WebSocket
    // broadcast; this interval catches drift (e.g. a pane assigned from
    // the phone) that the watcher misses. `pollPaneStatuses` was removed
    // from this cycle — the 1.5s interval below already fires it, and
    // piggybacking it on the 10s tick just doubled the wsl.exe load
    // without changing observed latency.
    const runPollCycle = () => {
      refreshTmuxState();
      loadProjectsWithMeta();
    };
    runPollCycle();
    // Still call pollPaneStatuses once on mount so the initial chips
    // reflect real state before the first 1.5s tick fires.
    pollPaneStatuses();
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
    completePanePick,
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
