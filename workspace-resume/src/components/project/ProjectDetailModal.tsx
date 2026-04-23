import { createSignal, createResource, For, Show, onMount, onCleanup } from "solid-js";
import { useApp } from "../../contexts/AppContext";
import {
  listSessions,
  setSessionBinding,
  deleteSession,
  openDirectory,
  setProjectTier,
  checkContinuityExists,
  sendToPane,
  getSessionNames,
  setSessionNames as setSessionNamesCmd,
} from "../../lib/tauri-commands";
import { launchToPane, newSessionInPane } from "../../lib/launch";
import { toWslPath, deriveName } from "../../lib/path";
import { relativeTime, formatDuration } from "../../lib/time";
import type { ProjectWithMeta, SessionInfo } from "../../lib/types";

function truncate(s: string | null, max: number): string {
  if (!s) return "";
  return s.length > max ? s.slice(0, max) + "..." : s;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

interface Props {
  project: ProjectWithMeta;
  onClose: () => void;
}

export function ProjectDetailModal(props: Props) {
  const { state, refreshTmuxState, refreshProjects, startPanePick, settingsFromPane } = useApp();

  /** Reactive project — always reads latest from state.projects so bind/tier changes reflect immediately. */
  const project = () =>
    state.projects.find((p) => p.encoded_name === props.project.encoded_name) ?? props.project;

  // Session rename state: map of session_id -> custom name
  const [sessionNames, setSessionNames] = createSignal<Record<string, string>>({});
  const [editingSession, setEditingSession] = createSignal<string | null>(null);
  const [editValue, setEditValue] = createSignal("");
  const [hasContinuity, setHasContinuity] = createSignal<boolean | null>(null);

  // Load session rename data from the Tauri store + check Continuity.
  // Session names used to live in localStorage; moving to the store
  // keeps them alongside the rest of the settings and survives a
  // webview-origin change.
  onMount(async () => {
    try {
      const stored = await getSessionNames(props.project.encoded_name);
      setSessionNames(stored);
    } catch (e) {
      console.error("[ProjectDetailModal] getSessionNames error:", e);
    }
    try {
      const exists = await checkContinuityExists(props.project.actual_path);
      setHasContinuity(exists);
    } catch { /* ignore */ }
  });

  function saveSessionNames(names: Record<string, string>) {
    setSessionNames(names);
    setSessionNamesCmd(props.project.encoded_name, names).catch((e) =>
      console.error("[ProjectDetailModal] setSessionNames error:", e),
    );
  }

  // Load sessions — from all linked Claude Code directories if renamed
  const [sessions, { refetch }] = createResource(
    () => props.project.encoded_name,
    async (enc) => {
      const dirs = project().meta.claude_project_dirs;
      if (dirs && dirs.length > 0) {
        // Umbrella project: load from all linked directories
        const allSessions = await Promise.all(
          [enc, ...dirs.filter((d) => d !== enc)].map((d) => listSessions(d).catch(() => []))
        );
        // Flatten and sort by last_timestamp (newest first)
        return allSessions.flat().sort((a, b) => {
          const ta = a.last_timestamp ?? "";
          const tb = b.last_timestamp ?? "";
          return tb.localeCompare(ta);
        });
      }
      return listSessions(enc);
    },
  );

  // Close on Escape
  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") props.onClose();
  }
  onMount(() => document.addEventListener("keydown", handleKeyDown));
  onCleanup(() => document.removeEventListener("keydown", handleKeyDown));

  // Derived
  const wslPath = () => toWslPath(project().actual_path);
  const displayName = () =>
    project().meta.display_name || deriveName(project().actual_path);

  const mostRecentSession = () => {
    const s = sessions();
    if (!s || s.length === 0) return null;
    return s[0]; // sessions come sorted by most recent
  };

  const lastActivity = () => {
    if (sessions.loading) return "loading...";
    const s = mostRecentSession();
    if (!s) return "no sessions";
    if (!s.last_timestamp) return "no timestamp";
    return relativeTime(s.last_timestamp);
  };

  const totalSize = () => {
    const s = sessions();
    if (!s) return 0;
    return s.reduce((sum, sess) => sum + sess.file_size_bytes, 0);
  };

  // Session display name
  function sessionDisplayName(session: SessionInfo): string {
    const custom = sessionNames()[session.session_id];
    if (custom) return custom;
    return session.session_id.slice(0, 12);
  }

  // Rename
  function startRenameSession(sessionId: string) {
    setEditValue(sessionNames()[sessionId] || "");
    setEditingSession(sessionId);
  }

  function commitRenameSession(sessionId: string) {
    setEditingSession(null);
    const val = editValue().trim();
    const names = { ...sessionNames() };
    if (val) {
      names[sessionId] = val;
    } else {
      delete names[sessionId];
    }
    saveSessionNames(names);
  }

  function handleRenameKeyDown(e: KeyboardEvent, sessionId: string) {
    if (e.key === "Enter") commitRenameSession(sessionId);
    if (e.key === "Escape") setEditingSession(null);
  }

  /**
   * Launch helper: if settings was opened from a pane (gear icon), launch
   * directly in that pane. Otherwise open the pane picker.
   */
  async function launchOrPick(opts: { mode: "resume" | "new"; yolo?: boolean; continuity?: boolean; sessionId?: string }) {
    const p = project();
    if (!p) return;
    const fromPane = settingsFromPane();
    if (fromPane != null) {
      // Direct launch in the originating pane
      const sess = state.selectedTmuxSession;
      const win = state.selectedTmuxWindow;
      if (!sess || win == null) return;
      props.onClose();
      try {
        if (opts.mode === "resume") {
          await launchToPane({
            tmuxSession: sess, tmuxWindow: win, tmuxPanes: state.tmuxPanes,
            paneAssignments: state.paneAssignments, encodedProject: p.encoded_name,
            projectPath: p.actual_path, boundSession: p.meta.bound_session,
            sessionId: opts.sessionId, targetPaneIndex: fromPane, yolo: opts.yolo,
          });
        } else {
          await newSessionInPane({
            tmuxSession: sess, tmuxWindow: win, tmuxPanes: state.tmuxPanes,
            paneAssignments: state.paneAssignments, encodedProject: p.encoded_name,
            projectPath: p.actual_path, targetPaneIndex: fromPane, yolo: opts.yolo,
          });
        }
        if (opts.continuity) {
          await new Promise((r) => setTimeout(r, 7000));
          await sendToPane(sess, win, fromPane, "/continuity");
        }
        refreshTmuxState();
        refreshProjects();
      } catch (e) {
        console.error("[ProjectDetailModal] direct launch error:", e);
      }
    } else {
      startPanePick({ project: p, ...opts });
    }
  }

  function handleLaunchWithContinuity() {
    launchOrPick({ mode: "resume", continuity: true });
  }

  function handleYoloResume() {
    launchOrPick({ mode: "resume", yolo: true });
  }

  function handleYoloNew() {
    launchOrPick({ mode: "new", yolo: true });
  }

  // Delete session
  const [confirmDeleteId, setConfirmDeleteId] = createSignal<string | null>(null);
  const [deleteInput, setDeleteInput] = createSignal("");

  async function handleDeleteSession(sessionId: string) {
    try {
      await deleteSession(project().encoded_name, sessionId);
      setConfirmDeleteId(null);
      refreshProjects();
      // Re-fetch sessions to update the list
      refetch();
    } catch (e) {
      console.error("[ProjectDetailModal] delete session error:", e);
    }
  }

  // Bind / Unbind
  async function handleBind(sessionId: string) {
    const isBound = project().meta.bound_session === sessionId;
    try {
      await setSessionBinding(project().encoded_name, isBound ? null : sessionId);
      refreshProjects();
    } catch (e) {
      console.error("[ProjectDetailModal] bind error:", e);
    }
  }

  // Resume specific session
  function handleResume(sessionId: string) {
    launchOrPick({ mode: "resume", sessionId });
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal-content" onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div class="modal-header">
          <h2 class="modal-title">{displayName()}</h2>
          <div class="modal-header-actions">
            <button class="modal-btn" onClick={() => openDirectory(project().actual_path)}>
              Open Directory
            </button>
            <button class="modal-close" onClick={props.onClose}>{"\u2715"}</button>
          </div>
        </div>

        {/* Project Info */}
        <div class="modal-section">
          <h3 class="modal-section-title">Project Info</h3>
          <div class="modal-info-grid">
            <span class="modal-label">Windows Path</span>
            <span class="modal-value copyable" title={project().actual_path}>
              {project().actual_path}
            </span>

            <span class="modal-label">WSL Path</span>
            <span class="modal-value copyable" title={wslPath()}>
              {wslPath()}
            </span>

            <span class="modal-label">Sessions</span>
            <span class="modal-value">{project().session_count}</span>

            <span class="modal-label">Last Session Activity</span>
            <span class="modal-value">{lastActivity()}</span>

            <span class="modal-label">Total Size</span>
            <span class="modal-value">{formatBytes(totalSize())}</span>

            <span class="modal-label">Tier</span>
            <span class="modal-value">
              <select
                class="modal-tier-select"
                value={project().meta.tier}
                onChange={async (e) => {
                  await setProjectTier(project().encoded_name, e.currentTarget.value);
                  refreshProjects();
                }}
              >
                <option value="pinned">Pinned</option>
                <option value="active">Active</option>
                <option value="paused">Paused</option>
                <option value="archived">Archived</option>
              </select>
            </span>

            <span class="modal-label">Resume Session</span>
            <span class="modal-value mono">
              <Show
                when={project().meta.bound_session}
                fallback={<span style={{ opacity: 0.6 }}>most recent</span>}
              >
                {project().meta.bound_session!.slice(0, 12)} <span style={{ opacity: 0.5 }}>(bound)</span>
              </Show>
            </span>

            <span class="modal-label">Continuity</span>
            <span class="modal-value">
              <Show when={hasContinuity() !== null} fallback="...">
                <span class={hasContinuity() ? "continuity-yes" : "continuity-no"}>
                  {hasContinuity() ? "Set up" : "Not set up"}
                </span>
              </Show>
              <button
                class="modal-btn continuity-launch-btn"
                onClick={handleLaunchWithContinuity}
              >
                Launch with Continuity
              </button>
            </span>

            <span class="modal-label">YOLO Mode</span>
            <span class="modal-value">
              <button
                class="modal-btn yolo-btn"
                onClick={handleYoloResume}
              >
                Resume YOLO
              </button>
              <button
                class="modal-btn yolo-btn"
                onClick={handleYoloNew}
              >
                New YOLO
              </button>
            </span>
          </div>
        </div>

        {/* Session List */}
        <div class="modal-section">
          <h3 class="modal-section-title">
            Sessions
            <Show when={sessions()}>
              <span class="modal-count">{sessions()!.length}</span>
            </Show>
          </h3>

          <Show when={sessions.loading}>
            <div class="modal-loading">Loading sessions...</div>
          </Show>

          <Show when={!sessions.loading && sessions()}>
            <div class="modal-session-list">
              <For each={sessions()}>
                {(session) => (
                  <div
                    class={`modal-session ${project().meta.bound_session === session.session_id ? "bound" : ""} ${session.is_corrupted ? "corrupted" : ""}`}
                  >
                    <div class="modal-session-header">
                      <Show
                        when={editingSession() !== session.session_id}
                        fallback={
                          <input
                            class="modal-session-rename"
                            value={editValue()}
                            onInput={(e) => setEditValue(e.currentTarget.value)}
                            onBlur={() => commitRenameSession(session.session_id)}
                            onKeyDown={(e) => handleRenameKeyDown(e, session.session_id)}
                            placeholder={session.session_id.slice(0, 12)}
                            ref={(el) => setTimeout(() => el.focus(), 0)}
                          />
                        }
                      >
                        <span
                          class="modal-session-name"
                          onDblClick={() => startRenameSession(session.session_id)}
                          title={`Double-click to rename\nFull ID: ${session.session_id}`}
                        >
                          {sessionDisplayName(session)}
                        </span>
                      </Show>

                      <Show when={project().meta.bound_session === session.session_id}>
                        <span class="modal-session-bound-tag">bound</span>
                      </Show>
                      <Show when={session.is_corrupted}>
                        <span class="modal-session-corrupted-tag">corrupted</span>
                      </Show>
                    </div>

                    <div class="modal-session-meta">
                      <span>{relativeTime(session.last_timestamp)}</span>
                      <span>{formatDuration(session.first_timestamp, session.last_timestamp)}</span>
                      <span>{formatBytes(session.file_size_bytes)}</span>
                    </div>

                    <Show when={session.last_user_message}>
                      <div class="modal-session-message">
                        {truncate(session.last_user_message, 120)}
                      </div>
                    </Show>

                    <div class="modal-session-actions">
                      <button
                        class="modal-btn primary"
                        disabled={session.is_corrupted}
                        onClick={() => handleResume(session.session_id)}
                      >
                        Resume
                      </button>
                      <button
                        class={`modal-btn ${project().meta.bound_session === session.session_id ? "active" : ""}`}
                        onClick={() => handleBind(session.session_id)}
                      >
                        {project().meta.bound_session === session.session_id
                          ? "Unbind"
                          : "Bind"}
                      </button>
                      <button
                        class="modal-btn"
                        onClick={() => startRenameSession(session.session_id)}
                      >
                        Rename
                      </button>
                      <button
                        class="modal-btn delete-btn"
                        onClick={() => { setConfirmDeleteId(session.session_id); setDeleteInput(""); }}
                      >
                        Delete
                      </button>
                    </div>

                    {/* Delete confirmation inline — type "delete" to confirm */}
                    <Show when={confirmDeleteId() === session.session_id}>
                      <div class="modal-session-delete-confirm">
                        <span>Type <strong>delete</strong> to permanently remove this session:</span>
                        <div class="delete-confirm-input-row">
                          <input
                            type="text"
                            class="delete-confirm-input"
                            placeholder="type delete"
                            value={deleteInput()}
                            onInput={(e) => setDeleteInput(e.currentTarget.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter" && deleteInput().toLowerCase() === "delete") handleDeleteSession(session.session_id);
                              if (e.key === "Escape") setConfirmDeleteId(null);
                            }}
                            ref={(el) => setTimeout(() => el.focus(), 0)}
                          />
                          <button
                            class="modal-btn danger"
                            disabled={deleteInput().toLowerCase() !== "delete"}
                            onClick={() => handleDeleteSession(session.session_id)}
                          >
                            Confirm
                          </button>
                          <button class="modal-btn" onClick={() => setConfirmDeleteId(null)}>
                            Cancel
                          </button>
                        </div>
                      </div>
                    </Show>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
