import { createSignal, createMemo, For, Show, onMount, onCleanup } from "solid-js";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { createSortable, SortableProvider, transformStyle } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import { SettingsPanel, showAnimations } from "../SettingsPanel";
import { GlobalActivePanel } from "./GlobalActivePanel";
import {
  createSession,
  killSession,
  createWindow,
  killWindow,
  renameSession,
  renameWindow,
  getTerminalSettings,
} from "../../lib/tauri-commands";
import type { TmuxWindow } from "../../lib/types";
import { Activity, Settings as SettingsIcon, Plus, ChevronDown } from "../ui/icons";
import { accountForPane, ACCOUNT_COLORS } from "../../lib/account";
import { StatusOrb } from "../ui/StatusOrb";
import { deriveName, matchProjectByPath } from "../../lib/path";

// Window tab reorder prefix (only remaining drag type in top bar)
export const WINDOW_TAB_PREFIX = "window-tab:";

// ---------------------------------------------------------------------------
// Window tab (still sortable)
// ---------------------------------------------------------------------------

function WindowTab(props: {
  win: TmuxWindow;
  displayName: string;
  isSelected: boolean;
  isEditing: boolean;
  editValue: string;
  hasActive: boolean;
  hasWaiting: boolean;
  onEditInput: (value: string) => void;
  onClick: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
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
      title={`${props.displayName} (${props.win.panes} panes)${
        props.hasWaiting ? " — waiting" : ""
      }\nRight-click: rename / kill`}
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
        <span class="tab-label">{props.displayName}</span>
      </Show>
    </button>
  );
}

// ---------------------------------------------------------------------------
// TopBar — single row
// ---------------------------------------------------------------------------

