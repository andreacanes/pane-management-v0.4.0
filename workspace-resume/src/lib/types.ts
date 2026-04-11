export interface ProjectInfo {
  encoded_name: string;
  actual_path: string;
  session_count: number;
  path_exists: boolean;
  git_branch?: string;
  is_linked_worktree?: boolean;
  worktree_count?: number;
}

export interface SessionUsage {
  session_id: string;
  model: string | null;
  input_tokens: number;
  output_tokens: number;
  cache_write_tokens: number;
  cache_read_tokens: number;
  message_count: number;
  cost_usd: number;
}

export interface ProjectUsage {
  encoded_name: string;
  sessions: SessionUsage[];
  total_input: number;
  total_output: number;
  total_cache_write: number;
  total_cache_read: number;
  total_cost_usd: number;
  total_messages: number;
}

export interface UsageSummary {
  projects: number;
  sessions: number;
  total_input: number;
  total_output: number;
  total_cache_write: number;
  total_cache_read: number;
  total_cost_usd: number;
}

export interface GitInfo {
  branch: string | null;
  worktree_root: string | null;
  is_linked_worktree: boolean;
  worktree_count: number;
}

export interface ActivePane {
  id: string;
  session_name: string;
  window_index: number;
  window_name: string;
  pane_index: number;
  current_command: string;
  current_path: string;
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
