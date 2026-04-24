import { invoke } from "@tauri-apps/api/core";
import type {
  ProjectInfo,
  SessionInfo,
  TerminalSettings,
  ErrorLogEntry,
  TmuxSession,
  TmuxWindow,
  TmuxPane,
  TmuxState,
  ProjectMeta,
  PanePreset,
  WindowPaneStatus,
  ProjectUsage,
  UsageSummary,
  GitInfo,
  ActivePane,
  CompanionConfig,
} from "./types";

export type { CompanionConfig };

export async function listProjects(): Promise<ProjectInfo[]> {
  return invoke("list_projects");
}

export async function listSessions(
  encodedProject: string,
): Promise<SessionInfo[]> {
  return invoke("list_sessions", { encodedProject });
}

export async function checkContinuityExists(path: string): Promise<boolean> {
  return invoke("check_continuity_exists", { path });
}

export async function openDirectory(path: string): Promise<void> {
  return invoke("open_directory", { path });
}

export async function createProjectFolder(parent: string, name: string): Promise<string> {
  return invoke("create_project_folder", { parent, name });
}

export async function deleteSession(encodedProject: string, sessionId: string): Promise<void> {
  return invoke("delete_session", { encodedProject, sessionId });
}

export async function getTerminalSettings(): Promise<TerminalSettings> {
  return invoke("get_terminal_settings");
}

export async function updateTmuxSessionName(sessionName: string): Promise<TerminalSettings> {
  return invoke("update_tmux_session_name", { sessionName });
}

// Phase B1: Usage
export async function getProjectUsage(encodedName: string): Promise<ProjectUsage> {
  return invoke("get_project_usage", { encodedName });
}

export async function getAllUsage(): Promise<Record<string, ProjectUsage>> {
  return invoke("get_all_usage");
}

export async function getUsageSummary(): Promise<UsageSummary> {
  return invoke("get_usage_summary");
}

// Phase B2: Worktree / git
export async function getGitInfo(path: string): Promise<GitInfo> {
  return invoke("get_git_info", { path });
}

export async function createWorktree(projectPath: string, slug: string): Promise<string> {
  return invoke("create_worktree", { projectPath, slug });
}

// Phase B4: Global active view
export async function listActiveClaudePanes(): Promise<ActivePane[]> {
  return invoke("list_active_claude_panes");
}

// Companion admin: bearer token / QR / rotation

export async function getCompanionConfig(): Promise<CompanionConfig> {
  return invoke("get_companion_config");
}

export async function getCompanionQr(): Promise<string> {
  return invoke("get_companion_qr");
}

export async function rotateCompanionToken(): Promise<CompanionConfig> {
  return invoke("rotate_companion_token");
}

export async function getErrorLog(): Promise<ErrorLogEntry[]> {
  return invoke("get_error_log");
}

export async function clearErrorLog(): Promise<void> {
  return invoke("clear_error_log");
}

// Phase 3: tmux commands

export async function listTmuxSessions(): Promise<TmuxSession[]> {
  return invoke("list_tmux_sessions");
}

/** Host-aware tmux session list. `host = "local"` matches legacy
 *  `listTmuxSessions`; any other value is an SSH alias (e.g. `"mac"`). */
export async function listTmuxSessionsOn(host: string): Promise<TmuxSession[]> {
  return invoke("list_tmux_sessions_on", { host });
}

export async function listTmuxWindows(sessionName: string): Promise<TmuxWindow[]> {
  return invoke("list_tmux_windows", { sessionName });
}

export async function listTmuxPanes(sessionName: string, windowIndex: number): Promise<TmuxPane[]> {
  return invoke("list_tmux_panes", { sessionName, windowIndex });
}

/** Host-aware per-window pane list. For the grid's remote-host rows,
 *  callers typically prefer `listTmuxPanesAllOn(host)` since remote
 *  sessions + windows aren't usually pre-selected in the UI. */
export async function listTmuxPanesOn(host: string, sessionName: string, windowIndex: number): Promise<TmuxPane[]> {
  return invoke("list_tmux_panes_on", { host, sessionName, windowIndex });
}

/** Every pane on one host in a single SSH round-trip. Used to populate
 *  Mac-side rows in the unified pane grid — the frontend then filters
 *  to "running Claude OR has an assignment" before rendering. */
export async function listTmuxPanesAllOn(host: string): Promise<TmuxPane[]> {
  return invoke("list_tmux_panes_all_on", { host });
}

