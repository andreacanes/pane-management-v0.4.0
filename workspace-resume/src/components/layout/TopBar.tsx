import { createSignal, createMemo, For, Show, onMount, onCleanup } from "solid-js";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { createSortable, SortableProvider, transformStyle } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import { SettingsPanel, showAnimations, showHotkeyHint } from "../SettingsPanel";
import {
  createSession,
  killSession,
  createWindow,
  killWindow,
  renameSession,
  renameWindow,
  getTerminalSettings,
} from "../../lib/tauri-commands";
import type { TmuxSession, TmuxWindow } from "../../lib/types";
import { NeonTitleSign } from "../theme/NeonSigns";

// Sortable session/window tab ID prefixes
export const SESSION_TAB_PREFIX = "session-tab:";
export const WINDOW_TAB_PREFIX = "window-tab:";

// ---------------------------------------------------------------------------
// Sortable session tab
// ---------------------------------------------------------------------------

function SortableSessionTab(props: {
  session: TmuxSession;
  isSelected: boolean;
  isEditing: boolean;
  editValue: string;
  onEditInput: (value: string) => void;
  onClick: () => void;
  onStartRename: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onKill: (e: MouseEvent) => void;
  onContextMenu: (e: MouseEvent) => void;
}) {
  const sortable = createSortable(SESSION_TAB_PREFIX + props.session.name);

  return (
    <button
      ref={(el) => sortable(el)}
      class={`session-tab ${props.isSelected ? "active" : ""} ${sortable.isActiveDraggable ? "dragging" : ""}`}
      style={transformStyle(sortable.transform)}
      onClick={() => { if (!props.isEditing) props.onClick(); }}
      onContextMenu={(e) => props.onContextMenu(e)}
      title={`${props.session.name} (${props.session.windows} windows${
        props.session.attached ? ", attached" : ""
      })\nRight-click for options · Drag to reorder`}
    >
      <Show
        when={!props.isEditing}
        fallback={
          <input
            class="tab-rename-input"
            value={props.editValue}
            onInput={(e) => props.onEditInput(e.currentTarget.value)}
            onBlur={() => props.onCommitRename()}
            onKeyDown={(e) => {
              if (e.key === "Enter") props.onCommitRename();
              if (e.key === "Escape") props.onCancelRename();
            }}
            onClick={(e) => e.stopPropagation()}
            ref={(el) => setTimeout(() => el.focus(), 0)}
          />
        }
      >
        <span class="tab-label">{props.session.name}</span>
      </Show>
      <span
        class="tab-badge-close"
        onClick={(e) => props.onKill(e)}
        title={`Kill session "${props.session.name}"`}
      >
        <span class="tab-badge-number">{props.session.windows}</span>
        <span class="tab-badge-x">{"\u2715"}</span>
      </span>
    </button>
  );
}

// ---------------------------------------------------------------------------
// Sortable window tab
// ---------------------------------------------------------------------------

