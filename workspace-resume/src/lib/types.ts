export interface ProjectInfo {
  encoded_name: string;
  actual_path: string;
  session_count: number;
  path_exists: boolean;
}

export interface SessionInfo {
  session_id: string;
  first_timestamp: string | null;
  last_timestamp: string | null;
  last_user_message: string | null;
  is_corrupted: boolean;
  file_size_bytes: number;
}

// Phase 2: Resume types
export type TerminalBackend = "tmux" | "warp" | "powershell";

export interface TerminalSettings {
  backend: TerminalBackend;
  tmux_session_name: string;
}

export interface ResumeResult {
  pid: number | null;
  terminal: string;
  session_id: string;
}

export interface ActiveSession {
  session_id: string;
  pid: number | null;
  terminal: string;
  is_alive: boolean;
}

export interface ErrorLogEntry {
  timestamp: string;
  terminal: string;
  error: string;
  project_path: string;
}

// Phase 3: Dashboard + tmux pane management types

export type ProjectTier = "pinned" | "active" | "paused" | "archived";

export interface ProjectMeta {
  display_name: string | null;
  tier: ProjectTier;
  bound_session: string | null;
  inode?: number | null;
  claude_project_dirs?: string[] | null;
}

export interface ProjectWithMeta extends ProjectInfo {
  meta: ProjectMeta;
}

export interface TmuxSession {
  name: string;
  windows: number;
  attached: boolean;
}

export interface TmuxWindow {
  index: number;
  name: string;
  panes: number;
  active: boolean;
}

export interface TmuxPane {
  pane_id: string;
  pane_index: number;
  width: number;
  height: number;
  top: number;
  left: number;
  active: boolean;
  current_command: string;
  current_path: string;
}

export interface TmuxState {
  sessions: TmuxSession[];
  windows: TmuxWindow[];
  panes: TmuxPane[];
}

export interface PanePreset {
  name: string;
  layout: string;
  pane_count: number;
}

export interface PaneAssignment {
  pane_index: number;
  encoded_project: string | null;
}

export interface WindowPaneStatus {
  has_active: boolean;
  active_panes: number[];
  active_paths: string[];
  waiting_panes: number[];
}
