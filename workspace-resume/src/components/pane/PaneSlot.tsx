import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { createDraggable, createDroppable } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import { setPaneAssignment, killPane, cancelPaneCommand, sendToPane, openDirectory } from "../../lib/tauri-commands";
import { launchToPane, newSessionInPane } from "../../lib/launch";
import { deriveName, fromWslPath } from "../../lib/path";
import type { TmuxPane, ProjectWithMeta } from "../../lib/types";
import { StatusChip, type StatusKind } from "../ui/StatusChip";
import { Button } from "../ui/Button";
import { AccountBadge } from "../ui/AccountBadge";
import {
  GitBranch,
  Link,
  MoreHorizontal,
  Settings as SettingsIcon,
  FolderOpen,
  Plus,
  Trash2,
  Bell,
  BellOff,
  Terminal,
} from "../ui/icons";

export const PANE_SLOT_PREFIX = "pane-slot:";

interface PaneSlotProps {
  pane: TmuxPane;
  assignment: string | null;
}

export function PaneSlot(props: PaneSlotProps) {
  const { state, refreshTmuxState, refreshProjects, openProjectSettings, pendingLaunch, cancelPanePick, mutePane, unmutePane, isPaneMuted } = useApp();

  const draggable = createDraggable(PANE_SLOT_PREFIX + props.pane.pane_index);
  const droppable = createDroppable(props.pane.pane_index.toString());

  const assignedProject = (): ProjectWithMeta | null => {
    if (!props.assignment) return null;
    return state.projects.find((p) => p.encoded_name === props.assignment) ?? null;
  };

  const detectedProject = (): ProjectWithMeta | null => {
    if (props.assignment) return null;
    const panePath = props.pane.current_path;
    if (!panePath) return null;
    const paneAsWsl = panePath.toLowerCase().replace(/\/+$/, "");
    const paneAsWin = fromWslPath(panePath).toLowerCase().replace(/[\\/]+$/, "");
    return (
      state.projects.find((p) => {
        const actual = p.actual_path.toLowerCase().replace(/[\\/]+$/, "");
        return actual === paneAsWsl || actual === paneAsWin;
      }) ?? null
    );
  };

  const effectiveProject = () => assignedProject() ?? detectedProject();

  const isRunningClaude = () =>
    props.pane.current_command.toLowerCase().includes("claude");

  const projectName = () => {
    const proj = effectiveProject();
    if (!proj) return "";
    return proj.meta.display_name || deriveName(proj.actual_path);
  };

  const isDetected = () => !props.assignment && detectedProject() != null;

  const isWaitingApproval = () => {
    const winIdx = String(state.selectedTmuxWindow ?? "");
    const status = state.windowStatuses[winIdx];
    return status?.waiting_panes?.includes(props.pane.pane_index) ?? false;
  };

  const [launching, setLaunching] = createSignal(false);
  const [confirmReplace, setConfirmReplace] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);

  const isMuted = () => {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    if (!sess || win == null) return false;
    return isPaneMuted(sess, win, props.pane.pane_index);
  };

  function toggleMute() {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    if (!sess || win == null) return;
    if (isMuted()) unmutePane(sess, win, props.pane.pane_index);
    else mutePane(sess, win, props.pane.pane_index);
  }

  const isPaneSelectMode = () => pendingLaunch() != null;
  const hasProject = () => effectiveProject() != null;
  const isOccupied = () => isRunningClaude() || hasProject();

  const statusKind = (): StatusKind | null => {
    if (isWaitingApproval()) return "waiting";
    if (isRunningClaude()) return "running";
    if (hasProject()) return "idle";
    return null;
  };

  const statusTooltip = () => {
    const parts: string[] = [];
    if (props.pane.current_path) parts.push(`Path: ${props.pane.current_path}`);
    if (props.pane.current_command && props.pane.current_command !== "-") {
      parts.push(`Command: ${props.pane.current_command}`);
    }
    parts.push(`Size: ${props.pane.width}×${props.pane.height}`);
    return parts.join("\n");
  };

  async function executePendingLaunch() {
    const pl = pendingLaunch();
    if (!pl) return;
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    if (!sess || win == null) return;

    cancelPanePick();
    setLaunching(true);
    try {
      if (pl.mode === "resume") {
        await launchToPane({
          tmuxSession: sess,
          tmuxWindow: win,
          tmuxPanes: state.tmuxPanes,
          paneAssignments: state.paneAssignments,
          encodedProject: pl.project.encoded_name,
          projectPath: pl.project.actual_path,
          boundSession: pl.project.meta.bound_session,
          sessionId: pl.sessionId,
          targetPaneIndex: props.pane.pane_index,
          yolo: pl.yolo,
        });
      } else {
        await newSessionInPane({
          tmuxSession: sess,
          tmuxWindow: win,
          tmuxPanes: state.tmuxPanes,
          paneAssignments: state.paneAssignments,
          encodedProject: pl.project.encoded_name,
          projectPath: pl.project.actual_path,
          targetPaneIndex: props.pane.pane_index,
          yolo: pl.yolo,
        });
      }
      if (pl.continuity) {
        await new Promise((r) => setTimeout(r, 7000));
        const { sendToPane: send } = await import("../../lib/tauri-commands");
        await send(sess, win, props.pane.pane_index, "/continuity");
      }
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] pending launch error:", e);
    } finally {
      setLaunching(false);
    }
  }

  function handlePaneSelectClick(e: MouseEvent) {
    if (!isPaneSelectMode()) return;
    e.stopPropagation();
    e.preventDefault();
    if (isOccupied()) setConfirmReplace(true);
    else executePendingLaunch();
  }

  async function handleResume() {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    const proj = effectiveProject();
    if (sess == null || win == null || !proj) return;
    setLaunching(true);
    try {
      await launchToPane({
        tmuxSession: sess,
        tmuxWindow: win,
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject: proj.encoded_name,
        projectPath: proj.actual_path,
        boundSession: proj.meta.bound_session,
        targetPaneIndex: props.pane.pane_index,
      });
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] resume error:", e);
    } finally {
      setLaunching(false);
    }
  }

  async function handleNewSession() {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    const proj = effectiveProject();
    if (sess == null || win == null || !proj) return;
    setLaunching(true);
    try {
      await newSessionInPane({
        tmuxSession: sess,
        tmuxWindow: win,
        tmuxPanes: state.tmuxPanes,
        paneAssignments: state.paneAssignments,
        encodedProject: proj.encoded_name,
        projectPath: proj.actual_path,
        targetPaneIndex: props.pane.pane_index,
      });
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] new session error:", e);
    } finally {
      setLaunching(false);
    }
  }

  async function handleClear() {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    if (sess == null || win == null) return;
    try {
      await setPaneAssignment(sess, win, props.pane.pane_index, null);
      await cancelPaneCommand(sess, win, props.pane.pane_index);
      await new Promise((r) => setTimeout(r, 500));
      await sendToPane(sess, win, props.pane.pane_index, "cd ~ && clear");
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] clear error:", e);
    }
  }

  async function handleClaim() {
    const proj = detectedProject();
    if (!proj) return;
    try {
      const sess = state.selectedTmuxSession;
      const win = state.selectedTmuxWindow;
      if (sess == null || win == null) return;
      await setPaneAssignment(sess, win, props.pane.pane_index, proj.encoded_name);
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] claim error:", e);
    }
  }

  async function handleUnassign() {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    try {
      if (sess != null && win != null) {
        await cancelPaneCommand(sess, win, props.pane.pane_index);
      }
      await setPaneAssignment(sess ?? "", win ?? 0, props.pane.pane_index, null);
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] unassign error:", e);
    }
  }

  async function handleKillPane() {
    const sess = state.selectedTmuxSession;
    const win = state.selectedTmuxWindow;
    if (sess == null || win == null) return;
    try {
      await setPaneAssignment(sess, win, props.pane.pane_index, null);
      await killPane(sess, win, props.pane.pane_index);
      refreshTmuxState();
      refreshProjects();
    } catch (e) {
      console.error("[PaneSlot] kill pane error:", e);
    }
  }

  function handleOpenDirectory() {
    const proj = effectiveProject();
    if (!proj) return;
    openDirectory(fromWslPath(proj.actual_path));
  }

  // Close overflow menu on outside click
  function handleDocumentClick(e: MouseEvent) {
    if (!menuOpen()) return;
    const target = e.target as HTMLElement;
    if (!target.closest?.(".pane-slot-menu-root")) {
      setMenuOpen(false);
    }
  }
  onMount(() => document.addEventListener("click", handleDocumentClick));
  onCleanup(() => document.removeEventListener("click", handleDocumentClick));

  function runMenuAction(fn: () => void | Promise<void>) {
    return (e: MouseEvent) => {
      e.stopPropagation();
      setMenuOpen(false);
      fn();
    };
  }

  const project = () => effectiveProject();
  const showResume = () => hasProject() && !isRunningClaude();
  const showClaim = () => isDetected();

  return (
    <div
      ref={(el) => { draggable(el); droppable(el); }}
      class={`pane-slot ${props.assignment ? "assigned" : ""} ${isDetected() ? "detected" : ""} ${draggable.isActiveDraggable ? "dragging" : ""} ${isWaitingApproval() ? "waiting-approval" : ""} ${isPaneSelectMode() ? "pane-selectable" : ""}`}
      classList={{ "drop-active": droppable.isActiveDroppable }}
      onClick={(e) => { if (isPaneSelectMode()) handlePaneSelectClick(e); }}
    >
      {/* Corner index */}
      <span class="pane-slot-index" title={`Pane ${props.pane.pane_index}`}>
        {props.pane.pane_index}
      </span>

      <Show
        when={hasProject() || isRunningClaude()}
        fallback={
          <div class="pane-slot-empty">
            <div class="pane-slot-empty-inner">
              <Terminal size={18} />
              <span>Drop a project here</span>
            </div>
          </div>
        }
      >
        <div class="pane-slot-body">
          {/* Primary row — project name + status chip + account */}
          <div class="pane-slot-primary">
            <span class="pane-slot-title" title={projectName() || deriveName(props.pane.current_path)}>
              {projectName() || deriveName(props.pane.current_path)}
            </span>
            <AccountBadge compact pane={props.pane} />
            <Show when={statusKind()}>
              <StatusChip status={statusKind()!} compact title={statusTooltip()} />
            </Show>
          </div>

          {/* Secondary row — branch + detected badge */}
          <div class="pane-slot-secondary">
            <Show when={project()?.git_branch}>
              <span class="pane-slot-branch" title="Git branch">
                <GitBranch size={11} />
                <span>{project()!.git_branch}</span>
                <Show when={project()!.is_linked_worktree}>
                  <Link size={11} />
                </Show>
              </span>
            </Show>
            <Show when={isDetected()}>
              <span class="pane-slot-detected-tag" title="Auto-detected from working directory">detected</span>
            </Show>
          </div>

          {/* Actions row */}
          <div class="pane-slot-actions">
            <Show when={showResume()}>
              <Button
                variant="primary"
                size="sm"
                disabled={launching()}
                onClick={(e) => { e.stopPropagation(); handleResume(); }}
                title="Resume Claude in this pane"
              >
                {launching() ? "…" : "Resume"}
              </Button>
            </Show>
            <Show when={showClaim()}>
              <Button
                variant="secondary"
                size="sm"
                onClick={(e) => { e.stopPropagation(); handleClaim(); }}
                title="Assign this project to this pane"
              >
                Claim
              </Button>
            </Show>
            <Show when={isRunningClaude() && !hasProject()}>
              <Button
                variant="secondary"
                size="sm"
                onClick={(e) => { e.stopPropagation(); handleClear(); }}
                title="Stop Claude and reset this pane"
              >
                Clear
              </Button>
            </Show>

            {/* Overflow menu */}
            <div class="pane-slot-menu-root">
              <button
                class="pane-slot-overflow"
                onClick={(e) => { e.stopPropagation(); setMenuOpen(!menuOpen()); }}
                onPointerDown={(e) => e.stopPropagation()}
                title="More actions"
                aria-label="More actions"
              >
                <MoreHorizontal size={14} />
              </button>
              <Show when={menuOpen()}>
                <div class="pane-slot-menu" onClick={(e) => e.stopPropagation()}>
                  <Show when={project()}>
                    <button class="pane-slot-menu-item" onClick={runMenuAction(() => openProjectSettings(project()!, props.pane.pane_index))}>
                      <SettingsIcon size={13} /> <span>Project settings</span>
                    </button>
                  </Show>
                  <Show when={hasProject() && !isRunningClaude()}>
                    <button class="pane-slot-menu-item" onClick={runMenuAction(handleNewSession)}>
                      <Plus size={13} /> <span>New Claude session</span>
                    </button>
                  </Show>
                  <Show when={project()}>
                    <button class="pane-slot-menu-item" onClick={runMenuAction(handleOpenDirectory)}>
                      <FolderOpen size={13} /> <span>Open directory</span>
                    </button>
                  </Show>
                  <Show when={isWaitingApproval() || isMuted()}>
                    <button class="pane-slot-menu-item" onClick={runMenuAction(toggleMute)}>
                      {isMuted() ? <Bell size={13} /> : <BellOff size={13} />}
                      <span>{isMuted() ? "Unmute notifications" : "Mute notifications"}</span>
                    </button>
                  </Show>
                  <Show when={props.assignment}>
                    <button class="pane-slot-menu-item" onClick={runMenuAction(handleUnassign)}>
                      <span class="pane-slot-menu-icon-placeholder" /> <span>Unassign project</span>
                    </button>
                  </Show>
                  <Show when={hasProject() || isRunningClaude()}>
                    <button class="pane-slot-menu-item" onClick={runMenuAction(handleClear)}>
                      <span class="pane-slot-menu-icon-placeholder" /> <span>Clear pane</span>
                    </button>
                  </Show>
                  <button class="pane-slot-menu-item danger" onClick={runMenuAction(handleKillPane)}>
                    <Trash2 size={13} /> <span>Kill pane</span>
                  </button>
                </div>
              </Show>
            </div>
          </div>
        </div>
      </Show>

      {/* Confirm replace in pane-select mode */}
      <Show when={confirmReplace()}>
        <div class="pane-select-confirm" onClick={(e) => e.stopPropagation()}>
          <p>Replace <strong>{projectName()}</strong> in this pane?</p>
          <div class="pane-select-confirm-actions">
            <button class="modal-btn" onClick={(e) => { e.stopPropagation(); setConfirmReplace(false); }}>Cancel</button>
            <button class="modal-btn danger" onClick={(e) => { e.stopPropagation(); setConfirmReplace(false); executePendingLaunch(); }}>Replace</button>
          </div>
        </div>
      </Show>
    </div>
  );
}
