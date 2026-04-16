import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { createDroppable } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import {
  setPaneAssignment,
  killPane,
  openDirectory,
} from "../../lib/tauri-commands";
import { launchToPane, newSessionInPane } from "../../lib/launch";
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

  const [launching, setLaunching] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [showKillModal, setShowKillModal] = createSignal(false);

  // Drop target only — no longer a drag source
  const droppable = createDroppable(paneIndex().toString());

  function onDocClick() { setMenuOpen(false); }
  onMount(() => document.addEventListener("click", onDocClick));
  onCleanup(() => document.removeEventListener("click", onDocClick));

  // Derived state
  const assignedProject = (): ProjectWithMeta | undefined => {
    const encodedName = props.assignment || state.paneAssignments[String(paneIndex())];
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
  const isRunningClaude = () => props.pane.current_command?.toLowerCase().includes("claude");
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
  const gitBranch = () => props.pane.git_branch || null;
  const isWorktree = () => props.pane.is_worktree === true;

  async function handleResume() {
    const p = effectiveProject();
    if (!p || launching()) return;
    setLaunching(true);
    try { await launchToPane(p, state, paneIndex()); }
    catch (e) { console.error("[PaneSlot] resume error:", e); }
    finally { setLaunching(false); refreshTmuxState(); }
  }

  async function handleNewSession() {
    const p = effectiveProject();
    if (!p || launching()) return;
    setLaunching(true);
    try { await newSessionInPane(p, state, paneIndex()); }
    catch (e) { console.error("[PaneSlot] newSession error:", e); }
    finally { setLaunching(false); refreshTmuxState(); }
  }

  async function handleClear() {
    const sess = state.selectedTmuxSession;
    const winIdx = state.selectedTmuxWindow;
    if (!sess || winIdx == null) return;
    try {
      await setPaneAssignment(sess, winIdx, paneIndex(), null);
      refreshTmuxState();
    } catch (e) {
      console.error("[PaneSlot] clear error:", e);
    }
  }

  async function handleKill() {
    const sess = state.selectedTmuxSession;
    const winIdx = state.selectedTmuxWindow;
    if (!sess || winIdx == null) return;
    try {
      await killPane(sess, winIdx, props.pane.pane_index);
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
        title={`Kill pane ${props.pane.pane_id} (tmux ${state.selectedTmuxSession}:${state.selectedTmuxWindow}.${paneIndex()})`}
      >
        <Trash2 size={12} />
      </button>
      {/* Occupied pane */}
      <Show when={isOccupied()}>
        <div class="pane-slot-body">
          <div class="pane-slot-primary">
            <span class="pane-slot-title">{projectName()}</span>
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

          <div class="pane-slot-actions">
            <Show when={showResume()}>
              <Button variant="primary" size="sm" onClick={handleResume} disabled={launching()}>
                Resume
              </Button>
              <Button variant="secondary" size="sm" onClick={handleNewSession} disabled={launching()}>
                New
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