export async function getTmuxState(sessionName: string, windowIndex: number): Promise<TmuxState> {
  return invoke("get_tmux_state", { sessionName, windowIndex });
}

export async function createPane(sessionName: string, windowIndex: number, direction: string): Promise<TmuxPane[]> {
  return invoke("create_pane", { sessionName, windowIndex, direction });
}

/** Host-aware tmux split. Routes `tmux split-window` to the named
 *  host via SSH when `host !== "local"`. */
export async function createPaneOn(host: string, sessionName: string, windowIndex: number, direction: string): Promise<TmuxPane[]> {
  return invoke("create_pane_on", { host, sessionName, windowIndex, direction });
}

export async function applyLayout(sessionName: string, windowIndex: number, layout: string): Promise<TmuxPane[]> {
  return invoke("apply_layout", { sessionName, windowIndex, layout });
}

export async function sendToPane(sessionName: string, windowIndex: number, paneIndex: number, command: string): Promise<void> {
  return invoke("send_to_pane", { sessionName, windowIndex, paneIndex, command });
}

/** Host-aware `tmux send-keys` — routes to the named host's tmux over
 *  SSH when `host !== "local"`. Used by assignToPane / forkPaneSession
 *  for the `cd` step that precedes a launch on a Mac pane. */
export async function sendToPaneOn(host: string, sessionName: string, windowIndex: number, paneIndex: number, command: string): Promise<void> {
  return invoke("send_to_pane_on_host", { host, sessionName, windowIndex, paneIndex, command });
}

export async function cancelPaneCommand(sessionName: string, windowIndex: number, paneIndex: number): Promise<void> {
  return invoke("cancel_pane_command", { sessionName, windowIndex, paneIndex });
}

/** Host-aware Ctrl-C: routes the two-shot cancel to the pane's actual
 *  host so Mac panes get their Ctrl-C on the Mac tmux server. */
export async function cancelPaneCommandOn(host: string, sessionName: string, windowIndex: number, paneIndex: number): Promise<void> {
  return invoke("cancel_pane_command_on", { host, sessionName, windowIndex, paneIndex });
}

export async function killPane(sessionName: string, windowIndex: number, paneIndex: number): Promise<TmuxPane[]> {
  return invoke("kill_pane", { sessionName, windowIndex, paneIndex });
}

/** Host-aware `tmux kill-pane`. Routes to the pane's actual host. */
export async function killPaneOn(host: string, sessionName: string, windowIndex: number, paneIndex: number): Promise<TmuxPane[]> {
  return invoke("kill_pane_on", { host, sessionName, windowIndex, paneIndex });
}

export async function createWindow(sessionName: string): Promise<TmuxWindow[]> {
  return invoke("create_window", { sessionName });
}

export async function killWindow(sessionName: string, windowIndex: number): Promise<TmuxWindow[]> {
  return invoke("kill_window", { sessionName, windowIndex });
}

export async function switchTmuxSession(sessionName: string): Promise<void> {
  return invoke("switch_tmux_session", { sessionName });
}

export async function selectTmuxWindowCmd(sessionName: string, windowIndex: number): Promise<void> {
  return invoke("select_tmux_window", { sessionName, windowIndex });
}

export async function renameSession(oldName: string, newName: string): Promise<void> {
  return invoke("rename_session", { oldName, newName });
}

export async function renameWindow(sessionName: string, windowIndex: number, newName: string): Promise<void> {
  return invoke("rename_window", { sessionName, windowIndex, newName });
}

export async function createSession(sessionName: string): Promise<TmuxSession[]> {
  return invoke("create_session", { sessionName });
}

/** Host-aware `tmux new-session -d`. Used by CreatePaneModal when the
 *  user picks a brand-new Mac session. Returns the post-creation
 *  session list for the host. */
export async function createSessionOn(host: string, sessionName: string): Promise<TmuxSession[]> {
  return invoke("create_session_on", { host, sessionName });
}

export async function killSession(sessionName: string): Promise<TmuxSession[]> {
  return invoke("kill_session", { sessionName });
}

export async function setupPaneGrid(sessionName: string, windowIndex: number, cols: number, rows: number): Promise<TmuxPane[]> {
  return invoke("setup_pane_grid", { sessionName, windowIndex, cols, rows });
}

export async function reflowPaneGrid(sessionName: string, windowIndex: number, cols: number, rows: number): Promise<TmuxPane[]> {
  return invoke("reflow_pane_grid", { sessionName, windowIndex, cols, rows });
}

