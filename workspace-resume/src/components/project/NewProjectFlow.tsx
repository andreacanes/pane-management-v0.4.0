import { createSignal, Show, onMount, onCleanup } from "solid-js";
import { createDraggable } from "@thisbeyond/solid-dnd";
import { open } from "@tauri-apps/plugin-dialog";
import { useApp } from "../../contexts/AppContext";
import { createProjectFolder } from "../../lib/tauri-commands";
import { toWslPath } from "../../lib/path";

export const NEW_PROJECT_PREFIX = "new-project:";
export const NEW_PROJECT_CONTINUITY_PREFIX = "new-project-continuity:";

/** Extract just the last folder name from a Windows or Unix path. */
function folderName(path: string): string {
  const clean = path.replace(/^["']+|["']+$/g, "").replace(/[\\/]+$/, "");
  const parts = clean.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] || clean;
}

/** Strip quotes and trailing slashes. */
function cleanPath(p: string): string {
  return p.replace(/^["']+|["']+$/g, "").replace(/[\\/]+$/, "");
}

interface NewProjectFlowProps {
  onCancel: () => void;
}

export function NewProjectFlow(props: NewProjectFlowProps) {
  const { state } = useApp();
  const [pathInput, setPathInput] = createSignal("");
  const [selectedPath, setSelectedPath] = createSignal<string | null>(null);
  const [duplicateMsg, setDuplicateMsg] = createSignal<string | null>(null);
  const [withContinuity, setWithContinuity] = createSignal(false);

  // Create-folder sub-panel state
  const [showCreate, setShowCreate] = createSignal(false);
  const [createParent, setCreateParent] = createSignal("");
  const [createName, setCreateName] = createSignal("");
  const [createError, setCreateError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);

  // Listen for successful drop from App.tsx
  onMount(() => {
    const handler = () => props.onCancel();
    document.addEventListener("new-project-dropped", handler);
    onCleanup(() => document.removeEventListener("new-project-dropped", handler));
  });

  /** Check if the path matches an existing project (handles Windows ↔ WSL format). */
  function isDuplicate(path: string): boolean {
    // Normalize input to WSL format for comparison
    const asWsl = toWslPath(path).toLowerCase().replace(/\/+$/, "");
    // Also normalize as forward-slash Windows for comparison
    const asForward = path.replace(/\\/g, "/").toLowerCase().replace(/\/+$/, "");
    return state.projects.some((p) => {
      const existing = p.actual_path.replace(/\\/g, "/").toLowerCase().replace(/\/+$/, "");
      return existing === asWsl || existing === asForward;
    });
  }

  /** Try to select a path — blocks if it's a duplicate. */
  function trySelect(path: string) {
    const clean = cleanPath(path);
    if (!clean) return;
    if (isDuplicate(clean)) {
      setDuplicateMsg(`"${folderName(clean)}" is already a project.`);
      return;
    }
    setDuplicateMsg(null);
    setSelectedPath(clean);
  }

  async function handleBrowse() {
    const result = await open({ directory: true, title: "Select project folder" });
    if (result) trySelect(result as string);
  }

  function handleConfirmPath() {
    const p = pathInput().trim();
    if (p) trySelect(p);
  }

  async function handleBrowseCreateParent() {
    const result = await open({ directory: true, title: "Select parent folder" });
    if (result) {
      setCreateParent(result as string);
      setCreateError(null);
    }
  }

  async function handleCreate() {
    setCreateError(null);
    const parent = createParent().trim();
    const name = createName().trim();
    if (!parent) {
      setCreateError("Pick a parent folder first");
      return;
    }
    if (!name) {
      setCreateError("Give the folder a name");
      return;
    }
    setCreating(true);
    try {
      const full = await createProjectFolder(parent, name);
      setShowCreate(false);
      setCreateName("");
      trySelect(full);
    } catch (e) {
      setCreateError(e instanceof Error ? e.message : String(e));
      console.error("[NewProjectFlow] createProjectFolder error:", e);
    } finally {
      setCreating(false);
    }
  }

  // -- Picking state --
  function PickerModal() {
    return (
      <div class="new-project-picker">
        <div class="new-project-picker-header">
          <strong>New Project</strong>
          <button class="modal-btn" onClick={props.onCancel}>Cancel</button>
        </div>
        <div class="new-project-picker-body">
          <div class="new-project-input-row">
            <input
              type="text"
              class="new-project-path-input"
              placeholder="Paste a folder path..."
              value={pathInput()}
              onInput={(e) => { setPathInput(e.currentTarget.value); setDuplicateMsg(null); }}
              onKeyDown={(e) => { if (e.key === "Enter") handleConfirmPath(); }}
            />
            <button class="modal-btn primary" onClick={handleConfirmPath}>Go</button>
          </div>
          <Show when={duplicateMsg()}>
            <div class="new-project-duplicate">{duplicateMsg()}</div>
          </Show>
          <div class="new-project-or">or</div>
          <button class="modal-btn primary new-project-browse-btn" onClick={handleBrowse}>
            Browse for folder...
          </button>
          <div class="new-project-or">or</div>
          <Show
            when={showCreate()}
            fallback={
              <button
                class="modal-btn new-project-browse-btn"
                onClick={() => { setShowCreate(true); setCreateError(null); }}
              >
                Create new folder...
              </button>
            }
          >
            <div class="new-project-create-panel">
              <div class="new-project-input-row">
                <input
                  type="text"
                  class="new-project-path-input"
                  placeholder="Parent folder..."
                  value={createParent()}
                  onInput={(e) => { setCreateParent(e.currentTarget.value); setCreateError(null); }}
                />
                <button class="modal-btn" onClick={handleBrowseCreateParent}>Browse</button>
              </div>
              <div class="new-project-input-row">
                <input
                  type="text"
                  class="new-project-path-input"
                  placeholder="New folder name..."
                  value={createName()}
                  onInput={(e) => { setCreateName(e.currentTarget.value); setCreateError(null); }}
                  onKeyDown={(e) => { if (e.key === "Enter") handleCreate(); }}
                />
                <button
                  class="modal-btn primary"
                  onClick={handleCreate}
                  disabled={creating()}
                >
                  {creating() ? "Creating..." : "Create"}
                </button>
              </div>
              <Show when={createError()}>
                <div class="new-project-duplicate">{createError()}</div>
              </Show>
              <button
                class="modal-btn new-project-cancel"
                onClick={() => { setShowCreate(false); setCreateError(null); }}
              >
                Back
              </button>
            </div>
          </Show>
          <label class="new-project-continuity-check">
            <input
              type="checkbox"
              checked={withContinuity()}
              onChange={(e) => setWithContinuity(e.currentTarget.checked)}
            />
            Start with Continuity
          </label>
        </div>
      </div>
    );
  }

  // -- Ready state (draggable ghost card) --
  function GhostCard() {
    const path = selectedPath()!;
    const prefix = withContinuity() ? NEW_PROJECT_CONTINUITY_PREFIX : NEW_PROJECT_PREFIX;
    const draggable = createDraggable(prefix + path);
    const name = folderName(path);

    return (
      <div class="new-project-ghost-wrapper">
        <div
          class={`new-project-hint ${draggable.isActiveDraggable ? "faded" : ""}`}
        >
          Drag to desired pane to initialize project
        </div>
        <div
          ref={(el) => draggable(el)}
          class={`new-project-ghost-card ${draggable.isActiveDraggable ? "dragging" : ""}`}
        >
          <span class="new-project-ghost-name">{name}</span>
        </div>
        <button
          class="modal-btn new-project-cancel"
          onClick={() => { setSelectedPath(null); setPathInput(""); props.onCancel(); }}
        >
          Cancel
        </button>
      </div>
    );
  }

  return (
    <Show when={selectedPath()} fallback={<PickerModal />}>
      <GhostCard />
    </Show>
  );
}
