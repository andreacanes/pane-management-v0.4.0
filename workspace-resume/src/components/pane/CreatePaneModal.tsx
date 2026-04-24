import { Show, createSignal, createEffect, For, onMount } from "solid-js";
import {
  createPaneOn,
  createSessionOn,
  launchProjectSessionOn,
  listTmuxSessionsOn,
} from "../../lib/tauri-commands";
import { useApp } from "../../contexts/AppContext";
import type { TmuxSession, ProjectWithMeta } from "../../lib/types";

/** Derive the Mac-side project path under the Mutagen `~/projects`
 *  convention. Mirrors `toMacPath` in `launch.ts`; kept inline here to
 *  avoid importing a module that also statically imports
 *  tauri-commands (the circular static/dynamic import warning). */
function macPathFor(wslProjectPath: string): string {
  const parts = wslProjectPath.split(/[\\/]+/).filter((s) => s.length > 0);
  const basename = parts[parts.length - 1] ?? "";
  return basename ? `/Users/admin/projects/${basename}` : "";
}

/**
 * Ask the user where to create a new tmux pane: which host, which
 * session (existing or new), which split direction. Returns via
 * `onCreated` with the full coord the backend just materialized so the
 * caller can refresh the grid and (optionally) pre-drop a project
 * assignment on the new slot.
 *
 * Design notes:
 * - The host list is hard-coded to [local, mac] for MVP because
 *   `list_remote_hosts` currently returns `["mac"]`. If we add a third
 *   host (another Mac, a Linux box) the list lifts into a prop or a
 *   Tauri store read.
 * - Existing-session picker defaults to the first session returned by
 *   `list_tmux_sessions_on(host)`. On Mac the `cc` convention is
 *   per-project sessions, so the caller can pre-select a session by
 *   passing `defaultSession`.
 * - We only support splitting *the active window* of the chosen session
 *   (window_index derived from `list_tmux_sessions`). Multi-window Mac
 *   sessions are rare for the `cc` workflow; add a window picker if
 *   that becomes a real case.
 */