export async function reducePaneGrid(sessionName: string, windowIndex: number, cols: number, rows: number): Promise<TmuxPane[]> {
  return invoke("reduce_pane_grid", { sessionName, windowIndex, cols, rows });
}

export async function listKillTargets(sessionName: string, windowIndex: number, keepCount: number): Promise<TmuxPane[]> {
  return invoke("list_kill_targets", { sessionName, windowIndex, keepCount });
}

export async function swapTmuxPane(sessionName: string, windowIndex: number, sourcePane: number, targetPane: number): Promise<TmuxPane[]> {
  return invoke("swap_tmux_pane", { sessionName, windowIndex, sourcePane, targetPane });
}

export async function tmuxResurrectSave(): Promise<string> {
  return invoke("tmux_resurrect_save");
}

export async function tmuxResurrectRestore(): Promise<string> {
  return invoke("tmux_resurrect_restore");
}

export async function swapTmuxWindow(sessionName: string, sourceIndex: number, targetIndex: number): Promise<TmuxWindow[]> {
  return invoke("swap_tmux_window", { sessionName, sourceIndex, targetIndex });
}

export async function getSessionOrder(): Promise<string[]> {
  return invoke("get_session_order");
}

export async function setSessionOrder(order: string[]): Promise<void> {
  return invoke("set_session_order", { order });
}

export async function getPinnedOrder(): Promise<string[]> {
  return invoke("get_pinned_order");
}

export async function setPinnedOrder(order: string[]): Promise<void> {
  return invoke("set_pinned_order", { order });
}

// Phase 3: project metadata commands

export async function getAllProjectMeta(): Promise<Record<string, ProjectMeta>> {
  return invoke("get_all_project_meta");
}

export async function setProjectTier(encodedName: string, tier: string): Promise<ProjectMeta> {
  return invoke("set_project_tier", { encodedName, tier });
}

export async function setDisplayName(encodedName: string, name: string | null): Promise<ProjectMeta> {
  return invoke("set_display_name", { encodedName, name });
}

export async function setSessionBinding(encodedName: string, sessionId: string | null): Promise<ProjectMeta> {
  return invoke("set_session_binding", { encodedName, sessionId });
}

export async function getSessionNames(encodedProject: string): Promise<Record<string, string>> {
  return invoke("get_session_names", { encodedProject });
}

export async function setSessionNames(
  encodedProject: string,
  names: Record<string, string>,
): Promise<void> {
  return invoke("set_session_names", { encodedProject, names });
}

// Phase 3: pane preset commands

export async function getPanePresets(): Promise<PanePreset[]> {
  return invoke("get_pane_presets");
}

export async function savePanePreset(name: string, layout: string, paneCount: number): Promise<PanePreset> {
  return invoke("save_pane_preset", { name, layout, paneCount });
}

export async function deletePanePreset(name: string): Promise<void> {
  return invoke("delete_pane_preset", { name });
}

/** Scoped pane_index → encoded_project map for one `(host, session, window)`.
 *  Host became a required coordinate after the refactor — pass `"local"`
 *  for the WSL scope. */
export async function getPaneAssignments(host: string, sessionName: string, windowIndex: number): Promise<Record<string, string>> {
  return invoke("get_pane_assignments", { host, sessionName, windowIndex });
}

/** Entire pane-assignment map, keyed on the full 4-segment coord
 *  `"host|session|window|pane"`. Resurrect and the multi-host grid
 *  both consume this shape. */
export async function getPaneAssignmentsRaw(): Promise<Record<string, string>> {
  return invoke("get_pane_assignments_raw");
}

/** Create / update / delete one assignment slot. Host is part of the
 *  coordinate — pass `"mac"` for Mac-side slots. `encodedProject = null`
 *  deletes the slot. Returns the post-mutation pane_index → encoded_project
 *  map for the same `(host, session, window)` scope. */
export async function setPaneAssignment(host: string, sessionName: string, windowIndex: number, paneIndex: number, encodedProject: string | null): Promise<Record<string, string>> {
  return invoke("set_pane_assignment", { host, sessionName, windowIndex, paneIndex, encodedProject });
}

/** Scoped full-struct map for one `(host, session, window)`. Drives
 *  per-slot host badge + account dropdown in the grid. */
