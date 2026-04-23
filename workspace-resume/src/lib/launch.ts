import {
  sendToPane,
  setPaneAssignment,
  createPane,
  listSessions,
  cancelPaneCommand,
  launchInPane,
} from "./tauri-commands";
import type { TmuxPane } from "./types";
import { toWslPath } from "./path";

/**
 * Derive the Mac-side project path for a Mutagen-synced project.
 * Convention (from `mac_studio_bridge` memory): `/home/andrea/<basename>`
 * on WSL ↔ `/Users/admin/projects/<basename>` on the Mac. The basename
 * matches on both sides because `sync-add-project` preserves it.
 */
function toMacPath(wslProjectPath: string): string {
  const parts = wslProjectPath.split("/").filter((s) => s.length > 0);
  const basename = parts[parts.length - 1] ?? "";
  return `/Users/admin/projects/${basename}`;
}

// Re-export toWslPath so existing imports from launch.ts still work.
export { toWslPath } from "./path";

/**
 * The shell token used to launch/resume Claude in a pane. Andrea invokes
 * Claude exclusively through the `ncld` bash function defined in ~/.bashrc,
 * which wraps `cli-ncld-114.bin` with `--dangerously-skip-permissions
 * --effort max`. The bare `claude` binary may not be on PATH here. Using
 * `ncld` ensures the Resurrect button and every per-project Resume button
 * runs under the same wrapper flags as an interactive launch.
 *
 * Hardcoded rather than a Tauri-store setting because this is a personal
 * fork; portability to upstream is not a goal.
 */
const CLAUDE_CMD = "ncld";

/**
 * Find or create an available pane. Returns the pane index.
 */
async function resolvePaneIndex(opts: {
  tmuxSession: string;
  tmuxWindow: number;
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  targetPaneIndex?: number;
}): Promise<number> {
  if (opts.targetPaneIndex != null) return opts.targetPaneIndex;

  const assignedIndices = new Set(
    Object.keys(opts.paneAssignments).map((k) => Number(k)),
  );

  let targetPane = opts.tmuxPanes.find(
    (p) => !assignedIndices.has(p.pane_index),
  );

  if (!targetPane) {
    const newPanes = await createPane(opts.tmuxSession, opts.tmuxWindow, "h");
    targetPane = newPanes.find(
      (p) => !assignedIndices.has(p.pane_index),
    );
    if (!targetPane && newPanes.length > 0) {
      targetPane = newPanes[newPanes.length - 1];
    }
    if (!targetPane) {
      throw new Error("Failed to find or create an available pane");
    }
  }

  return targetPane.pane_index;
}

/**
 * Assign a project to a pane and cd to its directory. Does NOT launch claude.
 * Use this for drag-to-pane — user decides when to resume.
 *
 * If the target pane is currently running a process (e.g. Claude from a
 * previous assignment), sends Ctrl-C first so the cd command reaches the
 * shell instead of being interpreted by the running process.
 */
