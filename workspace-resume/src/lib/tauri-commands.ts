import { invoke } from "@tauri-apps/api/core";
import type {
  ProjectInfo,
  SessionInfo,
  ResumeResult,
  ActiveSession,
  TerminalSettings,
  ErrorLogEntry,
  TmuxSession,
  TmuxWindow,
  TmuxPane,
  TmuxState,
  ProjectMeta,
  PanePreset,
  WindowPaneStatus,
} from "./types";

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

export async function deleteSession(encodedProject: string, sessionId: string): Promise<void> {
  return invoke("delete_session", { encodedProject, sessionId });
}

// Phase 2: Resume commands

export async function resumeSession(
  encodedProject: string,
  sessionId: string,
  projectPath: string,
): Promise<ResumeResult> {
  return invoke("resume_session", { encodedProject, sessionId, projectPath });
}

export async function getActiveSessions(): Promise<ActiveSession[]> {
  return invoke("get_active_sessions");
}

export async function getTerminalSettings(): Promise<TerminalSettings> {
  return invoke("get_terminal_settings");
}

export async function updateTerminalSettings(backend: string): Promise<TerminalSettings> {
  return invoke("update_terminal_settings", { backend });
}

export async function updateTmuxSessionName(sessionName: string): Promise<TerminalSettings> {
  return invoke("update_tmux_session_name", { sessionName });
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

export async function listTmuxWindows(sessionName: string): Promise<TmuxWindow[]> {
  return invoke("list_tmux_windows", { sessionName });
}

export async function listTmuxPanes(sessionName: string, windowIndex: number): Promise<TmuxPane[]> {
  return invoke("list_tmux_panes", { sessionName, windowIndex });
}

export async function getTmuxState(sessionName: string, windowIndex: number): Promise<TmuxState> {
  return invoke("get_tmux_state", { sessionName, windowIndex });
}

export async function createPane(sessionName: string, windowIndex: number, direction: string): Promise<TmuxPane[]> {
  return invoke("create_pane", { sessionName, windowIndex, direction });
}

export async function applyLayout(sessionName: string, windowIndex: number, layout: string): Promise<TmuxPane[]> {
  return invoke("apply_layout", { sessionName, windowIndex, layout });
}

export async function sendToPane(sessionName: string, windowIndex: number, paneIndex: number, command: string): Promise<void> {
  return invoke("send_to_pane", { sessionName, windowIndex, paneIndex, command });
}

export async function cancelPaneCommand(sessionName: string, windowIndex: number, paneIndex: number): Promise<void> {
  return invoke("cancel_pane_command", { sessionName, windowIndex, paneIndex });
}

export async function killPane(sessionName: string, windowIndex: number, paneIndex: number): Promise<TmuxPane[]> {
  return invoke("kill_pane", { sessionName, windowIndex, paneIndex });
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

export async function killSession(sessionName: string): Promise<TmuxSession[]> {
  return invoke("kill_session", { sessionName });
}

export async function setupPaneGrid(sessionName: string, windowIndex: number, cols: number, rows: number): Promise<TmuxPane[]> {
  return invoke("setup_pane_grid", { sessionName, windowIndex, cols, rows });
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

export async function getPaneAssignments(sessionName: string, windowIndex: number): Promise<Record<string, string>> {
  return invoke("get_pane_assignments", { sessionName, windowIndex });
}

export async function getPaneAssignmentsRaw(): Promise<Record<string, string>> {
  return invoke("get_pane_assignments_raw");
}

export async function setPaneAssignment(sessionName: string, windowIndex: number, paneIndex: number, encodedProject: string | null): Promise<Record<string, string>> {
  return invoke("set_pane_assignment", { sessionName, windowIndex, paneIndex, encodedProject });
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