export async function getPaneAssignmentsFull(host: string, sessionName: string, windowIndex: number): Promise<Record<string, import("./types").PaneAssignment>> {
  return invoke("get_pane_assignments_full", { host, sessionName, windowIndex });
}

/** Every assignment as `{ "host|session|window|pane": PaneAssignment }`.
 *  Used by AppContext to merge local and remote assignments in a
 *  single reactive store without one round-trip per slot. */
export async function getAllPaneAssignmentsFull(): Promise<Record<string, import("./types").PaneAssignment>> {
  return invoke("get_all_pane_assignments_full");
}

/** Change the `account` on an existing assignment at `(host, session, window, pane)`.
 *  Host is the lookup key — to move a slot between hosts, delete the
 *  old coord via `setPaneAssignment(..., null)` and create the new one.
 *  Errors when the slot has no project. */
export async function setPaneAssignmentMeta(host: string, sessionName: string, windowIndex: number, paneIndex: number, account: string): Promise<void> {
  return invoke("set_pane_assignment_meta", { host, sessionName, windowIndex, paneIndex, account });
}

/** Assemble + send the Claude launch command to a pane on the given host.
 *  Replaces the previous TS-side `cd ... && ncld -r ...` string-building
 *  for the Mac-host path. Local hosts can still use the legacy sendToPane
 *  path unchanged. */
export async function launchInPane(opts: {
  session: string;
  window: number;
  pane: number;
  host: string;
  account: string;
  projectPath: string;
  resumeSid: string | null;
  yolo: boolean;
}): Promise<void> {
  return invoke("launch_in_pane", opts);
}

/** Debug/test surface for `services::launch_cmd::build_launch_command`. */
export async function buildLaunchCommand(opts: {
  host: string;
  account: string;
  projectPath: string;
  resumeSid: string | null;
  yolo: boolean;
}): Promise<string> {
  return invoke("build_launch_command", opts);
}

/** Kick off a new Mutagen bidirectional sync for a project between WSL
 *  and the Mac (`/Users/admin/projects/<name>`). Idempotent — re-running
 *  for an already-synced project is a no-op. Returns the helper's combined
 *  stdout so the UI can display the outcome. */
export async function syncProjectToMac(encodedProject: string): Promise<string> {
  return invoke("sync_project_to_mac", { encodedProject });
}

/** SSH aliases of currently-supported remote hosts (static ["mac"] for MVP). */
export async function listRemoteHosts(): Promise<string[]> {
  return invoke("list_remote_hosts");
}

/** Does `path` exist as a directory on `host`? Used by PaneSlot to gate
 *  the Host=Mac dropdown: if the project hasn't been mirrored yet, a
 *  Host=Mac launch would silently fail inside the pane. `host = "local"`
 *  checks WSL; any other value is treated as an SSH alias. */
export async function checkRemotePathExists(host: string, path: string): Promise<boolean> {
  return invoke("check_remote_path_exists", { host, path });
}

/** Is the OpenSSH ControlMaster socket for `alias` currently live?
 *  Returned bool drives the health dot next to the host dropdown. A
 *  dead master doesn't prevent launching — just means the next remote
 *  call pays a full SSH handshake (~500 ms) instead of the multiplexed
 *  fast path (~15 ms). */
export async function checkSshMaster(alias: string): Promise<boolean> {
  return invoke("check_ssh_master", { alias });
}

/** Create a detached tmux session on a remote host, targeting a project
 *  directory with a specific Claude account. Mirrors the `cc` CLI on
 *  Mac but detached so SSH without a TTY works. Returns [session, window, pane]. */
export async function launchProjectSessionOn(
  host: string,
  sessionName: string,
  projectPath: string,
  account: string,
): Promise<[string, number, number]> {
  return invoke("launch_project_session_on", {
    host,
    sessionName,
    projectPath,
    account,
  });
}

export async function checkPaneStatuses(sessionName: string): Promise<Record<string, WindowPaneStatus>> {
  return invoke("check_pane_statuses", { sessionName });
}

export async function updateProjectInode(encodedName: string, inode: number | null, claudeProjectDirs: string[] | null): Promise<void> {
  return invoke("update_project_inode", { encodedName, inode, claudeProjectDirs });
}

export async function getInode(path: string): Promise<number | null> {
  return invoke("get_inode", { path });
}

export async function findInodeInTree(root: string, targetInode: number, maxDepth: number): Promise<string | null> {
  return invoke("find_inode_in_tree", { root, targetInode, maxDepth });
}