export async function assignToPane(opts: {
  tmuxSession: string;
  tmuxWindow: number;
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  encodedProject: string;
  projectPath: string;
  targetPaneIndex?: number;
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // If a process is running in this pane, cancel it first so the cd
  // goes to the shell, not to the running program.
  const targetPane = opts.tmuxPanes.find((p) => p.pane_index === paneIndex);
  const cmd = targetPane?.current_command?.toLowerCase() ?? "";
  if (cmd && cmd !== "bash" && cmd !== "zsh" && cmd !== "sh" && cmd !== "-") {
    await cancelPaneCommand(opts.tmuxSession, opts.tmuxWindow, paneIndex);
    // Small delay for the process to exit and shell prompt to return
    await new Promise((r) => setTimeout(r, 500));
  }

  await setPaneAssignment(opts.tmuxSession, opts.tmuxWindow, paneIndex, opts.encodedProject);

  const wslPath = toWslPath(opts.projectPath);
  await sendToPane(opts.tmuxSession, opts.tmuxWindow, paneIndex, `cd "${wslPath}"`);

  return paneIndex;
}

/**
 * Assign a project to a pane, cd to its directory, AND launch claude -r.
 * Use this for Resume buttons — auto-starts the session.
 */
export async function launchToPane(opts: {
  tmuxSession: string;
  tmuxWindow: number;
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  encodedProject: string;
  projectPath: string;
  sessionId?: string | null;
  boundSession?: string | null;
  targetPaneIndex?: number;
  yolo?: boolean;
  /** `"local"` (default, unchanged behaviour) or an SSH alias like `"mac"`. */
  host?: string;
  /** `"andrea"` (default) | `"bravura"` | `"sully"`. */
  account?: string;
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // Always cancel first — Ctrl-C twice, then wait for shell prompt
  await cancelPaneCommand(opts.tmuxSession, opts.tmuxWindow, paneIndex);
  await new Promise((r) => setTimeout(r, 800));

  await setPaneAssignment(opts.tmuxSession, opts.tmuxWindow, paneIndex, opts.encodedProject);

  // Determine which session ID to use: explicit > bound > most-recent
  let resumeId: string | null | undefined = opts.sessionId || opts.boundSession;

  if (!resumeId) {
    try {
      const sessions = await listSessions(opts.encodedProject);
      const validSession = sessions.find((s) => !s.is_corrupted && s.file_size_bytes > 500);
      if (validSession) {
        resumeId = validSession.session_id;
      }
    } catch (e) {
      console.warn("[launchToPane] failed to auto-resolve session:", e);
    }
  }

  const host = opts.host ?? "local";
  const account = opts.account ?? "andrea";

  if (host !== "local") {
    // Remote host: Rust-side launch_in_pane assembles the cd-then-mcld
    // string and routes it over SSH to the target host's tmux.
    await launchInPane({
      session: opts.tmuxSession,
      window: opts.tmuxWindow,
      pane: paneIndex,
      host,
      account,
      projectPath: toMacPath(opts.projectPath),
      resumeSid: resumeId ?? null,
      yolo: opts.yolo ?? false,
    });
    return paneIndex;
  }

  // Local path (unchanged pre-integration behaviour).
  const wslPath = toWslPath(opts.projectPath);
  const yoloFlag = opts.yolo ? " --dangerously-skip-permissions" : "";
  const envPrefix = account === "bravura"
    ? `CLAUDE_CONFIG_DIR="$HOME/.claude-b" `
    : account === "sully"
      ? `CLAUDE_CONFIG_DIR="$HOME/.claude-c" `
      : "";
  const claudeCmd = resumeId
    ? `${envPrefix}${CLAUDE_CMD} -r ${resumeId}${yoloFlag}`
    : `${envPrefix}${CLAUDE_CMD} -r${yoloFlag}`;

  // Chain cd + claude as a single command so claude only starts after cd completes
  await sendToPane(opts.tmuxSession, opts.tmuxWindow, paneIndex, `cd "${wslPath}" && ${claudeCmd}`);

  return paneIndex;
}

/**
 * Fork the conversation currently running in a source pane by driving
 * Claude's own `/branch` slash command — no process restart. Splits the
 * window to create a new sibling pane running the *original* session
 * resumed in place. The source pane keeps its process alive; Claude
 * creates a new session UUID Y internally and continues writing into it.
 *
 * Session id resolution (three steps, matches launchToPane priority):
 *   1. Explicit `sessionId` passed in by the caller.
 *   2. `boundSession` from project_meta.
 *   3. MRU fallback via `listSessions(encodedProject)` — the newest
 *      non-corrupted JSONL file. Works when the pane was restored by
 *      tmux-resurrect (start_command is a wrapper) and the project has
 *      exactly one active session. Wrong for multi-pane-same-project,
 *      which is rare; caller should pass `sessionId` explicitly when
 *      known.
 *
 * Operation order:
 *   1. Send `/branch` to source pane (Claude handles it as a slash
 *      command — creates Y.jsonl, continues in-place. Confirmed from
 *      Claude Code's built-in branching UX: the user's in-chat `/branch`
 *      emits "Branched conversation. You are now in the branch.").
 *   2. Wait ~1500 ms for Claude to create Y and stop writing to X.
 *   3. `split-window -h` to create pane B.
 *   4. Send `cd … && ncld -r X` to pane B so it resumes the original
 *      session Claude froze a moment ago.
 *
 * We do *not* Ctrl-C the source process — the user may have pending
 * tool output or in-flight approvals they don't want interrupted. The
 * slash command preserves all of that.
 */
export async function forkPaneSession(opts: {
  tmuxSession: string;
  tmuxWindow: number;
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  encodedProject: string;
  projectPath: string;
  sourcePaneIndex: number;
  sessionId?: string | null;
  boundSession?: string | null;
}): Promise<{ branchPaneIndex: number; originalPaneIndex: number; sessionId: string }> {
  let resolvedSid: string | null = opts.sessionId || opts.boundSession || null;

  if (!resolvedSid) {
    try {
      const sessions = await listSessions(opts.encodedProject);
      const valid = sessions.find((s) => !s.is_corrupted && s.file_size_bytes > 500);
      if (valid) resolvedSid = valid.session_id;
    } catch (e) {
      console.warn("[forkPaneSession] listSessions MRU failed:", e);
    }
  }

  if (!resolvedSid) {
    throw new Error(
      "fork: no session to fork — no explicit id, no bound_session, and no valid JSONL in project",
    );
  }

  // 1. Let Claude fork itself in-place via slash command.
  await sendToPane(opts.tmuxSession, opts.tmuxWindow, opts.sourcePaneIndex, "/branch");

  // 2. Give Claude time to mint Y, flush its last turn into X, and swap
  //    its open file descriptor to Y. After this window, pane A is
  //    writing Y and X.jsonl is no longer being appended to.
  await new Promise((r) => setTimeout(r, 1500));

  // Record the *immutable* pane ids (%N form) of all panes before the
  // split. After split-window, tmux renumbers remaining panes by spatial
  // layout (splitting pane 1 in a window with {1, 2} gives {1, 2, 3}
  // where the NEW pane becomes 2 and old pane 2 becomes 3). Comparing
  // by pane_index would mis-identify the new pane; comparing by pane_id
  // (tmux's %N, which never changes for the life of the pane) is
  // reliable.
  const preExistingPaneIds = new Set(opts.tmuxPanes.map((p) => p.pane_id));

  // 3. Split to create pane B.
  const newPanes = await createPane(opts.tmuxSession, opts.tmuxWindow, "h");

  const newPane = newPanes.find((p) => !preExistingPaneIds.has(p.pane_id));
  if (!newPane || newPane.pane_index === opts.sourcePaneIndex) {
    throw new Error("fork: split-window produced no identifiable new pane");
  }

  // 4. Resume the original session in the new pane.
  const wslPath = toWslPath(opts.projectPath);
  await sendToPane(
    opts.tmuxSession,
    opts.tmuxWindow,
    newPane.pane_index,
    `cd "${wslPath}" && ${CLAUDE_CMD} -r ${resolvedSid}`,
  );

  return {
    branchPaneIndex: opts.sourcePaneIndex,
    originalPaneIndex: newPane.pane_index,
    sessionId: resolvedSid,
  };
}

/**
 * Start a fresh Claude session (no -r) in a pane.
 */
export async function newSessionInPane(opts: {
  tmuxSession: string;
  tmuxWindow: number;
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  encodedProject: string;
  projectPath: string;
  targetPaneIndex?: number;
  yolo?: boolean;
  host?: string;
  account?: string;
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // Always cancel first — Ctrl-C twice, then wait for shell prompt
  await cancelPaneCommand(opts.tmuxSession, opts.tmuxWindow, paneIndex);
  await new Promise((r) => setTimeout(r, 800));

  await setPaneAssignment(opts.tmuxSession, opts.tmuxWindow, paneIndex, opts.encodedProject);

  const host = opts.host ?? "local";
  const account = opts.account ?? "andrea";

  if (host !== "local") {
    await launchInPane({
      session: opts.tmuxSession,
      window: opts.tmuxWindow,
      pane: paneIndex,
      host,
      account,
      projectPath: toMacPath(opts.projectPath),
      resumeSid: null,
      yolo: opts.yolo ?? false,
    });
    return paneIndex;
  }

  const wslPath = toWslPath(opts.projectPath);
  const yoloFlag = opts.yolo ? " --dangerously-skip-permissions" : "";
  const envPrefix = account === "bravura"
    ? `CLAUDE_CONFIG_DIR="$HOME/.claude-b" `
    : account === "sully"
      ? `CLAUDE_CONFIG_DIR="$HOME/.claude-c" `
      : "";
  await sendToPane(opts.tmuxSession, opts.tmuxWindow, paneIndex, `cd "${wslPath}" && ${envPrefix}${CLAUDE_CMD}${yoloFlag}`);

  return paneIndex;
}
