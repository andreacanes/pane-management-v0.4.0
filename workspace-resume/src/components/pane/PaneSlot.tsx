import { Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { createDroppable } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import {
  setPaneAssignment,
  setPaneAssignmentMeta,
  syncProjectToMac,
  checkRemotePathExists,
  checkSshMaster,
  killPaneOn,
  openDirectory,
} from "../../lib/tauri-commands";
import {
  launchToPane,
  newSessionInPane,
  forkPaneSession,
} from "../../lib/launch";
import { deriveName, fromWslPath } from "../../lib/path";
import type { TmuxPane, ProjectWithMeta } from "../../lib/types";
import { StatusChip } from "../ui/StatusChip";
import { Button } from "../ui/Button";
import { AccountBadge } from "../ui/AccountBadge";
import {
  GitBranch,
  MoreHorizontal,
  Settings,
  FolderOpen,
  Plus,
  Trash2,
  Terminal,
  Link,
} from "../ui/icons";

// Drop target prefix — kept for App.tsx backward compat
export const PANE_SLOT_PREFIX = "pane-slot:";

export function PaneSlot(props: { pane: TmuxPane; assignment?: string | null }) {
  const {
    state,
    refreshTmuxState,
    openProjectSettings,
    pendingLaunch,
  } = useApp();

  const paneIndex = () => props.pane.pane_index;
  // Host is a property of the pane itself in the post-refactor model —
  // the grid can show panes from local and Mac tmux side-by-side, so
  // the slot has no "configure host" control (see plan, Q1 option A).
  const paneHost = () => props.pane.host || "local";
  const paneSession = () => props.pane.session_name || state.selectedTmuxSession || "";
  const paneWindow = () => props.pane.window_index ?? state.selectedTmuxWindow ?? 0;
  /** The full 4-segment store key `host|session|window|pane` used to
   *  look this slot up in `state.paneAssignments[Full]`. Same shape the
   *  Rust side writes. */
  const assignmentKey = () => `${paneHost()}|${paneSession()}|${paneWindow()}|${paneIndex()}`;

  const [launching, setLaunching] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [showKillModal, setShowKillModal] = createSignal(false);

  // Drop target — key by the full coord so Local main:0.3 and Mac main:0.3
  // register as distinct drop targets in solid-dnd's registry.
  const droppable = createDroppable(assignmentKey());

  function onDocClick() { setMenuOpen(false); }
  onMount(() => document.addEventListener("click", onDocClick));
  onCleanup(() => document.removeEventListener("click", onDocClick));

  // Derived state
  const assignedProject = (): ProjectWithMeta | undefined => {
    const encodedName = props.assignment || state.paneAssignments[assignmentKey()];
    if (!encodedName) return undefined;
    return state.projects.find((p) => p.encoded_name === encodedName);
  };

  const detectedProject = (): ProjectWithMeta | undefined => {
    const panePath = props.pane.current_path?.toLowerCase().replace(/[\\/]+$/, "");
    if (!panePath) return undefined;
    return state.projects.find((p) => {
      const actual = p.actual_path.toLowerCase().replace(/[\\/]+$/, "");
      const wsl = fromWslPath(p.actual_path).toLowerCase().replace(/[\\/]+$/, "");
      return actual === panePath || wsl === panePath;
    });
  };

  /**
   * If the live cwd matches a known project different from the stored
   * assignment, the user cd'd away — prefer what the pane is actually
   * running in, not the stale label. Fall back to the assignment only
   * when the cwd doesn't match any project (e.g. pane is at $HOME).
   */
  const effectiveProject = () => {
    const assigned = assignedProject();
    const detected = detectedProject();
    if (detected && assigned && detected.encoded_name !== assigned.encoded_name) {
      return detected;
    }
    return assigned || detected;
  };
  // Andrea's Claude binary appears as `cli-ncld-114.bin`, which does NOT
  // contain the literal "claude". Widen the match so any ncld wrapper
  // also registers — otherwise the UI mis-reports running-Claude panes
  // as idle, and hides Fork / shows Resume+New in the wrong state.
  const isRunningClaude = () => {
    const cmd = props.pane.current_command?.toLowerCase() ?? "";
    return cmd.includes("claude") || cmd.includes("ncld");
  };
  const projectName = () => {
    const p = effectiveProject();
    if (p) return p.meta.display_name || deriveName(p.actual_path);
    return deriveName(props.pane.current_path || "");
  };
  const isWaitingApproval = () => {
    const winIdx = String(props.pane.window_index);
    const ws = state.windowStatuses[winIdx];
    return ws?.waiting_panes?.includes(paneIndex()) ?? false;
  };
  const isPaneSelectMode = () => pendingLaunch() != null;
  const hasProject = () => effectiveProject() != null;
  const isOccupied = () => isRunningClaude() || hasProject();

  const statusKind = () => {
    if (isWaitingApproval()) return "waiting";
    if (isRunningClaude()) return "running";
    if (hasProject()) return "idle";
    return null;
  };

  const showResume = () => hasProject() && !isRunningClaude();
  /**
   * Fork is available whenever the pane is running Claude and we can
   * identify a project to look up sessions in. Session-id resolution
   * happens inside forkPaneSession (parse start_command → bound_session
   * → listSessions MRU), so we don't need to pre-resolve here — the
   * async fallback chain is more robust than what we can compute sync.
   */
  const showFork = () => isRunningClaude() && effectiveProject() != null;
  const gitBranch = () => props.pane.git_branch || null;
  const isWorktree = () => props.pane.is_worktree === true;

  // Host is now immutable per slot (it's the pane's tmux server) — we
  // just surface it to the badge, no dropdown. Account remains editable.
  const currentHost = () => paneHost();
  const currentAccount = () => {
    const full = state.paneAssignmentsFull[assignmentKey()];
    return full?.account ?? "andrea";
  };
  const [syncing, setSyncing] = createSignal(false);

  // Pre-flight state for the Mac path. `null` = not checked / not relevant,
  // `true` = path exists on the Mac, `false` = project is not mirrored yet
  // and a Host=Mac launch would fail silently inside the pane. Recomputed
  // when the host changes or the effective project changes.
  const [macPathOk, setMacPathOk] = createSignal<boolean | null>(null);

  // SSH ControlMaster health for the Mac link. `null` = unchecked (we only
  // poll while Host=Mac to avoid per-pane overhead on local panes),
  // `true` = multiplexed socket is live, `false` = dead — next remote call
  // will pay the ~500 ms handshake cost.
  const [sshLive, setSshLive] = createSignal<boolean | null>(null);

  // Derive the expected Mac path for this project under the Mutagen
  // convention (`~/projects/<basename>`). Returns null if there's no
  // project assigned, which suppresses the pre-flight check.
  const expectedMacPath = () => {
    const p = effectiveProject();
    if (!p) return null;
    const parts = p.actual_path.split(/[\\/]+/).filter((s) => s.length > 0);
    const basename = parts[parts.length - 1] ?? "";
    return basename ? `/Users/admin/projects/${basename}` : null;
  };

  createEffect(() => {
    const host = paneHost();
    const path = expectedMacPath();
    if (host === "local" || !path) {
      setMacPathOk(null);
      return;
    }
    // Fire-and-forget: the check takes ~50 ms over a warm ControlMaster
    // socket, ~500 ms cold. We don't block any UI action on it.
    checkRemotePathExists(host, path)
      .then(setMacPathOk)
      .catch((e) => {
        console.error("[PaneSlot] checkRemotePathExists failed:", e);
        setMacPathOk(null);
      });
  });

  onMount(() => {
    let disposed = false;
    const poll = async () => {
      if (disposed) return;
      // Only interested in SSH health on remote panes. Skipping local
      // saves an ssh call per pane on purely-local grids.
      const host = paneHost();
      if (host === "local") {
        setSshLive(null);
        return;
      }
      try {
        const live = await checkSshMaster(host);
        if (!disposed) setSshLive(live);
      } catch (e) {
        console.error("[PaneSlot] checkSshMaster failed:", e);
        if (!disposed) setSshLive(null);
      }
    };
    poll();
    const timer = setInterval(poll, 15000);
    onCleanup(() => {
      disposed = true;
      clearInterval(timer);
    });
  });

  async function handleResume() {
    const p = effectiveProject();
    if (!p || launching()) return;
    setLaunching(true);
    try {
      await launchToPane({
        tmuxSession: paneSession(),
        tmuxWindow: paneWindow(),
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject: p.encoded_name,
        projectPath: p.actual_path,
        sessionId: null,
        boundSession: p.meta?.bound_session ?? null,
        targetPaneIndex: paneIndex(),
        host: paneHost(),
        account: currentAccount(),
      });
    } catch (e) { console.error("[PaneSlot] resume error:", e); }
    finally { setLaunching(false); refreshTmuxState(); }
  }

  async function handleNewSession() {
    const p = effectiveProject();
    if (!p || launching()) return;
    setLaunching(true);
    try {
      await newSessionInPane({
        tmuxSession: paneSession(),
        tmuxWindow: paneWindow(),
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject: p.encoded_name,
        projectPath: p.actual_path,
        targetPaneIndex: paneIndex(),
        host: paneHost(),
        account: currentAccount(),
      });
    } catch (e) { console.error("[PaneSlot] newSession error:", e); }
    finally { setLaunching(false); refreshTmuxState(); }
  }

  async function handleAccountChange(newAccount: string) {
    try {
      await setPaneAssignmentMeta(
        paneHost(),
        paneSession(),
        paneWindow(),
        paneIndex(),
        newAccount,
      );
      refreshTmuxState();
    } catch (e) {
      console.error("[PaneSlot] handleAccountChange error:", e);
    }
  }

  async function handleSyncToMac() {
    const p = effectiveProject();
    if (!p || syncing()) return;
    setSyncing(true);
    try {
      const out = await syncProjectToMac(p.encoded_name);
      console.log("[PaneSlot] sync-to-mac:", out);
    } catch (e) {
      console.error("[PaneSlot] syncToMac error:", e);
    } finally {
      setSyncing(false);
    }
  }

  async function handleFork() {
    const p = effectiveProject();
    if (launching() || !p) return;
    // Prefer an explicit id parsed from start_command when present; otherwise
    // forkPaneSession will fall back to bound_session → listSessions MRU.
    const startCmd = props.pane.start_command || "";
    const match = startCmd.match(/(?:^|\s)(?:--resume|-r)\s+([0-9a-f-]{36})(?:\s|$)/i);
    const explicitSid = match ? match[1] : null;
    setLaunching(true);
    try {
      await forkPaneSession({
        tmuxSession: paneSession(),
        tmuxWindow: paneWindow(),
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject: p.encoded_name,
        projectPath: p.actual_path,
        sourcePaneIndex: paneIndex(),
        sessionId: explicitSid,
        boundSession: p.meta?.bound_session,
        host: paneHost(),
        sourceSession: paneSession(),
        sourceWindow: paneWindow(),
        account: currentAccount(),
      });
    } catch (e) {
      console.error("[PaneSlot] fork error:", e);
    } finally {
      setLaunching(false);
      refreshTmuxState();
    }
  }

  async function handleClear() {
    try {
      await setPaneAssignment(paneHost(), paneSession(), paneWindow(), paneIndex(), null);
      refreshTmuxState();
    } catch (e) {
      console.error("[PaneSlot] clear error:", e);
    }
  }

  async function handleKill() {
    try {
      await killPaneOn(paneHost(), paneSession(), paneWindow(), paneIndex());
      refreshTmuxState();
    } catch (e) {
      console.error("[PaneSlot] killPane error:", e);
    }
  }

  async function handleOpenDir() {
    const path = props.pane.current_path;
    if (path) {
      try { await openDirectory(path); }
      catch (e) { console.error("[PaneSlot] openDir error:", e); }
    }
  }

  function handlePaneSelect() {
    const launch = pendingLaunch();
    if (launch) launch.execute(paneIndex());
  }

  return (
    <div
      ref={(el) => droppable(el)}
      class={`pane-slot ${isOccupied() ? "assigned" : ""} ${isWaitingApproval() ? "waiting-approval" : ""} ${isPaneSelectMode() ? "pane-selectable" : ""} ${droppable.isActiveDroppable ? "drop-active" : ""}`}
      onClick={() => isPaneSelectMode() && handlePaneSelect()}
      title={`${props.pane.current_path || ""}\n${props.pane.current_command || ""}`}
    >
      {/* Persistent kill button (top-right, always visible) */}
      <button
        class="pane-slot-kill-btn"
        onClick={(e) => { e.stopPropagation(); setShowKillModal(true); }}
        title={`Kill pane ${props.pane.pane_id} (${paneHost()} tmux ${paneSession()}:${paneWindow()}.${paneIndex()})`}
      >
        <Trash2 size={12} />
      </button>
      {/* Occupied pane */}
      <Show when={isOccupied()}>
        <div class="pane-slot-body">
          <div class="pane-slot-primary">
            <span class="pane-slot-title">{projectName()}</span>
            <span
              class="pane-slot-host-badge"
              title={(() => {
                const base = paneHost() === "local"
                  ? `Pane runs on this WSL tmux (${paneSession()}:${paneWindow()}.${paneIndex()})`
                  : `Pane runs on ${paneHost()} (${paneSession()}:${paneWindow()}.${paneIndex()})`;
                if (paneHost() === "local") return base;
                const live = sshLive();
                if (live === true) return `${base} — SSH link live`;
                if (live === false) return `${base} — SSH master dead (next call will be slow)`;
                return base;
              })()}
              style={{
                "background": paneHost() === "local"
                  ? "rgba(148, 163, 184, 0.16)"
                  : "rgba(20, 184, 166, 0.18)",
                "color": paneHost() === "local" ? "#94a3b8" : "#14b8a6",
                "padding": "1px 6px",
                "border-radius": "999px",
                "font-size": "10px",
                "font-weight": "600",
                "text-transform": "uppercase",
                "letter-spacing": "0.04em",
                "display": "inline-flex",
                "align-items": "center",
                "gap": "4px",
              }}
            >
              <Show when={paneHost() !== "local" && sshLive() !== null}>
                <span
                  aria-hidden
                  style={{
                    "display": "inline-block",
                    "width": "6px",
                    "height": "6px",
                    "border-radius": "50%",
                    "background": sshLive() ? "#22c55e" : "#f59e0b",
                    "box-shadow": sshLive()
                      ? "0 0 4px rgba(34,197,94,0.6)"
                      : "0 0 4px rgba(245,158,11,0.6)",
                  }}
                />
              </Show>
              {paneHost() === "local" ? "WSL" : paneHost()}
            </span>
            <AccountBadge compact pane={props.pane} />
            <Show when={statusKind()}>
              <StatusChip status={statusKind()!} compact />
            </Show>
          </div>

          <div class="pane-slot-secondary">
            <Show when={gitBranch()}>
              <span class="pane-slot-branch">
                <GitBranch size={11} />
                <span>{gitBranch()}</span>
                <Show when={isWorktree()}>
                  <Link size={10} />
                </Show>
              </span>
            </Show>
          </div>

          <Show when={hasProject()}>
            <div
              class="pane-slot-host-account"
              style={{
                "display": "flex",
                "gap": "6px",
                "margin-top": "4px",
                "font-size": "11px",
              }}
              onClick={(e) => e.stopPropagation()}
            >
              {/* Host is a property of the pane, not a knob — see plan's
                  Q1 option A. The badge at the top of the slot shows
                  which host this is; here we only control the Claude
                  account identity the next launch runs under. */}
              <select
                value={currentAccount()}
                onChange={(e) => handleAccountChange(e.currentTarget.value)}
                title="Claude account for the next launch in this pane"
                style={{
                  "background": "var(--surface-2, #1f1f23)",
                  "color": "var(--text, #d4d4d4)",
                  "border": "1px solid var(--border, #2d2d33)",
                  "border-radius": "4px",
                  "padding": "2px 4px",
                }}
              >
                <option value="andrea">Andrea</option>
                <option value="bravura">Bravura</option>
                <option value="sully">Sully</option>
              </select>
            </div>
            <Show when={paneHost() !== "local" && macPathOk() === false}>
              <div
                class="pane-slot-mac-warn"
                onClick={(e) => e.stopPropagation()}
                style={{
                  "display": "flex",
                  "align-items": "center",
                  "gap": "6px",
                  "margin-top": "4px",
                  "padding": "4px 6px",
                  "background": "rgba(239, 68, 68, 0.12)",
                  "border": "1px solid rgba(239, 68, 68, 0.35)",
                  "border-radius": "4px",
                  "font-size": "10px",
                  "color": "#fca5a5",
                }}
                title={`Project not mirrored to /Users/admin/projects/${expectedMacPath()?.split("/").pop() ?? ""} — launching Host=Mac will fail until you sync.`}
              >
                <span>Not synced to Mac</span>
                <button
                  onClick={() => handleSyncToMac()}
                  disabled={syncing()}
                  style={{
                    "margin-left": "auto",
                    "background": "rgba(239, 68, 68, 0.22)",
                    "border": "1px solid rgba(239, 68, 68, 0.5)",
                    "color": "#fecaca",
                    "border-radius": "3px",
                    "padding": "1px 6px",
                    "font-size": "10px",
                    "cursor": syncing() ? "default" : "pointer",
                  }}
                >
                  {syncing() ? "Syncing…" : "Sync now"}
                </button>
              </div>
            </Show>
          </Show>

          <div class="pane-slot-actions">
            <Show when={showResume()}>
              <Button variant="primary" size="sm" onClick={handleResume} disabled={launching()}>
                Resume
              </Button>
              <Button variant="secondary" size="sm" onClick={handleNewSession} disabled={launching()}>
                New
              </Button>
            </Show>
            <Show when={showFork()}>
              <Button
                variant="secondary"
                size="sm"
                onClick={handleFork}
                disabled={launching()}
                title="Fork this conversation — this pane continues as a new branch, a sibling pane opens the original frozen at the fork point"
              >
                <GitBranch size={12} /> Fork
              </Button>
            </Show>

            <div class="pane-slot-menu-root" style={{ "margin-left": "auto" }}>
              <button
                class="pane-slot-overflow"
                onClick={(e) => { e.stopPropagation(); setMenuOpen((v) => !v); }}
                title="More actions"
              >
                <MoreHorizontal size={14} />
              </button>
              <Show when={menuOpen()}>
                <div class="pane-slot-menu" onClick={(e) => e.stopPropagation()}>
                  <Show when={effectiveProject()}>
                    <button class="pane-slot-menu-item" onClick={() => { openProjectSettings(effectiveProject()!.encoded_name); setMenuOpen(false); }}>
                      <Settings size={12} /> Settings
                    </button>
                    <Show when={!isRunningClaude()}>
                      <button class="pane-slot-menu-item" onClick={() => { handleNewSession(); setMenuOpen(false); }}>
                        <Plus size={12} /> New session
                      </button>
                    </Show>
                  </Show>
                  <button class="pane-slot-menu-item" onClick={() => { handleOpenDir(); setMenuOpen(false); }}>
                    <FolderOpen size={12} /> Open directory
                  </button>
                  <Show when={hasProject() && paneHost() !== "local"}>
                    <button
                      class="pane-slot-menu-item"
                      disabled={syncing()}
                      onClick={() => { handleSyncToMac(); setMenuOpen(false); }}
                    >
                      <FolderOpen size={12} /> {syncing() ? "Syncing…" : "Sync to Mac"}
                    </button>
                  </Show>
                  <Show when={hasProject()}>
                    <button class="pane-slot-menu-item" onClick={() => { handleClear(); setMenuOpen(false); }}>
                      Clear assignment
                    </button>
                  </Show>
                  <button class="pane-slot-menu-item danger" onClick={() => { handleKill(); setMenuOpen(false); }}>
                    <Trash2 size={12} /> Kill pane
                  </button>
                </div>
              </Show>
            </div>
          </div>
        </div>
      </Show>

      {/* Empty pane */}
      <Show when={!isOccupied()}>
        <div class="pane-slot-empty">
          <div class="pane-slot-empty-inner">
            <Terminal size={18} />
            <Show when={isPaneSelectMode()} fallback="Empty pane">
              Click to assign here
            </Show>
          </div>
        </div>
      </Show>

      {/* Kill pane confirmation */}
      <Show when={showKillModal()}>
        <div class="modal-backdrop" onClick={(e) => { e.stopPropagation(); setShowKillModal(false); }}>
          <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
            <p class="confirm-message">
              <strong>Kill pane {props.pane.pane_id}?</strong>
            </p>
            <p class="confirm-warning">
              tmux target: <code>{state.selectedTmuxSession}:{state.selectedTmuxWindow}.{paneIndex()}</code><br />
              Running: <code>{props.pane.current_command || "(empty)"}</code><br />
              In: <code>{props.pane.current_path || "(no cwd)"}</code>
            </p>
            <p class="confirm-warning">
              Any process in this pane will be terminated. Other panes are unaffected.
            </p>
            <div class="confirm-actions">
              <button class="modal-btn" onClick={() => setShowKillModal(false)}>
                Cancel
              </button>
              <button
                class="modal-btn danger"
                onClick={() => { setShowKillModal(false); handleKill(); }}
              >
                Kill pane
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
