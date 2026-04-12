import {
  sendToPane,
  setPaneAssignment,
  createPane,
  listSessions,
  cancelPaneCommand,
} from "./tauri-commands";
import type { TmuxPane } from "./types";
import { toWslPath } from "./path";

// Re-export toWslPath so existing imports from launch.ts still work.
export { toWslPath } from "./path";

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
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // Always cancel first — Ctrl-C twice, then wait for shell prompt
  await cancelPaneCommand(opts.tmuxSession, opts.tmuxWindow, paneIndex);
  await new Promise((r) => setTimeout(r, 800));

  await setPaneAssignment(opts.tmuxSession, opts.tmuxWindow, paneIndex, opts.encodedProject);

  const wslPath = toWslPath(opts.projectPath);
  const yoloFlag = opts.yolo ? " --dangerously-skip-permissions" : "";

  // Determine which session ID to use: explicit > bound > most-recent
  let resumeId = opts.sessionId || opts.boundSession;

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

  const claudeCmd = resumeId
    ? `claude -r ${resumeId}${yoloFlag}`
    : `claude -r${yoloFlag}`;

  // Chain cd + claude as a single command so claude only starts after cd completes
  await sendToPane(opts.tmuxSession, opts.tmuxWindow, paneIndex, `cd "${wslPath}" && ${claudeCmd}`);

  return paneIndex;
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
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // Always cancel first — Ctrl-C twice, then wait for shell prompt
  await cancelPaneCommand(opts.tmuxSession, opts.tmuxWindow, paneIndex);
  await new Promise((r) => setTimeout(r, 800));

  await setPaneAssignment(opts.tmuxSession, opts.tmuxWindow, paneIndex, opts.encodedProject);

  const wslPath = toWslPath(opts.projectPath);
  const yoloFlag = opts.yolo ? " --dangerously-skip-permissions" : "";
  await sendToPane(opts.tmuxSession, opts.tmuxWindow, paneIndex, `cd "${wslPath}" && claude${yoloFlag}`);

  return paneIndex;
}
