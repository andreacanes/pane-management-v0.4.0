import { createSignal, For, Show } from "solid-js";
import { useApp } from "../../contexts/AppContext";
import {
  setupPaneGrid,
  reflowPaneGrid,
  reducePaneGrid,
  listKillTargets,
  setPaneAssignment,
  tmuxResurrectRestore,
  listTmuxPanes,
} from "../../lib/tauri-commands";
import type { TmuxPane } from "../../lib/types";
import { launchToPane } from "../../lib/launch";

/** Fixed layout presets with dot-grid icons. */
const FIXED_LAYOUTS = [
  { label: "1", cols: 1, rows: 1, dots: [[1]] },
  { label: "2", cols: 2, rows: 1, dots: [[1, 1]] },
  { label: "3", cols: 3, rows: 1, dots: [[1, 1, 1]] },
  { label: "4", cols: 2, rows: 2, dots: [[1, 1], [1, 1]] },
  { label: "6", cols: 3, rows: 2, dots: [[1, 1, 1], [1, 1, 1]] },
] as const;

function DotGrid(props: { dots: readonly (readonly number[])[] }) {
  return (
    <span class="layout-dot-grid">
      {props.dots.map((row) => (
        <span class="layout-dot-row">
          {row.map(() => <span class="layout-dot" />)}
        </span>
      ))}
    </span>
  );
}

