import {
  sendToPane,
  sendToPaneOn,
  setPaneAssignment,
  createPane,
  createPaneOn,
  listSessions,
  cancelPaneCommand,
  cancelPaneCommandOn,
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
 *
 * Host-aware: when `opts.host === "local"` (or omitted) it keeps the
 * pre-refactor auto-create behavior — split the local tmux window if no
 * unassigned pane exists. For `host !== "local"` a `targetPaneIndex` is
 * required; we won't implicitly create a Mac pane here because that
 * needs an explicit Mac session choice which the modal flow provides.
 */
async function resolvePaneIndex(opts: {
  tmuxSession: string;
  tmuxWindow: number;
  tmuxPanes: TmuxPane[];
  paneAssignments: Record<string, string>;
  targetPaneIndex?: number;
  host?: string;
}): Promise<number> {
  if (opts.targetPaneIndex != null) return opts.targetPaneIndex;

  const host = opts.host ?? "local";
  if (host !== "local") {
    throw new Error(
      `resolvePaneIndex: auto-create not supported for host=${host} — pass targetPaneIndex or use CreatePaneModal first`,
    );
  }

  // paneAssignments keys are now the full 4-segment coord
  // "host|session|window|pane". Pull out just the pane indices that
  // belong to the local session+window we're operating on.
  const prefix = `local|${opts.tmuxSession}|${opts.tmuxWindow}|`;
  const assignedIndices = new Set<number>();
  for (const k of Object.keys(opts.paneAssignments)) {
    if (k.startsWith(prefix)) {
      const pIdx = Number(k.slice(prefix.length));
      if (!Number.isNaN(pIdx)) assignedIndices.add(pIdx);
    }
  }

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
  /** Optional host override. When not given, derives from the target pane
   *  (a TmuxPane in `tmuxPanes` matching `targetPaneIndex`) or defaults
   *  to "local". Drag-to-local panes never need to pass this. */
  host?: string;
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // If a process is running in this pane, cancel it first so the cd
  // goes to the shell, not to the running program.
  const targetPane = opts.tmuxPanes.find((p) => p.pane_index === paneIndex);
  const host = opts.host ?? targetPane?.host ?? "local";
  const session = targetPane?.session_name ?? opts.tmuxSession;
  const windowIndex = targetPane?.window_index ?? opts.tmuxWindow;
  const cmd = targetPane?.current_command?.toLowerCase() ?? "";
  if (cmd && cmd !== "bash" && cmd !== "zsh" && cmd !== "sh" && cmd !== "-") {
    await cancelPaneCommandOn(host, session, windowIndex, paneIndex);
    // Small delay for the process to exit and shell prompt to return
    await new Promise((r) => setTimeout(r, 500));
  }

  await setPaneAssignment(host, session, windowIndex, paneIndex, opts.encodedProject);

  // `cd` runs on the pane's actual host, so the path must be in that
  // host's filesystem — WSL POSIX for local, `/Users/admin/projects/...`
  // for Mac. The caller passes `projectPath` in WSL form; for Mac slots
  // we translate. Routing: `sendToPaneOn` so the keys land on the right
  // tmux server; plain `sendToPane` would hit local tmux regardless.
  const pathForHost = host === "local"
    ? toWslPath(opts.projectPath)
    : toMacPath(opts.projectPath);
  await sendToPaneOn(host, session, windowIndex, paneIndex, `cd "${pathForHost}"`);

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
  /** `"local"` (default) or an SSH alias like `"mac"`. When the target
   *  pane is known (targetPaneIndex + tmuxPanes match), its `host` field
   *  is the authoritative source; this arg is the fallback for
   *  auto-resolved panes. */
  host?: string;
  /** `"andrea"` (default) | `"bravura"` | `"sully"`. */
  account?: string;
}): Promise<number> {
  const paneIndex = await resolvePaneIndex(opts);

  // Find the actual target pane in the grid so we can route all ops
  // (cancel / assign / send) to its own tmux server, not the ambient
  // `opts.tmuxSession/Window` which is only meaningful for local panes.
  // Disambiguate by host + pane_index since Local main:0.3 and Mac
  // main:0.3 can coexist in a unified grid.
  const requestedHost = opts.host ?? "local";
  const targetPane = opts.tmuxPanes.find(
    (p) => p.pane_index === paneIndex && (p.host ?? "local") === requestedHost,
  ) ?? opts.tmuxPanes.find((p) => p.pane_index === paneIndex);
  const host = (targetPane?.host ?? requestedHost) || "local";
  const session = targetPane?.session_name ?? opts.tmuxSession;
  const windowIndex = targetPane?.window_index ?? opts.tmuxWindow;

  // Always cancel first — Ctrl-C twice on the correct host, then wait.
  await cancelPaneCommandOn(host, session, windowIndex, paneIndex);
  await new Promise((r) => setTimeout(r, 800));

  await setPaneAssignment(host, session, windowIndex, paneIndex, opts.encodedProject);

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

  const account = opts.account ?? "andrea";

  if (host !== "local") {
    // Remote host: Rust-side launch_in_pane assembles the cd-then-mncld
    // string and routes it over SSH to the target host's tmux.
    await launchInPane({
      session,
      window: windowIndex,
      pane: paneIndex,
      host,
      account,
      projectPath: toMacPath(opts.projectPath),
      resumeSid: resumeId ?? null,
      yolo: opts.yolo ?? false,
    });
    return paneIndex;
  }

  // Local path — explicit session/window from the target pane so a
  // caller who hands us a non-currently-selected local pane still
  // routes correctly (e.g. a future grid view that spans multiple
  // local windows).
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

  await sendToPane(session, windowIndex, paneIndex, `cd "${wslPath}" && ${claudeCmd}`);

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
  /** The pane we're forking lives on which host? Derived from the
   *  source TmuxPane when possible; the caller usually has the full
   *  pane object and just passes `sourcePane.host` here. */
  host?: string;
  /** Session name on the pane's host — for local panes this is usually
   *  the selected tmux session; for Mac panes it's the pane's own
   *  session (per-project under the `cc` convention). */
  sourceSession?: string;
  /** Window index on the pane's host. */
  sourceWindow?: number;
  /** Claude account the forked session should resume under. Defaults
   *  to "andrea" to match the local pre-refactor behavior. */
  account?: string;
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

  // Prefer explicit host/session/window from the caller; otherwise look
  // them up from the source pane in the grid. The fallback to opts.tmux*
  // preserves pre-refactor behavior when neither is supplied.
  const sourcePane = opts.tmuxPanes.find(
    (p) => p.pane_index === opts.sourcePaneIndex,
  );
  const host = opts.host ?? sourcePane?.host ?? "local";
  const session = opts.sourceSession ?? sourcePane?.session_name ?? opts.tmuxSession;
  const windowIndex = opts.sourceWindow ?? sourcePane?.window_index ?? opts.tmuxWindow;
  const account = opts.account ?? "andrea";

  // 1. Let Claude fork itself in-place via slash command. Must land on
  //    the pane's actual tmux server — `/branch` over SSH for Mac panes.
  await sendToPaneOn(host, session, windowIndex, opts.sourcePaneIndex, "/branch");

  // 2. Give Claude time to mint Y, flush its last turn into X, and swap
  //    its open file descriptor to Y. After this window, the source pane
  //    writes Y and the forked-from X.jsonl is no longer appended to.
  await new Promise((r) => setTimeout(r, 1500));

  // Record the *immutable* pane ids (%N form) of panes on this host's
  // same session/window before the split. Comparing by pane_id (stable)
  // rather than pane_index (re-numbered on split) reliably identifies
  // the new pane after tmux re-lays out.
  const preExistingPaneIds = new Set(
    opts.tmuxPanes
      .filter(
        (p) =>
          (p.host ?? "local") === host &&
          p.session_name === session &&
          p.window_index === windowIndex,
      )
      .map((p) => p.pane_id),
  );

  // 3. Split on the pane's host. createPaneOn returns all panes for the
  //    host's session+window after the split.
  const newPanes = await createPaneOn(host, session, windowIndex, "h");

  const newPane = newPanes.find((p) => !preExistingPaneIds.has(p.pane_id));
  if (!newPane || newPane.pane_index === opts.sourcePaneIndex) {
    throw new Error("fork: split-window produced no identifiable new pane");
  }

  // 4. Resume the original session in the new pane. For remote hosts we
  //    route via the backend `launchInPane` so the same assembly rules
  //    (mncld, env prefix, yolo) apply as a normal Mac launch. For local
  //    we keep the inline `cd ... && ncld` send to preserve the fast path.
  if (host !== "local") {
    await launchInPane({
      session,
      window: windowIndex,
      pane: newPane.pane_index,
      host,
      account,
      projectPath: toMacPath(opts.projectPath),
      resumeSid: resolvedSid,
      yolo: false,
    });
  } else {
    const wslPath = toWslPath(opts.projectPath);
    const envPrefix = account === "bravura"
      ? `CLAUDE_CONFIG_DIR="$HOME/.claude-b" `
      : account === "sully"
        ? `CLAUDE_CONFIG_DIR="$HOME/.claude-c" `
        : "";
    await sendToPane(
      session,
      windowIndex,
      newPane.pane_index,
      `cd "${wslPath}" && ${envPrefix}${CLAUDE_CMD} -r ${resolvedSid}`,
    );
  }

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

  const requestedHost = opts.host ?? "local";
  const targetPane = opts.tmuxPanes.find(
    (p) => p.pane_index === paneIndex && (p.host ?? "local") === requestedHost,
  ) ?? opts.tmuxPanes.find((p) => p.pane_index === paneIndex);
  const host = (targetPane?.host ?? requestedHost) || "local";
  const session = targetPane?.session_name ?? opts.tmuxSession;
  const windowIndex = targetPane?.window_index ?? opts.tmuxWindow;

  await cancelPaneCommandOn(host, session, windowIndex, paneIndex);
  await new Promise((r) => setTimeout(r, 800));

  await setPaneAssignment(host, session, windowIndex, paneIndex, opts.encodedProject);

  const account = opts.account ?? "andrea";

  if (host !== "local") {
    await launchInPane({
      session,
      window: windowIndex,
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
  await sendToPane(session, windowIndex, paneIndex, `cd "${wslPath}" && ${envPrefix}${CLAUDE_CMD}${yoloFlag}`);

  return paneIndex;
}
