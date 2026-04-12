import { createResource, createSignal, Show } from "solid-js";
import { createDraggable } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import {
  setDisplayName,
  setProjectTier,
  getInode,
  updateProjectInode,
  getProjectUsage,
  createWorktree,
} from "../../lib/tauri-commands";
import { open } from "@tauri-apps/plugin-dialog";
import { launchToPane, newSessionInPane } from "../../lib/launch";
import type { ProjectWithMeta, ProjectTier } from "../../lib/types";
import { GitBranch, Link, Plus } from "../ui/icons";

/** Format a token count into a short human-readable string. */
function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** Format a USD amount. */
function fmtUSD(n: number): string {
  if (n >= 100) return `$${n.toFixed(0)}`;
  if (n >= 10) return `$${n.toFixed(1)}`;
  return `$${n.toFixed(2)}`;
}

/**
 * Derive a display name from the project's actual path.
 * Shows the last path segment (the folder name).
 */
function deriveName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

export function ProjectCard(props: { project: ProjectWithMeta }) {
  const { state, refreshProjects, refreshTmuxState, isProjectActive, openProjectSettings, startPanePick } = useApp();

  // Draggable for Plan 04 drop zones
  const draggable = createDraggable(props.project.encoded_name);

  // Local UI state
  const [editing, setEditing] = createSignal(false);
  const [editValue, setEditValue] = createSignal("");
  const [launching, setLaunching] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const displayName = () =>
    props.project.meta.display_name || deriveName(props.project.actual_path);

  const isActive = () => isProjectActive(props.project.encoded_name);

  // Lazy usage fetch — one Rust call per card on mount, cached by SolidJS.
  const [usage] = createResource(
    () => props.project.encoded_name,
    async (encoded) => {
      try {
        return await getProjectUsage(encoded);
      } catch {
        return null;
      }
    },
  );

  // -- Inline rename -------------------------------------------------------

  function startRename() {
    setEditValue(displayName());
    setEditing(true);
  }

  async function commitRename() {
    const newName = editValue().trim();
    setEditing(false);
    if (!newName || newName === displayName()) return;
    try {
      await setDisplayName(props.project.encoded_name, newName);
      refreshProjects();
    } catch (e) {
      console.error("[ProjectCard] rename error:", e);
    }
  }

  function cancelRename() {
    setEditing(false);
  }

  function handleNameKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter") commitRename();
    if (e.key === "Escape") cancelRename();
  }

  // -- Tier change ---------------------------------------------------------

  async function handleTierChange(e: Event) {
    const newTier = (e.target as HTMLSelectElement).value as ProjectTier;
    try {
      await setProjectTier(props.project.encoded_name, newTier);
      refreshProjects();
    } catch (err) {
      console.error("[ProjectCard] tier change error:", err);
    }
  }

  // -- Resume / New Session -------------------------------------------------
  // If the project is already assigned to a pane, launch directly there.
  // Otherwise, open the pane picker so the user can choose.

  /** Find if this project is already assigned to a pane in the current window. */
  function findAssignedPane(): number | undefined {
    for (const [idx, proj] of Object.entries(state.paneAssignments)) {
      if (proj === props.project.encoded_name) return Number(idx);
    }
    return undefined;
  }

  async function handleResume() {
    setError(null);
    const tmuxSession = state.selectedTmuxSession;
    const tmuxWindow = state.selectedTmuxWindow;
    if (!tmuxSession || tmuxWindow == null) {
      setError("Select a tmux session first");
      return;
    }

    const assignedPane = findAssignedPane();
    if (assignedPane != null) {
      // Already has a pane — launch directly
      setLaunching(true);
      try {
        await launchToPane({
          tmuxSession,
          tmuxWindow,
          tmuxPanes: state.tmuxPanes,
          paneAssignments: state.paneAssignments,
          encodedProject: props.project.encoded_name,
          projectPath: props.project.actual_path,
          boundSession: props.project.meta.bound_session,
          targetPaneIndex: assignedPane,
        });
        refreshTmuxState();
        refreshProjects();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLaunching(false);
      }
    } else {
      // No pane — open picker
      startPanePick({ project: props.project, mode: "resume" });
    }
  }

  async function handleNewWorktree() {
    setError(null);
    const slug = window.prompt(
      "New worktree slug (will create a branch with this name):",
    );
    if (!slug || !slug.trim()) return;
    setLaunching(true);
    try {
      await createWorktree(props.project.actual_path, slug.trim());
      refreshProjects();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLaunching(false);
    }
  }

  async function handleNewSession() {
    setError(null);
    const tmuxSession = state.selectedTmuxSession;
    const tmuxWindow = state.selectedTmuxWindow;
    if (!tmuxSession || tmuxWindow == null) {
      setError("Select a tmux session first");
      return;
    }

    const assignedPane = findAssignedPane();
    if (assignedPane != null) {
      setLaunching(true);
      try {
        await newSessionInPane({
          tmuxSession,
          tmuxWindow,
          tmuxPanes: state.tmuxPanes,
          paneAssignments: state.paneAssignments,
          encodedProject: props.project.encoded_name,
          projectPath: props.project.actual_path,
          targetPaneIndex: assignedPane,
        });
        refreshTmuxState();
        refreshProjects();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLaunching(false);
      }
    } else {
      startPanePick({ project: props.project, mode: "new" });
    }
  }

  return (
    <div
      ref={(el) => draggable(el)}
      class={`project-card ${draggable.isActiveDraggable ? "dragging" : ""}`}
    >
      {/* Header: name + active dot */}
      <div class="project-card-header">
        <Show when={isActive()}>
          <span class="active-dot" title="Active" />
        </Show>

        <Show
          when={!editing()}
          fallback={
            <div class="project-card-name editing">
              <input
                value={editValue()}
                onInput={(e) => setEditValue(e.currentTarget.value)}
                onBlur={commitRename}
                onKeyDown={handleNameKeyDown}
                ref={(el) => setTimeout(() => el.focus(), 0)}
              />
            </div>
          }
        >
          <span
            class="project-card-name"
            onDblClick={startRename}
            title={props.project.actual_path}
          >
            {displayName()}
          </span>
        </Show>
      </div>

      {/* Branch + worktree badge */}
      <Show when={props.project.git_branch}>
        <div class="project-card-git" style={{ "font-size": "0.72rem", "opacity": 0.75, "margin-top": "2px", "display": "inline-flex", "align-items": "center", "gap": "4px" }}>
          <GitBranch size={11} />
          <span title="Git branch">{props.project.git_branch}</span>
          <Show when={props.project.is_linked_worktree}>
            <Link size={11} aria-label="Linked worktree" />
          </Show>
          <Show when={(props.project.worktree_count ?? 1) > 1}>
            <span title="Worktree count">({props.project.worktree_count})</span>
          </Show>
        </div>
      </Show>

      {/* Usage overlay */}
      <Show when={usage() && usage()!.total_messages > 0}>
        <div class="project-card-usage" style={{ "font-size": "0.72rem", "opacity": 0.85, "margin-top": "2px" }}>
          <span title="Total cost">{fmtUSD(usage()!.total_cost_usd)}</span>
          <span style={{ "margin-left": "6px" }} title="Total tokens (in+out+cache)">
            · {fmtTokens(usage()!.total_input + usage()!.total_output + usage()!.total_cache_write + usage()!.total_cache_read)}
          </span>
          <span style={{ "margin-left": "6px" }} title="Message count">
            · {usage()!.total_messages} msgs
          </span>
        </div>
      </Show>

      {/* Meta line */}
      <div class="project-card-meta">
        <span>{props.project.session_count} sessions</span>
        <select
          class="tier-select"
          value={props.project.meta.tier}
          onChange={handleTierChange}
          onClick={(e) => e.stopPropagation()}
        >
          <option value="pinned">Pinned</option>
          <option value="active">Active</option>
          <option value="paused">Paused</option>
          <option value="archived">Archived</option>
        </select>
      </div>

      {/* Error display */}
      <Show when={error()}>
        <div class="error" style={{ "font-size": "0.72rem", margin: "2px 0" }}>
          {error()}
        </div>
      </Show>

      {/* Unlinked state — only show if we have a stored inode AND the inode scan failed
           (meaning we actually confirmed the path is gone, not just that Windows can't check WSL paths) */}
      {/* Unlinked: only when reconciliation explicitly confirmed the path is gone
           (claude_project_dirs is [] empty array, not null/undefined) */}
      <Show when={Array.isArray(props.project.meta.claude_project_dirs) && props.project.meta.claude_project_dirs.length === 0}>
        <div class="project-card-unlinked">
          <span class="unlinked-badge" title="The project directory has been moved or renamed. Use Relink to reconnect to the new location.">Unlinked</span>
          <button
            class="relink-btn"
            onClick={async () => {
              const selected = await open({ directory: true, title: "Relink project — select the new directory" });
              if (selected && typeof selected === "string") {
                const inode = await getInode(selected);
                const existingDirs = props.project.meta.claude_project_dirs ?? [];
                if (!existingDirs.includes(props.project.encoded_name)) {
                  existingDirs.push(props.project.encoded_name);
                }
                await updateProjectInode(props.project.encoded_name, inode, existingDirs);
                refreshProjects();
              }
            }}
          >
            Relink
          </button>
        </div>
      </Show>

      {/* Actions — always show */}
      <div class="project-card-actions">
        <button
          class={`resume-btn ${launching() ? "loading" : ""}`}
          disabled={launching()}
          onClick={handleResume}
        >
          {launching() ? "..." : "Resume"}
        </button>
        <button
          class="new-session-btn"
          disabled={launching()}
          onClick={handleNewSession}
          title="Start fresh Claude session"
        >
          <Plus size={12} />
        </button>
        <Show when={props.project.git_branch && !props.project.is_linked_worktree}>
          <button
            class="new-worktree-btn"
            disabled={launching()}
            onClick={handleNewWorktree}
            title="Create a new linked worktree + branch"
            style={{ display: "inline-flex", "align-items": "center", gap: "2px" }}
          >
            <GitBranch size={11} />
            <Plus size={10} />
          </button>
        </Show>
        <button class="project-settings-btn" onClick={() => openProjectSettings(props.project)}>
          Settings
        </button>
      </div>
    </div>
  );
}