export function PanePresetPicker() {
  const { state, refreshTmuxState } = useApp();

  const [busy, setBusy] = createSignal(false);
  const [showSnapshotModal, setShowSnapshotModal] = createSignal(false);
  const [reduceTarget, setReduceTarget] = createSignal<{
    cols: number;
    rows: number;
    victims: TmuxPane[];
  } | null>(null);
  const [showResetModal, setShowResetModal] = createSignal(false);

  const session = () => state.selectedTmuxSession;
  const window = () => state.selectedTmuxWindow;
  const currentPaneCount = () => state.tmuxPanes.length;

  /** Look up the encoded-project name assigned to a pane index in the current window. */
  function projectForIndex(idx: number): string | null {
    return state.paneAssignments[String(idx)] ?? null;
  }

  /** Non-destructive path: same count or growth. Reflows or appends without killing. */
  async function reflow(cols: number, rows: number) {
    const sess = session();
    const win = window();
    if (sess == null || win == null) return;
    setBusy(true);
    try {
      await reflowPaneGrid(sess, win, cols, rows);
      refreshTmuxState();
    } catch (e) {
      console.error("[PanePresetPicker] reflow error:", e);
    } finally {
      setBusy(false);
    }
  }

  /** Destructive reduce: kills only indices >= newCount, reflows survivors. */
  async function reduce(cols: number, rows: number) {
    const sess = session();
    const win = window();
    if (sess == null || win == null) return;
    setBusy(true);
    try {
      const newCount = cols * rows;
      await reducePaneGrid(sess, win, cols, rows);
      const staleKeys = Object.keys(state.paneAssignments).filter(
        (idx) => Number(idx) >= newCount,
      );
      if (staleKeys.length > 0) {
        await Promise.all(
          staleKeys.map((idx) => setPaneAssignment(sess, win, Number(idx), null)),
        );
      }
      refreshTmuxState();
    } catch (e) {
      console.error("[PanePresetPicker] reduce error:", e);
    } finally {
      setBusy(false);
    }
  }

  /** Full destructive reset: kills every pane but one, wipes every assignment for (sess, win). */
  async function resetGrid() {
    const sess = session();
    const win = window();
    if (sess == null || win == null) return;
    setBusy(true);
    try {
      await setupPaneGrid(sess, win, 1, 1);
      const allKeys = Object.keys(state.paneAssignments);
      if (allKeys.length > 0) {
        await Promise.all(
          allKeys.map((idx) => setPaneAssignment(sess, win, Number(idx), null)),
        );
      }
      refreshTmuxState();
    } catch (e) {
      console.error("[PanePresetPicker] reset error:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleLayoutClick(cols: number, rows: number) {
    const sess = session();
    const win = window();
    if (sess == null || win == null) return;
    const newCount = cols * rows;
    const current = currentPaneCount();
    if (newCount >= current) {
      await reflow(cols, rows);
      return;
    }
    // Reduce path — fetch exact kill targets and open the confirmation modal.
    try {
      const victims = await listKillTargets(sess, win, newCount);
      setReduceTarget({ cols, rows, victims });
    } catch (e) {
      console.error("[PanePresetPicker] listKillTargets error:", e);
    }
  }

  return (
    <div class="preset-picker">
      {/* Fixed layout buttons */}
      <div class="preset-picker-row">
        <span class="preset-picker-label">Arrange:</span>
        <For each={FIXED_LAYOUTS}>
          {(item) => (
            <button
              class={`preset-btn ${currentPaneCount() === item.cols * item.rows ? "preset-active" : ""}`}
              disabled={busy()}
              onClick={() => handleLayoutClick(item.cols, item.rows)}
              title={`${item.cols * item.rows} pane${item.cols * item.rows > 1 ? "s" : ""} (${item.cols}\u00d7${item.rows})`}
            >
              <DotGrid dots={item.dots} /> {item.label}
            </button>
          )}
        </For>
      </div>

      {/* Resurrect + snapshot + reset */}
      <div class="preset-picker-row">
        <button
          class="preset-btn resurrect"
          disabled={busy()}
          onClick={async () => {
            setBusy(true);
            try {
              await tmuxResurrectRestore();
              refreshTmuxState();

              // Wait for tmux to finish restoring panes
              await new Promise((r) => setTimeout(r, 2000));

              // Re-launch Claude sessions across ALL sessions+windows from saved assignments
              // Keys are "session|window|pane" format — get all from the raw store
              const { getPaneAssignmentsRaw } = await import("../../lib/tauri-commands");
              const allAssignments = await getPaneAssignmentsRaw();

              // Group by session+window
              const groups = new Map<string, { sess: string; win: number; panes: { idx: number; project: string }[] }>();
              for (const [key, encodedProject] of Object.entries(allAssignments)) {
                const parts = key.split("|");
                if (parts.length !== 3) continue;
                const [sessName, winStr, paneStr] = parts;
                const groupKey = `${sessName}|${winStr}`;
                if (!groups.has(groupKey)) {
                  groups.set(groupKey, { sess: sessName, win: Number(winStr), panes: [] });
                }
                groups.get(groupKey)!.panes.push({ idx: Number(paneStr), project: encodedProject });
              }

              // Launch in each session+window
              for (const group of groups.values()) {
                let panes;
                try {
                  panes = await listTmuxPanes(group.sess, group.win);
                } catch (e) {
                  console.warn(`[Resurrect] window ${group.sess}:${group.win} not found, skipping`);
                  continue;
                }
                for (const { idx: paneIndex, project: encodedProject } of group.panes) {
                  const project = state.projects.find((p) => p.encoded_name === encodedProject);
                  if (!project) continue;
                  const paneExists = panes.some((p) => p.pane_index === paneIndex);
                  if (!paneExists) continue;
                  try {
                    await launchToPane({
                      tmuxSession: group.sess,
                      tmuxWindow: group.win,
                      tmuxPanes: panes,
                      paneAssignments: state.paneAssignments,
                      encodedProject,
                      projectPath: project.actual_path,
                      boundSession: project.meta.bound_session,
                      targetPaneIndex: paneIndex,
                    });
                  } catch (e) {
                    console.error(`[Resurrect] failed to resume pane ${paneIndex} in ${group.sess}:${group.win}:`, e);
                  }
                }
              }
              refreshTmuxState();
            } catch (e) {
              console.error("[PanePresetPicker] resurrect restore error:", e);
            } finally {
              setBusy(false);
            }
          }}
          title="Restore last saved tmux state"
        >
          Resurrect
        </button>

        <span class="preset-picker-divider" />

        <button
          class="preset-btn"
          onClick={() => setShowSnapshotModal(true)}
          title="Save a named workspace snapshot"
        >
          Snapshot
        </button>

        <span class="preset-picker-divider" />

        <button
          class="preset-btn danger-outline"
          disabled={busy()}
          onClick={() => setShowResetModal(true)}
          title="Kill every pane in this window and return to a single empty shell"
        >
          Reset grid
        </button>
      </div>

      {/* Confirm reduce panes modal — lists exactly which panes will die */}
      <Show when={reduceTarget()}>
        {(target) => {
          const t = target();
          return (
            <div class="modal-backdrop" onClick={() => setReduceTarget(null)}>
              <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
                <p class="confirm-message">
                  <strong>
                    Kill {t.victims.length} pane{t.victims.length === 1 ? "" : "s"} to shrink to {t.cols}×{t.rows}?
                  </strong>
                </p>
                <p class="confirm-warning">
                  Surviving panes (indices 0–{t.cols * t.rows - 1}) keep their running processes. Only the panes listed below will be killed.
                </p>
                <ul class="kill-target-list">
                  <For each={t.victims}>
                    {(v) => {
                      const proj = projectForIndex(v.pane_index);
                      return (
                        <li class="kill-target-row">
                          <span class="kill-target-id">{v.pane_id}</span>
                          <span class="kill-target-idx">#{v.pane_index}</span>
                          <span class="kill-target-cmd">{v.current_command}</span>
                          <span class="kill-target-path">{v.current_path}</span>
                          <Show when={proj}>
                            <span class="kill-target-project" title="Stored project assignment — may be stale">{proj} (assigned)</span>
                          </Show>
                        </li>
                      );
                    }}
                  </For>
                </ul>
                <div class="confirm-actions">
                  <button class="modal-btn" onClick={() => setReduceTarget(null)}>
                    Cancel
                  </button>
                  <button
                    class="modal-btn danger"
                    onClick={() => {
                      setReduceTarget(null);
                      reduce(t.cols, t.rows);
                    }}
                  >
                    Kill {t.victims.length} pane{t.victims.length === 1 ? "" : "s"}
                  </button>
                </div>
              </div>
            </div>
          );
        }}
      </Show>

      {/* Reset grid confirmation — wipes everything in this window */}
      <Show when={showResetModal()}>
        <div class="modal-backdrop" onClick={() => setShowResetModal(false)}>
          <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
            <p class="confirm-message">
              <strong>Reset this window to a single pane?</strong>
            </p>
            <p class="confirm-warning">
              Every pane in this window will be killed and all project assignments wiped. Use this when the layout is stuck or panes have gone missing.
            </p>
            <div class="confirm-actions">
              <button class="modal-btn" onClick={() => setShowResetModal(false)}>
                Cancel
              </button>
              <button
                class="modal-btn danger"
                onClick={() => {
                  setShowResetModal(false);
                  resetGrid();
                }}
              >
                Reset grid
              </button>
            </div>
          </div>
        </div>
      </Show>

      {/* Snapshot coming-soon modal */}
      <Show when={showSnapshotModal()}>
        <div class="modal-backdrop" onClick={() => setShowSnapshotModal(false)}>
          <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
            <p class="confirm-message">
              <strong>Snapshot</strong> is under development.
            </p>
            <p class="confirm-warning">
              This feature will save named workspace snapshots including tmux layout and project assignments. Check the backlog for more details.
            </p>
            <div class="confirm-actions">
              <button class="modal-btn" onClick={() => setShowSnapshotModal(false)}>
                OK
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
