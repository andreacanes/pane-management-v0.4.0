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
import { subscribeCompanionEvents } from "../lib/companion-events";
import type { CompanionEvent } from "../lib/types";
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
  checkPaneStatusesOn,
} from "../lib/tauri-commands";
import { toastError, toastSuccess } from "../components/ui/Toast";
import { launchToPane, newSessionInPane } from "../lib/launch";
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
  /** Remote panes that are RUNNING on a remote host but have no local
   *  ssh-mirror window in WSL tmux. Kept separate from `tmuxPanes` so
   *  they don't pollute every local window's grid — rendered as an
   *  "Unmirrored Mac sessions" caret at the top of PaneGrid with a
   *  one-click Attach action. A remote pane migrates into `tmuxPanes`
   *  the moment a local window gets named `<host>/<session>`
   *  (attachRemoteSession does exactly that). */
  unmirroredRemotePanes: TmuxPane[];
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
  /** Re-read the persistent `remote_hosts` list and kick off a fresh
   *  remote poll. Settings calls this after saving edits so the grid
   *  reflects the new host set immediately. */
  refreshRemoteHosts: () => Promise<void>;
  // Notification muting — keyed on the full host+session+window+pane so
  // a Mac pane and a local pane with colliding coords don't share
  // state. Callers typically pass `pane.host || "local"`.
  mutePane: (host: string, sessionName: string, windowIndex: number, paneIndex: number) => void;
  unmutePane: (host: string, sessionName: string, windowIndex: number, paneIndex: number) => void;
  isPaneMuted: (host: string, sessionName: string, windowIndex: number, paneIndex: number) => boolean;
  // Derived getters
  projectsByTier: (tier: ProjectTier) => ProjectWithMeta[];
  getProjectMeta: (encodedName: string) => ProjectMeta;
  isProjectActive: (encodedName: string) => boolean;
  isProjectActiveInSession: (encodedName: string) => boolean;
  isProjectWaitingInSession: (encodedName: string) => boolean;
  /** Find the first window where a project has panes. After the
   *  multi-host refactor this can return a remote-host window too, so
   *  the shape is an object with host + session + windowIndex instead
   *  of a bare window number. Consumers that only want to focus local
   *  windows should gate on `host === "local"`. */
  findProjectWindow: (encodedName: string) => { host: string; session: string; windowIndex: number } | null;
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
    unmirroredRemotePanes: [],
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

  /** Split remote panes into "mirror-matched" (belongs in the grid of
   *  the selected local window) and "unmirrored" (rendered in the
   *  PaneGrid caret). A remote pane is considered mirrored when some
   *  local tmux window's name equals `<host>/<session>` — that's the
   *  convention `attachRemoteSession` uses when it creates a local
   *  window ssh-attached to the remote tmux. Matching by window name
   *  is resilient to renames mid-flight (tmux reports the new name on
   *  the next poll) and doesn't require us to parse `start_command`.
   *
   *  Returns `[inCurrentWindow, unmirrored]`. Panes whose mirror is in
   *  a DIFFERENT local window (e.g. user has main:9 = mac/A mirror and
   *  main:12 = mac/B mirror, currently viewing main:9) are excluded
   *  from both — they'll appear when the user switches to that window.
   *  This is the whole point of Fix 1: Mac panes get a "location"
   *  instead of polluting every local window's grid. */
  /** Drop local SSH-mirror panes whose target Mac pane is already in
   *  `matched`. Both panes represent the same conceptual remote
   *  session — render just the richer Mac pane card (project, account,
   *  Claude state) instead of two cards for the same thing. Falls back
   *  to keeping the mirror if the Mac pane isn't reachable, so the
   *  slot doesn't disappear when the Mac is slow / off / SSH down.
   *
   *  Reads `mirror_target` from the wire DTO — the backend's
   *  `services::ssh_mirror` parses `start_command` once at poll time,
   *  so this function doesn't carry its own parser. */
  function dropDuplicateMirrors(local: TmuxPane[], matched: TmuxPane[]): TmuxPane[] {
    const targets = new Set(
      matched.map((p) => `${p.host || ""}/${p.session_name || ""}`),
    );
    return local.filter((p) => {
      const m = p.mirror_target;
      if (!m) return true;
      return !targets.has(`${m.alias}/${m.session}`);
    });
  }

  function partitionRemotePanes(
    remote: TmuxPane[],
    localWindows: TmuxWindow[],
    currentWindowIndex: number | null,
  ): { matched: TmuxPane[]; unmirrored: TmuxPane[] } {
    const currentName = localWindows.find((w) => w.index === currentWindowIndex)?.name ?? "";
    const mirroredNames = new Set(localWindows.map((w) => w.name));
    const matched: TmuxPane[] = [];
    const unmirrored: TmuxPane[] = [];
    for (const p of remote) {
      const mirrorKey = `${p.host || ""}/${p.session_name || ""}`;
      if (mirrorKey === currentName) {
        matched.push(p);
      } else if (!mirroredNames.has(mirrorKey)) {
        unmirrored.push(p);
      }
      // else: mirrored in another local window — it'll show when the
      // user switches there. Deliberately not shown anywhere right now.
    }
    return { matched, unmirrored };
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
        const { matched, unmirrored } = partitionRemotePanes(
          remote,
          freshState.windows,
          activeWin.index,
        );
        batch(() => {
          setState("tmuxPanes", [
            ...dropDuplicateMirrors(freshState.panes, matched),
            ...matched,
          ]);
          setState("unmirroredRemotePanes", unmirrored);
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

      // Then fetch remote panes, partition by mirror match, and merge.
      // Keeps the grid responsive even when a Mac is slow to answer —
      // the Mac section appears ~50-500 ms later without blocking the
      // local render path.
      const remote = await loadRemotePanes();
      const { matched, unmirrored } = partitionRemotePanes(
        remote,
        tmuxState.windows,
        windowIndex,
      );
      batch(() => {
        if (matched.length > 0) {
          setState("tmuxPanes", [
            ...dropDuplicateMirrors(tmuxState.panes, matched),
            ...matched,
          ]);
        }
        setState("unmirroredRemotePanes", unmirrored);
      });
    } catch (e) {
      console.error("[AppContext] loadTmuxPanes error:", e);
      batch(() => {
        setState("tmuxPanes", []);
        setState("unmirroredRemotePanes", []);
      });
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
          toastSuccess(`${host} reachable again`, "Remote panes restored");
        }
        remoteHealth.set(host, { ok: true });
      } catch (e) {
        const prev = remoteHealth.get(host);
        if (!prev || prev.ok) {
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
   *  to poll. Backed by the persistent `remote_hosts` Tauri store key
   *  (editable via Settings > Remote hosts); defaults to `["mac"]` when
   *  the store is empty. `refreshRemoteHosts` re-reads the store —
   *  Settings calls it after a save so host changes take effect without
   *  an app restart. */
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
  // Muted panes are tracked by key "host|session|window|pane_index".
  // Muted panes are filtered out of waiting_panes before storing in state,
  // so window tabs, pinned pills, and pane slots all naturally see "not waiting".
  // Auto-unmutes when the pane stops waiting (agent resumed).
  //
  // The `host` segment is required because after the multi-host refactor
  // a local `main:0.3` and a Mac `main:0.3` are distinct panes; muting
  // one must not silence the other.

  const mutedPanes = new Set<string>();
  const prevWaiting = new Map<string, boolean>(); // track previous waiting state for auto-unmute

  function muteKey(host: string, session: string, window: number, pane: number): string {
    return `${host}|${session}|${window}|${pane}`;
  }

  function mutePane(host: string, session: string, window: number, pane: number) {
    mutedPanes.add(muteKey(host, session, window, pane));
    // Re-filter current state immediately
    pollPaneStatuses();
  }

  function unmutePane(host: string, session: string, window: number, pane: number) {
    mutedPanes.delete(muteKey(host, session, window, pane));
    pollPaneStatuses();
  }

  function isPaneMuted(host: string, session: string, window: number, pane: number): boolean {
    return mutedPanes.has(muteKey(host, session, window, pane));
  }

  /** Fan out `check_pane_statuses_on` across every (host, session) pair
   *  the UI currently knows about — the local selected session plus
   *  every distinct pair from the pane grid (captures remote Mac panes
   *  at whatever sessions they live in). One slow or failing host
   *  doesn't block the others; each probe has its own catch that
   *  degrades to an empty status map for that scope.
   *
   *  Results are merged into `state.windowStatuses` under a composite
   *  key `"host|session|winIdx"` so same-coord collisions across hosts
   *  can't overwrite each other. Consumers either look up by the full
   *  key (PaneSlot, TopBar) or iterate `Object.values()` when they
   *  only care about "any pane waiting anywhere" (StatusOrb,
   *  QuickLaunch status counts, project-level helpers). */
  async function pollPaneStatuses() {
    if (pollingPaused) return;

    const pairs = new Map<string, { host: string; session: string }>();
    if (state.selectedTmuxSession) {
      pairs.set(`local|${state.selectedTmuxSession}`, {
        host: "local",
        session: state.selectedTmuxSession,
      });
    }
    for (const p of state.tmuxPanes) {
      const host = p.host || "local";
      const session = p.session_name;
      if (!session) continue;
      const k = `${host}|${session}`;
      if (!pairs.has(k)) pairs.set(k, { host, session });
    }
    if (pairs.size === 0) return;

    const results = await Promise.all(
      [...pairs.values()].map(async ({ host, session }) => {
        try {
          const statuses = await checkPaneStatusesOn(host, session);
          return { host, session, statuses };
        } catch {
          return { host, session, statuses: {} as Record<string, WindowPaneStatus> };
        }
      }),
    );

    const merged: Record<string, WindowPaneStatus> = {};
    for (const { host, session, statuses } of results) {
      for (const [winIdx, status] of Object.entries(statuses)) {
        merged[`${host}|${session}|${winIdx}`] = status;
      }
    }

    // Auto-unmute panes that are no longer waiting — iterate on a
    // snapshot so deletion mid-loop is safe.
    for (const mk of [...mutedPanes]) {
      const [mHost, mSess, mWin, mPane] = mk.split("|");
      const ws = merged[`${mHost}|${mSess}|${mWin}`];
      const stillWaiting = ws?.waiting_panes?.includes(Number(mPane)) ?? false;
      if (!stillWaiting) mutedPanes.delete(mk);
    }

    // Filter muted panes out of waiting_panes. Defensive copy per
    // entry — earlier versions mutated in place on the assumption no
    // other consumer held a reference, which is true today but brittle
    // if a caller ever caches the result of `checkPaneStatusesOn`.
    for (const [key, status] of Object.entries(merged)) {
      const [mHost, mSess, mWin] = key.split("|");
      merged[key] = {
        ...status,
        waiting_panes: status.waiting_panes.filter(
          (paneIdx) => !mutedPanes.has(muteKey(mHost, mSess, Number(mWin), paneIdx)),
        ),
      };
    }

    setState("windowStatuses", reconcile(merged));
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

  /** Find the first window where a project has a pane. Prefers a
   *  waiting pane over a merely-active one so the UI focuses where
   *  user attention is actually needed. Returns an object with
   *  `host` + `session` + `windowIndex` — callers that only want to
   *  navigate to local windows should gate on `host === "local"`. */
  function findProjectWindow(encodedName: string) {
    const proj = state.projects.find((p) => p.encoded_name === encodedName);
    if (!proj) return null;
    let firstActive: { host: string; session: string; windowIndex: number } | null = null;
    for (const [key, status] of Object.entries(state.windowStatuses)) {
      const [host, session, winStr] = key.split("|");
      if (!host || !session || !winStr) continue;
      const windowIndex = Number(winStr);
      const paths = status.active_paths ?? [];
      const panes = status.active_panes ?? [];
      const waiting = status.waiting_panes ?? [];
      for (let i = 0; i < paths.length; i++) {
        if (pathMatchesProject(paths[i], proj.actual_path)) {
          if (waiting.includes(panes[i])) return { host, session, windowIndex };
          if (firstActive == null) firstActive = { host, session, windowIndex };
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
  let unlistenCompanionEvent: UnlistenFn | undefined;

  /** Handler for the companion's Tauri-bridged event stream.
   *  Translates Rust-side pane + window changes into local state
   *  mutations so the UI reacts with ~1 s latency instead of waiting
   *  on the fallback poll cycle. Anything this doesn't recognise
   *  falls through to the 60 s full-refresh interval. */
  function handleCompanionEvent(ev: CompanionEvent) {
    if (pollingPaused) return;
    switch (ev.type) {
      case "window_focus_changed": {
        // Auto-follow: switch the dashboard's selected window to
        // whatever the user just focused in tmux. Host-scoped so a
        // Mac focus change doesn't yank the local-session view.
        if (ev.host !== "local") break;
        if (ev.session_name !== state.selectedTmuxSession) break;
        if (ev.window_index === state.selectedTmuxWindow) break;
        if (Date.now() <= autoFollowSuppressedUntil) break;
        selectTmuxWindow(ev.window_index as number);
        break;
      }
      case "pane_state_changed":
      case "pane_updated":
      case "pane_removed": {
        // Any pane-level change that the poller noticed: refresh the
        // current window's pane list + the waiting/running chips. The
        // underlying calls are cheap (single tmux list-panes per host)
        // and coalesce naturally because multiple events within the
        // same frame collapse to one refresh via the async queue.
        refreshTmuxState();
        pollPaneStatuses();
        break;
      }
      case "session_ended":
      case "session_started": {
        // Session list needs a re-pull; the top-bar session dropdown
        // consumes tmuxSessions.
        loadTmuxSessions();
        break;
      }
      default:
        // hello / snapshot / approval_* / pane_output_changed — no
        // desktop-side action; the fallback poll handles any drift.
        break;
    }
  }

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

    // Main tmux poll — 60 s fallback. Live updates now arrive via the
    // Tauri event bridge (`companion-event`), which forwards the Rust
    // poller's 1 s tick broadcasts directly into this context. The
    // interval is a safety net for (a) events dropped under a lagged
    // broadcast ring, (b) project metadata that the poller doesn't
    // touch, and (c) state that drifts between events (e.g. an
    // assignment edited by the phone). Dropped from 10 s because at
    // that cadence it dominated the wsl.exe load budget even when
    // events were flowing.
    const runPollCycle = () => {
      refreshTmuxState();
      loadProjectsWithMeta();
    };
    runPollCycle();
    pollPaneStatuses();
    tmuxPollInterval = setInterval(runPollCycle, 60_000);

    // Chip-status safety net. With events flowing this is strictly a
    // catch-up for anything pane_state_changed missed; dropped from
    // 1.5 s to 10 s because the event-triggered refresh above already
    // covers the fast-path cases.
    statusPollInterval = setInterval(() => {
      pollPaneStatuses();
    }, 10_000);

    // Listen for session file changes from Tauri file watcher
    unlistenSessionChanged = await listen<string[]>("session-changed", () => {
      loadProjectsWithMeta();
    });

    // Subscribe to the companion's live event stream via the Tauri
    // bridge. Events arrive within ~1 s of the Rust poller detecting
    // them — orders of magnitude faster than the 60 s fallback above.
    unlistenCompanionEvent = await subscribeCompanionEvents(
      handleCompanionEvent,
    );
  });

  onCleanup(() => {
    if (tmuxPollInterval) clearInterval(tmuxPollInterval);
    if (statusPollInterval) clearInterval(statusPollInterval);
    if (unlistenSessionChanged) unlistenSessionChanged();
    if (unlistenCompanionEvent) unlistenCompanionEvent();
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
    refreshRemoteHosts: async () => {
      await loadRemoteHosts();
      refreshTmuxState();
    },
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
