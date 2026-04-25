import { Show, createSignal, createEffect, For, onMount } from "solid-js";
import {
  createPaneOn,
  createSessionOn,
  listTmuxSessionsOn,
  checkRemotePathExists,
  syncProjectToMac,
  launchProjectSessionOn,
  attachRemoteSession,
} from "../../lib/tauri-commands";
import { useApp } from "../../contexts/AppContext";
import type { TmuxSession, ProjectWithMeta } from "../../lib/types";

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

        // Remote + project picked: split the user's CURRENT LOCAL tmux
        // window and run `ssh -t <host> cc <project> <account>` in the
        // new pane. The new pane is a LOCAL tmux pane — visible in the
        // user's WezTerm-attached main session — that happens to be
        // running an SSH session into the Mac. Inside, `cc` does
        // `tmux new-session -A` on the Mac side, so the Mac tmux
        // session is created/attached transparently. Exactly what
        // typing `ssh -t mac cc <proj> <acct>` manually does.
        if (h !== "local" && proj) {
          // Canonical Mac launch path — same primitives the phone
          // (companion `/api/v1/launch-host-session`) uses. Three
          // steps, all delegating to Mac-side cc as the single source
          // of truth:
          //
          //   1. Pre-flight: ensure the project directory is mirrored.
          //      `sync-add-project` is idempotent, so re-running is a
          //      fast no-op. Without this, cc fails with "no such
          //      project" inside ssh and the user is left at a bash
          //      prompt (the original CreatePaneModal local-split +
          //      send-keys bug).
          //   2. `launchProjectSessionOn` → ssh's into Mac and runs
          //      `cc <project> <account>` in headless mode. cc handles
          //      session-name suffixing (`-b` / `-c`), env prefix,
          //      account-mismatch detection, and prints the effective
          //      session name on stdout.
          //   3. `attachRemoteSession` creates a dedicated local
          //      `<host>/<session>` tmux window running
          //      `ssh -t mac tmux attach-session -t <session>`. That
          //      window is recognized by Fix 1a's mirror detection,
          //      so the Mac pane card lands in its grid and the user
          //      sees the live terminal in WezTerm by switching to
          //      that window.
          //
          // The previous local-split + send-keys-`ssh -t mac cc …`
          // approach was deleted: it created a stray pane in the
          // user's currently-selected window, didn't form a mirror
          // (window name was wrong; start_command was bash from the
          // resurrect snapshot), and confused the grid filter into
          // mis-labeling the pane via cwd-match. Going through the
          // canonical path means desktop launches behave identically
          // to phone launches — one mental model.
          const projBasename =
            proj.actual_path.split(/[\\/]+/).filter((s) => s.length > 0).pop() ??
            proj.encoded_name;
          const macPath = `/Users/admin/projects/${projBasename}`;
          const mirrored = await checkRemotePathExists(h, macPath);
          if (!mirrored) {
            setError(`Syncing ${projBasename} to ${h}…`);
            try {
              await syncProjectToMac(proj.encoded_name);
              setError(null);
            } catch (syncErr) {
              throw new Error(
                `sync to ${h} failed — cannot launch without a synced project: ${
                  syncErr instanceof Error ? syncErr.message : String(syncErr)
                }`,
              );
            }
          }
          const [effectiveSession] = await launchProjectSessionOn(
            h,
            projBasename,
            macPath,
            account(),
          );
          // Auto-attach. Failure is non-fatal: the Mac session still
          // exists, the user can click the unmirrored caret chip to
          // retry. Surfaced as a warning rather than blocking.
          try {
            await attachRemoteSession(h, effectiveSession);
          } catch (attachErr) {
            console.warn(
              "[CreatePaneModal] attachRemoteSession failed (Mac session still exists):",
              attachErr,
            );
          }
          props.onCreated({
            host: h,
            session: effectiveSession,
            windowIndex: 0,
            paneIndex: 0,
          });
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
        <div class="create-pane-modal" onClick={(e) => e.stopPropagation()}>
          <div class="create-pane-modal__title">Add Pane</div>

          <label class="create-pane-modal__row">
            <span class="create-pane-modal__label">Host</span>
            <select
              class="create-pane-modal__select"
              value={host()}
              onChange={(e) => setHost(e.currentTarget.value)}
            >
              <option value="local">WSL (local)</option>
              <option value="mac">Mac</option>
            </select>
          </label>

          <div class="create-pane-modal__row create-pane-modal__row--stacked">
            <span class="create-pane-modal__label create-pane-modal__label--stacked">Session</span>
            <div class="create-pane-modal__radio-row">
              <label class="create-pane-modal__radio-option">
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
                >
                  <For each={existingSessions()}>
                    {(s) => <option value={s.name}>{s.name}</option>}
                  </For>
                  <Show when={existingSessions().length === 0}>
                    <option value="" disabled>(no sessions on this host)</option>
                  </Show>
                </select>
              </label>

              <label class="create-pane-modal__radio-option">
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
                />
              </label>
            </div>
          </div>

          <Show when={mode() === "existing"}>
            <label class="create-pane-modal__row">
              <span class="create-pane-modal__label">Split</span>
              <select
                class="create-pane-modal__select"
                value={direction()}
                onChange={(e) => setDirection(e.currentTarget.value as "h" | "v")}
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
            <label class="create-pane-modal__row">
              <span class="create-pane-modal__label">Project</span>
              <select
                class="create-pane-modal__select"
                value={projectEncoded()}
                onChange={(e) => setProjectEncoded(e.currentTarget.value)}
                title="Pick a project to cd+mncld into, or leave empty for a bare shell"
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
              <label class="create-pane-modal__row">
                <span class="create-pane-modal__label">Account</span>
                <select
                  class="create-pane-modal__select"
                  value={account()}
                  onChange={(e) => setAccount(e.currentTarget.value)}
                  title="Claude account mncld runs under in the new session"
                >
                  <option value="andrea">Andrea</option>
                  <option value="bravura">Bravura</option>
                  <option value="sully">Sully</option>
                </select>
              </label>
            </Show>
          </Show>

          <Show when={error()}>
            <div class="create-pane-modal__error">{error()}</div>
          </Show>

          <div class="create-pane-modal__actions">
            <button class="modal-btn" onClick={props.onClose} disabled={busy()}>
              Cancel
            </button>
            <button class="modal-btn primary" onClick={handleCreate} disabled={busy()}>
              {busy() ? "Creating…" : "Create"}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