function SortableWindowTab(props: {
  win: TmuxWindow;
  isSelected: boolean;
  isEditing: boolean;
  editValue: string;
  hasActive: boolean;
  hasWaiting: boolean;
  onEditInput: (value: string) => void;
  onClick: () => void;
  onStartRename: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onKill: (e: MouseEvent) => void;
  onContextMenu: (e: MouseEvent) => void;
}) {
  const sortable = createSortable(WINDOW_TAB_PREFIX + props.win.index);

  const dotClass = () => {
    if (props.hasWaiting) return "window-status-dot waiting";
    if (props.hasActive) return "window-status-dot active";
    return "";
  };

  return (
    <button
      ref={(el) => sortable(el)}
      class={`window-tab ${props.isSelected ? "active" : ""} ${sortable.isActiveDraggable ? "dragging" : ""}`}
      style={transformStyle(sortable.transform)}
      onClick={() => { if (!props.isEditing) props.onClick(); }}
      onContextMenu={(e) => props.onContextMenu(e)}
      title={`Window ${props.win.index}: ${props.win.name} (${props.win.panes} panes)${
        props.hasWaiting ? "\nAgent waiting for approval" : ""
      }\nRight-click for options · Drag to reorder`}
    >
      <Show when={dotClass()}>
        <span class={dotClass()} />
      </Show>
      <Show
        when={!props.isEditing}
        fallback={
          <input
            class="tab-rename-input"
            value={props.editValue}
            onInput={(e) => props.onEditInput(e.currentTarget.value)}
            onBlur={() => props.onCommitRename()}
            onKeyDown={(e) => {
              if (e.key === "Enter") props.onCommitRename();
              if (e.key === "Escape") props.onCancelRename();
            }}
            onClick={(e) => e.stopPropagation()}
            ref={(el) => setTimeout(() => el.focus(), 0)}
          />
        }
      >
        <span class="tab-label">{props.win.name}</span>
      </Show>
      <span
        class="tab-badge-close"
        onClick={(e) => props.onKill(e)}
        title={`Kill window ${props.win.index}`}
      >
        <span class="tab-badge-number">{props.win.panes}</span>
        <span class="tab-badge-x">{"\u2715"}</span>
      </span>
    </button>
  );
}

// ---------------------------------------------------------------------------
// TopBar
// ---------------------------------------------------------------------------

