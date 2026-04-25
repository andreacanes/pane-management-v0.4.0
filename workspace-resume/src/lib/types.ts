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
  start_command: string;
  pane_pid?: string;
  claude_account?: "andrea" | "bravura" | "sully" | null;
  /** Host the pane runs on — `"local"` for WSL tmux, any other value
   *  is an SSH alias (e.g. `"mac"`). Stamped by the cross-host scanner. */
  host: string;
}

export interface SessionInfo {
  session_id: string;
  first_timestamp: string | null;
  last_timestamp: string | null;
  last_user_message: string | null;
  is_corrupted: boolean;
  file_size_bytes: number;
}

export interface TerminalSettings {
  tmux_session_name: string;
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
  start_command?: string;
  /** Top-level process PID for this pane (usually the shell). */
  pane_pid?: string;
  /** Server-detected Claude profile ("andrea" | "bravura" | "sully"). */
  claude_account?: "andrea" | "bravura" | "sully" | null;
  /** Window index this pane belongs to (stamped by the backend). */
  window_index: number;
  /** Short git branch name at current_path, null when not a git repo. */
  git_branch?: string | null;
  /** True when current_path is a linked (non-primary) git worktree. */
  is_worktree?: boolean;
  /**
   * Host the pane lives on. `"local"` for WSL tmux, any other value is an
   * SSH alias (typically `"mac"`). Stamped by the backend command. Part
   * of the full coordinate `(host, session_name, window_index, pane_index)`.
   */
  host: string;
  /**
   * tmux session this pane belongs to. Stamped by the backend. Local
   * panes use the selected tmux session name; remote panes can come
   * from any Mac session (one per project in the `cc` convention).
   */
  session_name: string;
  /**
   * Set when this pane is a local SSH mirror — i.e. its `start_command`
   * is `ssh -t <alias> tmux attach-session -t <session>`, used as a
   * viewport into a remote tmux server. The backend stamps this via
   * `services::ssh_mirror::parse_mirror_target` so every client reads
   * one unambiguous field instead of each rolling its own regex.
   * `null` / absent for ordinary panes; the project_encoded_name and
   * project_display_name are skipped on the wire when mirror_target is
   * set, so this is the canonical signal.
   */
  mirror_target?: { alias: string; session: string } | null;
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
  encoded_project: string;
  /** `"local"` (WSL) or an SSH alias such as `"mac"`. Default `"local"`. */
  host: string;
  /** `"andrea"` | `"bravura"` | `"sully"` — matches the Rust registry. */
  account: string;
}

export interface WindowPaneStatus {
  has_active: boolean;
  active_panes: number[];
  active_paths: string[];
  waiting_panes: number[];
}

export interface CompanionConfig {
  bearer_token: string;
  hook_secret: string;
  ntfy_topic: string;
  port: number;
  bind: string;
  suggested_url: string;
}

/**
 * Wire shape of the companion's event broadcast — same tagged union the
 * APK consumes over `/api/v1/events`. In the desktop we receive these
 * via the in-process Tauri bridge (see `src-tauri/src/companion/mod.rs`:
 * `bridge_rx → app.emit("companion-event", ev)`), so the frontend only
 * subscribes to a single Tauri event channel and decodes with
 * `type`-based narrowing.
 *
 * Only variants the desktop frontend currently consumes are exhaustively
 * typed; the rest fall through the `{ type: string }` catch-all so new
 * Rust-side variants don't break decoding.
 */
export type CompanionEvent =
  | { type: "hello"; at: number }
  | { type: "snapshot"; panes: PaneDtoWire[]; approvals: unknown[]; attention?: unknown[] }
  | { type: "pane_updated"; pane: PaneDtoWire }
  | { type: "pane_state_changed"; pane_id: string; old: string; new: string; at: number }
  | { type: "pane_output_changed"; pane_id: string; tail: string[]; seq: number; at: number }
  | { type: "pane_removed"; pane_id: string; at: number }
  | {
      type: "window_focus_changed";
      host: string;
      session_name: string;
      window_index: number;
      at: number;
    }
  | { type: "session_started"; name: string; at: number }
  | { type: "session_ended"; name: string; at: number }
  | { type: string; [k: string]: unknown };

/**
 * Wire shape of a pane DTO as the companion emits it — a superset of
 * what the Tauri-direct `TmuxPane` carries. Only fields the frontend
 * reads on WebSocket updates are listed; the decoder preserves others
 * untouched for pass-through to `state.tmuxPanes`.
 *
 * Unlike `TmuxPane` this uses the wire id (`<host>/<session>:<window>.<pane>`
 * or `<session>:<window>.<pane>`) as the primary key, not a tuple.
 */
export interface PaneDtoWire {
  id: string;
  session_name: string;
  window_index: number;
  window_name: string;
  pane_index: number;
  current_command: string;
  current_path: string;
  state: string;
  waiting_reason?: string;
  last_output_preview: string[];
  project_encoded_name?: string | null;
  project_display_name?: string | null;
  claude_session_id?: string | null;
  claude_account?: string | null;
  host?: string | null;
  claude_effort?: string | null;
  updated_at: number;
  last_activity_at?: number | null;
  warning?: string | null;
  mirror_target?: { alias: string; session: string } | null;
}