export function TopBar() {
  const { state, selectTmuxSession, selectTmuxWindow, refreshTmuxState, pausePolling, resumePolling } = useApp();
  const [showSettings, setShowSettings] = createSignal(false);
  const [showGlobalActive, setShowGlobalActive] = createSignal(false);
  const [alwaysOnTop, setAlwaysOnTop] = createSignal(true);
  const [sessionDropdownOpen, setSessionDropdownOpen] = createSignal(false);

  const SHELL_WINDOW_NAMES = new Set(["claude", "claude-b", "bash", "zsh", "sh", "fish", "node", "-"]);

  function windowDisplayName(win: TmuxWindow): string {
    const rawName = win.name || "";
    if (!SHELL_WINDOW_NAMES.has(rawName.toLowerCase())) return rawName;
    // TopBar's window tabs always belong to the selected LOCAL session
    // (remote sessions aren't yet exposed in tabs), so the lookup is
    // scoped to `local|<selected-session>|<win.index>` in the new
    // host-namespaced windowStatuses map.
    const status = state.windowStatuses[`local|${state.selectedTmuxSession}|${win.index}`];
    const paths = status?.active_paths ?? [];
    for (const panePath of paths) {
      if (!panePath) continue;
      const project = matchProjectByPath(panePath, state.projects);
      if (project) return project.meta.display_name || deriveName(project.actual_path);
      return deriveName(panePath);
    }
    return rawName || `window ${win.index}`;
  }

  const accountSummary = createMemo(() => {
    let andrea = 0;
    let bravura = 0;
    for (const pane of state.tmuxPanes) {
      const acct = accountForPane(pane);
      if (acct === "andrea") andrea++;
      else if (acct === "bravura") bravura++;
    }
    return { andrea, bravura };
  });

  onMount(async () => {
    const win = getCurrentWebviewWindow();
    await win.setAlwaysOnTop(true);
  });

  async function toggleAlwaysOnTop() {
    const next = !alwaysOnTop();
    await getCurrentWebviewWindow().setAlwaysOnTop(next);
    setAlwaysOnTop(next);
  }

  // Confirm-kill state
  const [confirmKill, setConfirmKill] = createSignal<{
    type: "session" | "window";
    label: string;
    action: () => Promise<void>;
  } | null>(null);

  // Inline rename state
  const [editingWindow, setEditingWindow] = createSignal<number | null>(null);
  const [editValue, setEditValue] = createSignal("");

  // Right-click context menu
  const [contextMenu, setContextMenu] = createSignal<{
    x: number; y: number;
    items: { label: string; action: () => void }[];
  } | null>(null);

  function showContextMenu(e: MouseEvent, items: { label: string; action: () => void }[]) {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, items });
  }
  function closeContextMenu() { setContextMenu(null); }
  onMount(() => document.addEventListener("click", closeContextMenu));
  onCleanup(() => document.removeEventListener("click", closeContextMenu));

  // Close session dropdown on outside click
  onMount(() => document.addEventListener("click", () => setSessionDropdownOpen(false)));

  const orderedSessions = createMemo(() => {
    const sessions = [...state.tmuxSessions];
    const order = state.sessionOrder;
    if (order.length === 0) return sessions;
    const orderMap = new Map(order.map((name, i) => [name, i]));
    return sessions.sort((a, b) => (orderMap.get(a.name) ?? Infinity) - (orderMap.get(b.name) ?? Infinity));
  });

  const windowIds = createMemo(() =>
    state.tmuxWindows.map((w) => WINDOW_TAB_PREFIX + w.index),
  );

  // Session actions
  async function handleCreateSession() {
    const existing = new Set(state.tmuxSessions.map((s) => s.name));
    const settings = await getTerminalSettings().catch(() => null);
    let name = settings?.tmux_session_name || "main";
    let i = 2;
    while (existing.has(name)) name = `session-${i++}`;
    try {
      await createSession(name);
      refreshTmuxState();
      selectTmuxSession(name);
    } catch (e) {
      console.error("[TopBar] createSession error:", e);
    }
  }

  async function handleKillSession(sessionName: string) {
    setConfirmKill({
      type: "session", label: sessionName,
      action: async () => { await killSession(sessionName); refreshTmuxState(); },
    });
  }

  async function handleRenameSession(oldName: string) {
    const newName = prompt("Rename session:", oldName);
    if (!newName || newName === oldName) return;
    try {
      await renameSession(oldName, newName);
      refreshTmuxState();
      if (state.selectedTmuxSession === oldName) selectTmuxSession(newName);
    } catch (e) {
      console.error("[TopBar] renameSession error:", e);
    }
  }

  // Window actions
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
    } catch (e) {
      console.error("[TopBar] createWindow error:", e);
    }
  }

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
    } catch (e) {
      console.error("[TopBar] renameWindow error:", e);
    }
  }

  function requestKillWindow(e: MouseEvent, windowIndex: number, windowName: string) {
    e.stopPropagation();
    setConfirmKill({
      type: "window", label: `${windowIndex}: ${windowName}`,
      action: async () => {
        const sess = state.selectedTmuxSession;
        if (!sess) return;
        await killWindow(sess, windowIndex);
        refreshTmuxState();
      },
    });
  }

  async function executeKill() {
    const pending = confirmKill();
    if (!pending) return;
    try {
      await pending.action();
    } catch (e) {
      console.error("[TopBar] confirmKill action error:", e);
    }
    setConfirmKill(null);
  }

  return (
    <header class="top-bar">
      <div class="top-bar-row">
        {/* Status orb */}
        <StatusOrb />

        {/* Session dropdown */}
        <div class="session-dropdown" onClick={(e) => e.stopPropagation()}>
          <button
            class="session-dropdown-trigger"
            onClick={() => setSessionDropdownOpen((v) => !v)}
            title="Switch session"
          >
            <span>{state.selectedTmuxSession || "No session"}</span>
            <ChevronDown size={12} />
          </button>
          <Show when={sessionDropdownOpen()}>
            <div class="session-dropdown-menu">
              <For each={orderedSessions()}>
                {(session) => (
                  <button
                    class={`session-dropdown-item ${state.selectedTmuxSession === session.name ? "active" : ""}`}
                    onClick={() => {
                      selectTmuxSession(session.name);
                      setSessionDropdownOpen(false);
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      showContextMenu(e, [
                        { label: "Rename", action: () => handleRenameSession(session.name) },
                        { label: "Kill", action: () => handleKillSession(session.name) },
                      ]);
                    }}
                  >
                    <span>{session.name}</span>
                    <span class="session-dropdown-meta">{session.windows}w{session.attached ? " ·" : ""}</span>
                  </button>
                )}
              </For>
              <button class="session-dropdown-item add" onClick={handleCreateSession}>
                <Plus size={12} /> New session
              </button>
            </div>
          </Show>
        </div>

        <span class="top-bar-separator">:</span>

        {/* Window tabs */}
        <div class="window-tabs">
          <Show when={state.selectedTmuxSession && state.tmuxWindows.length > 0}>
            <SortableProvider ids={windowIds()}>
              <For each={state.tmuxWindows}>
                {(win) => {
                  const winStatus = () => state.windowStatuses[`local|${state.selectedTmuxSession}|${win.index}`];
                  return (
                    <WindowTab
                      win={win}
                      displayName={windowDisplayName(win)}
                      isSelected={state.selectedTmuxWindow === win.index}
                      isEditing={editingWindow() === win.index}
                      editValue={editValue()}
                      hasActive={winStatus()?.has_active ?? false}
                      hasWaiting={(winStatus()?.waiting_panes?.length ?? 0) > 0}
                      onEditInput={setEditValue}
                      onClick={() => selectTmuxWindow(win.index)}
                      onCommitRename={() => commitRenameWindow(win.index)}
                      onCancelRename={() => { setEditingWindow(null); resumePolling(); }}
                      onContextMenu={(e) => showContextMenu(e, [
                        { label: "Rename", action: () => startRenameWindow(win.index, win.name) },
                        { label: "Kill", action: () => requestKillWindow(e, win.index, win.name) },
                      ])}
                    />
                  );
                }}
              </For>
            </SortableProvider>
          </Show>
          <button class="tab-add-btn" onClick={handleCreateWindow} title="New window">
            <Plus size={14} />
          </button>
        </div>

        {/* Right side: account dots + controls */}
        <div class="top-bar-right">
          <Show when={accountSummary().andrea > 0}>
            <span class="top-bar-account-dot" style={{ background: ACCOUNT_COLORS.andrea }} title={`Andrea: ${accountSummary().andrea}`} />
          </Show>
          <Show when={accountSummary().bravura > 0}>
            <span class="top-bar-account-dot" style={{ background: ACCOUNT_COLORS.bravura }} title={`Bravura: ${accountSummary().bravura}`} />
          </Show>

          <button
            class={`top-bar-icon-btn ${alwaysOnTop() ? "active" : ""}`}
            onClick={toggleAlwaysOnTop}
            title={alwaysOnTop() ? "Unpin from top" : "Pin on top"}
          >
            <span class="aot-pill" />
          </button>

          <button
            class="top-bar-icon-btn"
            onClick={() => setShowGlobalActive((v) => !v)}
            title="All active Claude sessions"
          >
            <Activity size={15} />
          </button>

          <button
            class="top-bar-icon-btn"
            onClick={() => setShowSettings((v) => !v)}
            title="Settings"
          >
            <SettingsIcon size={15} />
          </button>
        </div>
      </div>

      {/* Settings slide-over */}
      <Show when={showSettings()}>
        <div class="slide-over-backdrop" onClick={() => setShowSettings(false)}>
          <div class="slide-over" onClick={(e) => e.stopPropagation()}>
            <div class="slide-over-header">
              <strong>Settings</strong>
              <button class="modal-btn" onClick={() => setShowSettings(false)}>{"\u2715"}</button>
            </div>
            <div class="slide-over-body">
              <SettingsPanel />
            </div>
          </div>
        </div>
      </Show>

      {/* Global active overlay */}
      <Show when={showGlobalActive()}>
        <div class="active-overlay-backdrop" onClick={() => setShowGlobalActive(false)}>
          <div class="active-overlay" onClick={(e) => e.stopPropagation()}>
            <GlobalActivePanel onClose={() => setShowGlobalActive(false)} />
          </div>
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
              <button class="modal-btn" onClick={() => setConfirmKill(null)}>Cancel</button>
              <button class="modal-btn danger" onClick={executeKill}>Kill</button>
            </div>
          </div>
        </div>
      </Show>
    </header>
  );
}