export function TopBar() {
  const { state, selectTmuxSession, selectTmuxWindow, refreshTmuxState, activeProjectCount, pausePolling, resumePolling } = useApp();
  const [showSettings, setShowSettings] = createSignal(false);
  const [alwaysOnTop, setAlwaysOnTop] = createSignal(true);
  const [appFocused, setAppFocused] = createSignal(true);
  const [blurFlash, setBlurFlash] = createSignal(false);
  const [activeTheme, setActiveTheme] = createSignal(
    document.documentElement.getAttribute("data-theme") || "default"
  );
  onMount(() => {
    const obs = new MutationObserver(() => {
      setActiveTheme(document.documentElement.getAttribute("data-theme") || "default");
    });
    obs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    onCleanup(() => obs.disconnect());
  });
  const isNeonShinjuku = () => activeTheme() === "neon-shinjuku";

  // Set always-on-top on mount + track focus via Tauri window events
  onMount(async () => {
    const win = getCurrentWebviewWindow();
    await win.setAlwaysOnTop(true);

    await win.onFocusChanged(({ payload: focused }) => {
      setAppFocused(focused);
      if (!focused && alwaysOnTop() && showAnimations()) {
        setBlurFlash(true);
        setTimeout(() => setBlurFlash(false), 2000);
      }
    });
  });

  async function toggleAlwaysOnTop() {
    const next = !alwaysOnTop();
    const win = getCurrentWebviewWindow();
    await win.setAlwaysOnTop(next);
    setAlwaysOnTop(next);
  }

  // Confirm-kill state
  const [confirmKill, setConfirmKill] = createSignal<{
    type: "session" | "window";
    label: string;
    action: () => Promise<void>;
  } | null>(null);

  // Inline rename state
  const [editingSession, setEditingSession] = createSignal<string | null>(null);
  const [editingWindow, setEditingWindow] = createSignal<number | null>(null);
  const [editValue, setEditValue] = createSignal("");

  // Right-click context menu
  const [contextMenu, setContextMenu] = createSignal<{
    x: number;
    y: number;
    items: { label: string; action: () => void }[];
  } | null>(null);

  function showContextMenu(e: MouseEvent, items: { label: string; action: () => void }[]) {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, items });
  }

  function closeContextMenu() {
    setContextMenu(null);
  }

  // Close context menu on click anywhere
  onMount(() => document.addEventListener("click", closeContextMenu));
  onCleanup(() => document.removeEventListener("click", closeContextMenu));

  // Window/session info modal
  const [infoModal, setInfoModal] = createSignal<{
    type: "session" | "window";
    name: string;
    details: Record<string, string>;
  } | null>(null);

  function showSessionInfo(sessionName: string) {
    const session = state.tmuxSessions.find((s) => s.name === sessionName);
    if (!session) return;
    setInfoModal({
      type: "session",
      name: session.name,
      details: {
        "Windows": String(session.windows),
        "Attached": session.attached ? "Yes" : "No",
      },
    });
  }

  function showWindowInfo(winIndex: number) {
    const win = state.tmuxWindows.find((w) => w.index === winIndex);
    if (!win) return;
    const winStatus = state.windowStatuses[String(winIndex)];
    const activeCount = winStatus?.active_panes?.length ?? 0;
    const waitingCount = winStatus?.waiting_panes?.length ?? 0;
    setInfoModal({
      type: "window",
      name: `${win.index}: ${win.name}`,
      details: {
        "Panes": String(win.panes),
        "Active Claude sessions": String(activeCount),
        "Waiting for approval": String(waitingCount),
        "Active": win.active ? "Yes" : "No",
      },
    });
  }

  const totalProjects = () => state.projects.length;

  // -- Ordered sessions (respects stored session order) ----------------------

  const orderedSessions = createMemo(() => {
    const sessions = [...state.tmuxSessions];
    const order = state.sessionOrder;
    if (order.length === 0) return sessions;
    const orderMap = new Map(order.map((name, i) => [name, i]));
    return sessions.sort((a, b) => {
      const ai = orderMap.get(a.name) ?? Infinity;
      const bi = orderMap.get(b.name) ?? Infinity;
      return ai - bi;
    });
  });

  const sessionIds = createMemo(() =>
    orderedSessions().map((s) => SESSION_TAB_PREFIX + s.name),
  );

  const windowIds = createMemo(() =>
    state.tmuxWindows.map((w) => WINDOW_TAB_PREFIX + w.index),
  );

  // -- Session actions -------------------------------------------------------

  async function handleCreateSession() {
    const existing = new Set(state.tmuxSessions.map((s) => s.name));
    // Default new-session name comes from terminal_settings, not a hardcoded value
    const settings = await getTerminalSettings().catch(() => null);
    let name = settings?.tmux_session_name || "main";
    let i = 2;
    while (existing.has(name)) {
      name = `session-${i++}`;
    }
    try {
      await createSession(name);
      refreshTmuxState();
      selectTmuxSession(name);
    } catch (err) {
      console.error("[TopBar] create session error:", err);
    }
  }

  function requestKillSession(e: MouseEvent, sessionName: string) {
    e.stopPropagation();
    setConfirmKill({
      type: "session",
      label: sessionName,
      action: async () => {
        await killSession(sessionName);
        refreshTmuxState();
      },
    });
  }

  // -- Session rename --------------------------------------------------------

  function startRenameSession(sessionName: string) {
    setEditValue(sessionName);
    setEditingSession(sessionName);
    pausePolling();
  }

  async function commitRenameSession(oldName: string) {
    const newName = editValue().trim();
    setEditingSession(null);
    resumePolling();
    if (!newName || newName === oldName) return;
    try {
      await renameSession(oldName, newName);
      refreshTmuxState();
      if (state.selectedTmuxSession === oldName) {
        selectTmuxSession(newName);
      }
    } catch (err) {
      console.error("[TopBar] rename session error:", err);
    }
  }

  // -- Window actions --------------------------------------------------------

  async function handleCreateWindow() {
    const sess = state.selectedTmuxSession;
    if (!sess) return;
    try {
      const windows = await createWindow(sess);
      refreshTmuxState();
      if (windows.length > 0) {
        const newest = windows.reduce((a, b) => (b.index > a.index ? b : a));
        selectTmuxWindow(newest.index);
      }
    } catch (err) {
      console.error("[TopBar] create window error:", err);
    }
  }

  function requestKillWindow(e: MouseEvent, windowIndex: number, windowName: string) {
    e.stopPropagation();
    setConfirmKill({
      type: "window",
      label: `${windowIndex}: ${windowName}`,
      action: async () => {
        const sess = state.selectedTmuxSession;
        if (!sess) return;
        await killWindow(sess, windowIndex);
        refreshTmuxState();
      },
    });
  }

  // -- Window rename ---------------------------------------------------------

  function startRenameWindow(windowIndex: number, windowName: string) {
    setEditValue(windowName);
    setEditingWindow(windowIndex);
    pausePolling();
  }

  async function commitRenameWindow(windowIndex: number) {
    const newName = editValue().trim();
    setEditingWindow(null);
    resumePolling();
    if (!newName) return;
    const sess = state.selectedTmuxSession;
    if (!sess) return;
    try {
      await renameWindow(sess, windowIndex, newName);
      refreshTmuxState();
    } catch (err) {
      console.error("[TopBar] rename window error:", err);
    }
  }

  // -- Confirm modal ---------------------------------------------------------

  async function executeKill() {
    const pending = confirmKill();
    if (!pending) return;
    try {
      await pending.action();
    } catch (err) {
      console.error("[TopBar] kill error:", err);
    }
    setConfirmKill(null);
  }

  return (
    <header class="top-bar">
      <div class="top-bar-header">
        <span class="top-bar-title">Pane Management</span>
        <span class="top-bar-stats">
          {totalProjects()} projects | {activeProjectCount()} active
        </span>
        <Show when={isNeonShinjuku()}>
          <NeonTitleSign />
        </Show>
        <div class="on-top-area">
          <Show when={alwaysOnTop() && showHotkeyHint()}>
            <span class="on-top-hint">Ctrl+Space to Hide/Show</span>
          </Show>
          <button
            class={`on-top-toggle ${alwaysOnTop() ? "active" : ""} ${alwaysOnTop() && showAnimations() ? "gentle-glow" : ""} ${blurFlash() && showAnimations() ? "blur-flash" : ""}`}
            onClick={toggleAlwaysOnTop}
            title={alwaysOnTop() ? "Disable always on top" : "Keep window on top"}
          >
            <span class="on-top-pill" />
            <span class="on-top-label">Always On Top</span>
          </button>
        </div>
        <button
          class="settings-gear-btn"
          onClick={() => setShowSettings((v) => !v)}
          title="Settings"
        >
          {"\u2699"}
        </button>
      </div>

      <Show when={showSettings()}>
        <div class="modal-backdrop" onClick={() => setShowSettings(false)}>
          <div class="settings-modal" onClick={(e) => e.stopPropagation()}>
            <div class="settings-modal-header">
              <strong>Settings</strong>
              <button class="modal-btn" onClick={() => setShowSettings(false)}>{"\u2715"}</button>
            </div>
            <div class="settings-modal-body">
              <SettingsPanel />
            </div>
          </div>
        </div>
      </Show>

      {/* Session tabs */}
      <div class="session-tabs">
        <Show
          when={state.tmuxSessions.length > 0}
          fallback={
            <span class="session-tabs-empty">No tmux sessions running</span>
          }
        >
          <SortableProvider ids={sessionIds()}>
            <For each={orderedSessions()}>
              {(session) => (
                <SortableSessionTab
                  session={session}
                  isSelected={state.selectedTmuxSession === session.name}
                  isEditing={editingSession() === session.name}
                  editValue={editValue()}
                  onEditInput={setEditValue}
                  onClick={() => selectTmuxSession(session.name)}
                  onStartRename={() => startRenameSession(session.name)}
                  onCommitRename={() => commitRenameSession(session.name)}
                  onCancelRename={() => { setEditingSession(null); resumePolling(); }}
                  onKill={(e) => requestKillSession(e, session.name)}
                  onContextMenu={(e) => showContextMenu(e, [
                    { label: "Rename", action: () => startRenameSession(session.name) },
                    { label: "Info", action: () => showSessionInfo(session.name) },
                  ])}
                />
              )}
            </For>
          </SortableProvider>
        </Show>

        <button
          class="tab-add-btn"
          onClick={handleCreateSession}
          title="New session"
        >
          +
        </button>
      </div>

      {/* Window tabs */}
      <Show when={state.selectedTmuxSession && state.tmuxWindows.length > 0}>
        <div class="window-tabs">
          <SortableProvider ids={windowIds()}>
            <For each={state.tmuxWindows}>
              {(win) => {
                const winStatus = () => state.windowStatuses[String(win.index)];
                return (
                  <SortableWindowTab
                    win={win}
                    isSelected={state.selectedTmuxWindow === win.index}
                    isEditing={editingWindow() === win.index}
                    editValue={editValue()}
                    hasActive={winStatus()?.has_active ?? false}
                    hasWaiting={(winStatus()?.waiting_panes?.length ?? 0) > 0}
                    onEditInput={setEditValue}
                    onClick={() => selectTmuxWindow(win.index)}
                    onStartRename={() => startRenameWindow(win.index, win.name)}
                    onCommitRename={() => commitRenameWindow(win.index)}
                    onCancelRename={() => { setEditingWindow(null); resumePolling(); }}
                    onKill={(e) => requestKillWindow(e, win.index, win.name)}
                    onContextMenu={(e) => showContextMenu(e, [
                      { label: "Rename", action: () => startRenameWindow(win.index, win.name) },
                      { label: "Info", action: () => showWindowInfo(win.index) },
                    ])}
                  />
                );
              }}
            </For>
          </SortableProvider>

          <button
            class="tab-add-btn"
            onClick={handleCreateWindow}
            title="New window"
          >
            +
          </button>
        </div>
      </Show>

      {/* Right-click context menu */}
      <Show when={contextMenu()}>
        <div
          class="tab-context-menu"
          style={{ left: `${contextMenu()!.x}px`, top: `${contextMenu()!.y}px` }}
        >
          <For each={contextMenu()!.items}>
            {(item) => (
              <button class="tab-context-item" onClick={() => { item.action(); closeContextMenu(); }}>
                {item.label}
              </button>
            )}
          </For>
        </div>
      </Show>

      {/* Info modal */}
      <Show when={infoModal()}>
        <div class="modal-backdrop" onClick={() => setInfoModal(null)}>
          <div class="confirm-modal pane-info-modal" onClick={(e) => e.stopPropagation()}>
            <div class="pane-info-modal-header">
              <strong>{infoModal()!.type === "session" ? "Session" : "Window"}: {infoModal()!.name}</strong>
              <button class="modal-btn" onClick={() => setInfoModal(null)}>{"\u2715"}</button>
            </div>
            <div class="pane-info-modal-body">
              <For each={Object.entries(infoModal()!.details)}>
                {([key, val]) => (
                  <>
                    <span class="pane-info-label">{key}</span>
                    <span class="pane-info-value">{val}</span>
                  </>
                )}
              </For>
            </div>
          </div>
        </div>
      </Show>

      {/* Confirm kill modal */}
      <Show when={confirmKill()}>
        <div class="modal-backdrop" onClick={() => setConfirmKill(null)}>
          <div class="confirm-modal" onClick={(e) => e.stopPropagation()}>
            <p class="confirm-message">
              Kill {confirmKill()!.type} <strong>{confirmKill()!.label}</strong>?
            </p>
            <p class="confirm-warning">
              This will terminate all processes in this {confirmKill()!.type}.
            </p>
            <div class="confirm-actions">
              <button class="modal-btn" onClick={() => setConfirmKill(null)}>
                Cancel
              </button>
              <button class="modal-btn danger" onClick={executeKill}>
                Kill
              </button>
            </div>
          </div>
        </div>
      </Show>
    </header>
  );
}