export function CreatePaneModal(props: {
  open: boolean;
  onClose: () => void;
  onCreated: (coord: { host: string; session: string; windowIndex: number; paneIndex: number }) => void;
  /** Session name hint — used as the default value for the "new session"
   *  radio on Mac so the picker opens pre-filled with the relevant
   *  project name when we launched the modal from a drag-drop flow. */
  defaultSession?: string;
}) {
  const { state } = useApp();
  const [host, setHost] = createSignal<string>("local");
  const [mode, setMode] = createSignal<"existing" | "new">("existing");
  const [direction, setDirection] = createSignal<"h" | "v">("h");
  const [existingSessions, setExistingSessions] = createSignal<TmuxSession[]>([]);
  const [selectedSession, setSelectedSession] = createSignal<string>("");
  const [newSessionName, setNewSessionName] = createSignal<string>("");
  /** Optional project to launch in the new remote session. When set on
   *  host=mac+new the modal takes the `cc`-equivalent path:
   *  `launch_project_session_on` starts a detached session whose first
   *  pane runs `mncld` inside the mirrored project dir — matches the
   *  interactive `cc <project> <account>` flow without needing a TTY.
   *  Leave empty for a bare zsh session. */
  const [projectEncoded, setProjectEncoded] = createSignal<string>("");
  const [account, setAccount] = createSignal<string>("andrea");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const sortedProjects = (): ProjectWithMeta[] =>
    [...state.projects].sort((a, b) =>
      (a.meta.display_name ?? a.encoded_name).localeCompare(
        b.meta.display_name ?? b.encoded_name,
      ),
    );

  // When a project is picked, default the session name to its basename
  // — matches the `cc <project>` convention so users get a sensible
  // session name without typing.
  createEffect(() => {
    const enc = projectEncoded();
    if (!enc) return;
    const proj = state.projects.find((p) => p.encoded_name === enc);
    if (!proj) return;
    const parts = proj.actual_path.split(/[\\/]+/).filter((s) => s.length > 0);
    const basename = parts[parts.length - 1] ?? "";
    if (basename) setNewSessionName(basename);
  });

  onMount(() => {
    setNewSessionName(props.defaultSession ?? "scratch");
  });

  // Refetch the session list whenever host changes so the existing
  // picker always shows the right tmux server's sessions.
  createEffect(async () => {
    const h = host();
    setExistingSessions([]);
    setSelectedSession("");
    try {
      const sessions = await listTmuxSessionsOn(h);
      setExistingSessions(sessions);
      if (sessions.length > 0) {
        // Prefer the user's hint when it matches an existing session.
        const preferred =
          (props.defaultSession && sessions.find((s) => s.name === props.defaultSession)) ??
          sessions[0];
        setSelectedSession(preferred.name);
      } else {
        // No existing sessions → force "new" mode so the user isn't
        // staring at an empty radio list.
        setMode("new");
      }
    } catch (e) {
      console.warn("[CreatePaneModal] listTmuxSessionsOn failed:", e);
      setExistingSessions([]);
      setMode("new");
    }
  });

  async function handleCreate() {
    setBusy(true);
    setError(null);
    try {
      const h = host();
      let session = mode() === "existing" ? selectedSession() : newSessionName().trim();
      if (!session) throw new Error("session name is required");

      if (mode() === "new") {
        const enc = projectEncoded();
        const proj = enc
          ? state.projects.find((p) => p.encoded_name === enc)
          : undefined;

        // cc-equivalent path: host != local AND a project is picked.
        // Starts a detached tmux session running mncld inside the Mac
        // project dir under the chosen Claude account. Matches what
        // `~/bin/cc <project> <account>` does on the Mac terminal but
        // stays headless so no TTY is required over SSH.
        if (h !== "local" && proj) {
          const projectPathOnHost = macPathFor(proj.actual_path);
          if (!projectPathOnHost) {
            throw new Error("unable to derive project path for this host");
          }
          try {
            await launchProjectSessionOn(h, session, projectPathOnHost, account());
          } catch (e) {
            const msg = e instanceof Error ? e.message : String(e);
            // `tmux new-session -A -d` is already idempotent, so a
            // duplicate-session error here is rare — but catch it for
            // parity with the bare-session path and treat as "attach to
            // existing".
            if (!/duplicate session/i.test(msg)) throw e;
          }
          props.onCreated({ host: h, session, windowIndex: 0, paneIndex: 0 });
          props.onClose();
          return;
        }

        // Bare new-session path: no project / no account. Starts a
        // plain shell so the user can drag a project onto the slot
        // later and pick host/account at assignment time.
        try {
          await createSessionOn(h, session);
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          if (/duplicate session/i.test(msg)) {
            props.onCreated({ host: h, session, windowIndex: 0, paneIndex: 0 });
            props.onClose();
            return;
          }
          throw e;
        }
        props.onCreated({ host: h, session, windowIndex: 0, paneIndex: 0 });
        props.onClose();
        return;
      }

      // Existing session path: split window 0 (active window) in the
      // requested direction.
      const panes = await createPaneOn(h, session, 0, direction());
      // createPaneOn returns all panes for the host's session+window
      // after the split; the newly-created one is the highest pane_index.
      const newPane = panes.reduce<typeof panes[number] | null>(
        (acc, p) => (!acc || p.pane_index > acc.pane_index ? p : acc),
        null,
      );
      if (!newPane) throw new Error("tmux returned no panes after split");
      props.onCreated({
        host: h,
        session,
        windowIndex: 0,
        paneIndex: newPane.pane_index,
      });
      props.onClose();
    } catch (e) {
      console.error("[CreatePaneModal] create error:", e);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={props.open}>
      <div
        class="modal-backdrop"
        onClick={(e) => {
          e.stopPropagation();
          props.onClose();
        }}
      >
        <div
          class="create-pane-modal"
          onClick={(e) => e.stopPropagation()}
          style={{
            "background": "var(--surface-1, #111113)",
            "border": "1px solid var(--border, #2d2d33)",
            "border-radius": "8px",
            "padding": "16px",
            "width": "360px",
            "max-width": "90vw",
            "color": "var(--text, #d4d4d4)",
            "display": "flex",
            "flex-direction": "column",
            "gap": "12px",
          }}
        >
          <div style={{ "font-size": "14px", "font-weight": "600" }}>Add Pane</div>

          <label style={{ "display": "flex", "gap": "8px", "align-items": "center" }}>
            <span style={{ "min-width": "72px", "font-size": "12px" }}>Host</span>
            <select
              value={host()}
              onChange={(e) => setHost(e.currentTarget.value)}
              style={{
                "flex": "1",
                "background": "var(--surface-2, #1f1f23)",
                "color": "var(--text, #d4d4d4)",
                "border": "1px solid var(--border, #2d2d33)",
                "border-radius": "4px",
                "padding": "4px 6px",
              }}
            >
              <option value="local">WSL (local)</option>
              <option value="mac">Mac</option>
            </select>
          </label>

          <div style={{ "display": "flex", "gap": "8px", "align-items": "flex-start" }}>
            <span style={{ "min-width": "72px", "font-size": "12px", "margin-top": "2px" }}>Session</span>
            <div style={{ "flex": "1", "display": "flex", "flex-direction": "column", "gap": "4px" }}>
              <label style={{ "display": "flex", "gap": "6px", "align-items": "center", "font-size": "12px" }}>
                <input
                  type="radio"
                  name="session-mode"
                  value="existing"
                  checked={mode() === "existing"}
                  onChange={() => setMode("existing")}
                  disabled={existingSessions().length === 0}
                />
                <span>Existing</span>
                <select
                  value={selectedSession()}
                  onChange={(e) => setSelectedSession(e.currentTarget.value)}
                  disabled={mode() !== "existing" || existingSessions().length === 0}
                  style={{
                    "flex": "1",
                    "background": "var(--surface-2, #1f1f23)",
                    "color": "var(--text, #d4d4d4)",
                    "border": "1px solid var(--border, #2d2d33)",
                    "border-radius": "4px",
                    "padding": "2px 4px",
                  }}
                >
                  <For each={existingSessions()}>
                    {(s) => <option value={s.name}>{s.name}</option>}
                  </For>
                  <Show when={existingSessions().length === 0}>
                    <option value="" disabled>(no sessions on this host)</option>
                  </Show>
                </select>
              </label>

              <label style={{ "display": "flex", "gap": "6px", "align-items": "center", "font-size": "12px" }}>
                <input
                  type="radio"
                  name="session-mode"
                  value="new"
                  checked={mode() === "new"}
                  onChange={() => setMode("new")}
                />
                <span>New</span>
                <input
                  type="text"
                  value={newSessionName()}
                  onInput={(e) => setNewSessionName(e.currentTarget.value)}
                  disabled={mode() !== "new"}
                  placeholder="session name"
                  style={{
                    "flex": "1",
                    "background": "var(--surface-2, #1f1f23)",
                    "color": "var(--text, #d4d4d4)",
                    "border": "1px solid var(--border, #2d2d33)",
                    "border-radius": "4px",
                    "padding": "2px 6px",
                  }}
                />
              </label>
            </div>
          </div>

          <Show when={mode() === "existing"}>
            <label style={{ "display": "flex", "gap": "8px", "align-items": "center" }}>
              <span style={{ "min-width": "72px", "font-size": "12px" }}>Split</span>
              <select
                value={direction()}
                onChange={(e) => setDirection(e.currentTarget.value as "h" | "v")}
                style={{
                  "flex": "1",
                  "background": "var(--surface-2, #1f1f23)",
                  "color": "var(--text, #d4d4d4)",
                  "border": "1px solid var(--border, #2d2d33)",
                  "border-radius": "4px",
                  "padding": "4px 6px",
                }}
              >
                <option value="h">Horizontal (side by side)</option>
                <option value="v">Vertical (top / bottom)</option>
              </select>
            </label>
          </Show>

          {/* Project + account — only relevant when creating a new
              session on a remote host. Picking them here runs the
              cc-equivalent flow (mncld in ~/projects/<name> under the
              chosen account's config dir). Leaving Project empty falls
              back to a bare shell the user can drag a project onto later. */}
          <Show when={mode() === "new" && host() !== "local"}>
            <label style={{ "display": "flex", "gap": "8px", "align-items": "center" }}>
              <span style={{ "min-width": "72px", "font-size": "12px" }}>Project</span>
              <select
                value={projectEncoded()}
                onChange={(e) => setProjectEncoded(e.currentTarget.value)}
                title="Pick a project to cd+mncld into, or leave empty for a bare shell"
                style={{
                  "flex": "1",
                  "background": "var(--surface-2, #1f1f23)",
                  "color": "var(--text, #d4d4d4)",
                  "border": "1px solid var(--border, #2d2d33)",
                  "border-radius": "4px",
                  "padding": "4px 6px",
                }}
              >
                <option value="">(none — bare shell)</option>
                <For each={sortedProjects()}>
                  {(p) => (
                    <option value={p.encoded_name}>
                      {p.meta.display_name ??
                        p.actual_path.split(/[\\/]+/).pop() ??
                        p.encoded_name}
                    </option>
                  )}
                </For>
              </select>
            </label>
            <Show when={projectEncoded() !== ""}>
              <label style={{ "display": "flex", "gap": "8px", "align-items": "center" }}>
                <span style={{ "min-width": "72px", "font-size": "12px" }}>Account</span>
                <select
                  value={account()}
                  onChange={(e) => setAccount(e.currentTarget.value)}
                  title="Claude account mncld runs under in the new session"
                  style={{
                    "flex": "1",
                    "background": "var(--surface-2, #1f1f23)",
                    "color": "var(--text, #d4d4d4)",
                    "border": "1px solid var(--border, #2d2d33)",
                    "border-radius": "4px",
                    "padding": "4px 6px",
                  }}
                >
                  <option value="andrea">Andrea</option>
                  <option value="bravura">Bravura</option>
                  <option value="sully">Sully</option>
                </select>
              </label>
            </Show>
          </Show>

          <Show when={error()}>
            <div
              style={{
                "color": "#fca5a5",
                "background": "rgba(239, 68, 68, 0.1)",
                "border": "1px solid rgba(239, 68, 68, 0.35)",
                "border-radius": "4px",
                "padding": "6px 8px",
                "font-size": "11px",
              }}
            >
              {error()}
            </div>
          </Show>

          <div style={{ "display": "flex", "gap": "8px", "justify-content": "flex-end", "margin-top": "4px" }}>
            <button
              class="modal-btn"
              onClick={props.onClose}
              disabled={busy()}
            >
              Cancel
            </button>
            <button
              class="modal-btn primary"
              onClick={handleCreate}
              disabled={busy()}
            >
              {busy() ? "Creating…" : "Create"}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
