import { createSignal, For, Show, onMount } from "solid-js";
import { LazyStore } from "@tauri-apps/plugin-store";
import {
  getTerminalSettings,
  updateTmuxSessionName,
  getErrorLog,
  clearErrorLog,
  getCompanionConfig,
  getCompanionQr,
  rotateCompanionToken,
  getRemoteHosts,
  setRemoteHosts,
  type CompanionConfig,
} from "../lib/tauri-commands";
import { toastError, toastSuccess } from "./ui/Toast";
import { useApp } from "../contexts/AppContext";
import type { ErrorLogEntry } from "../lib/types";

const uiStore = new LazyStore("settings.json");

// Module-level signals survive component unmount/remount
const [showAnimations, setShowAnimations] = createSignal(true);
const [showHotkeyHint, setShowHotkeyHint] = createSignal(true);
export { showAnimations, showHotkeyHint };

// Load prefs once at module init
(async () => {
  try {
    const anim = await uiStore.get<boolean>("show_on_top_animations");
    if (anim !== null && anim !== undefined) setShowAnimations(anim);
    const hint = await uiStore.get<boolean>("show_hotkey_hint");
    if (hint !== null && hint !== undefined) setShowHotkeyHint(hint);
  } catch (_) {}
})();

export function SettingsPanel() {
  const { refreshRemoteHosts: refreshHostsInGrid } = useApp();
  const [tmuxSessionName, setTmuxSessionName] = createSignal("main");
  const [tmuxSessionDraft, setTmuxSessionDraft] = createSignal("main");
  const [sessionNameError, setSessionNameError] = createSignal<string | null>(null);
  const [errors, setErrors] = createSignal<ErrorLogEntry[]>([]);
  const [updating, setUpdating] = createSignal(false);

  const [companionConfig, setCompanionConfig] = createSignal<CompanionConfig | null>(null);
  const [companionQr, setCompanionQr] = createSignal<string | null>(null);
  const [tokenRevealed, setTokenRevealed] = createSignal(false);

  // Remote hosts — SSH aliases the app polls for Mac-style multi-host
  // panes. `remoteHostsDraft` is the editable mirror; we save when the
  // user clicks Save (vs. losing every keystroke on blur, which would
  // be surprising on a typing-intensive control).
  const [remoteHosts, setRemoteHostsState] = createSignal<string[]>([]);
  const [remoteHostsDraft, setRemoteHostsDraft] = createSignal<string[]>([]);
  const [remoteHostsSaving, setRemoteHostsSaving] = createSignal(false);

  onMount(async () => {
    try {
      const settings = await getTerminalSettings();
      setTmuxSessionName(settings.tmux_session_name);
      setTmuxSessionDraft(settings.tmux_session_name);
    } catch (e) {
      console.error("[SettingsPanel] Failed to load settings:", e);
    }
    try {
      const hosts = await getRemoteHosts();
      setRemoteHostsState(hosts);
      setRemoteHostsDraft(hosts);
    } catch (e) {
      console.error("[SettingsPanel] Failed to load remote hosts:", e);
    }
    await loadCompanion();
    await refreshErrors();
  });

  async function handleSaveRemoteHosts() {
    setRemoteHostsSaving(true);
    try {
      const saved = await setRemoteHosts(remoteHostsDraft());
      setRemoteHostsState(saved);
      setRemoteHostsDraft(saved);
      // Tell AppContext to re-read the store + kick a fresh remote poll
      // so the grid reflects the new host list without an app restart.
      await refreshHostsInGrid();
      toastSuccess(
        "Remote hosts saved",
        saved.length === 0
          ? "No hosts — AppContext will default to ['mac']"
          : saved.join(", "),
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toastError("Remote hosts save failed", msg);
    } finally {
      setRemoteHostsSaving(false);
    }
  }

  function addRemoteHost() {
    setRemoteHostsDraft([...remoteHostsDraft(), ""]);
  }
  function removeRemoteHost(idx: number) {
    const next = [...remoteHostsDraft()];
    next.splice(idx, 1);
    setRemoteHostsDraft(next);
  }
  function updateRemoteHost(idx: number, value: string) {
    const next = [...remoteHostsDraft()];
    next[idx] = value;
    setRemoteHostsDraft(next);
  }
  function remoteHostsDirty(): boolean {
    const a = remoteHosts();
    const b = remoteHostsDraft();
    if (a.length !== b.length) return true;
    for (let i = 0; i < a.length; i++) {
      if (a[i] !== b[i]) return true;
    }
    return false;
  }

  async function loadCompanion() {
    try {
      const cfg = await getCompanionConfig();
      setCompanionConfig(cfg);
      const qr = await getCompanionQr();
      setCompanionQr(qr);
    } catch (e) {
      console.error("[SettingsPanel] Failed to load companion config:", e);
    }
  }

  async function handleRotateToken() {
    if (!window.confirm("Rotate the companion bearer token? Existing phone sessions will need to re-auth.")) {
      return;
    }
    try {
      const cfg = await rotateCompanionToken();
      setCompanionConfig(cfg);
      const qr = await getCompanionQr();
      setCompanionQr(qr);
    } catch (e) {
      console.error("[SettingsPanel] rotate failed:", e);
    }
  }

  function maskedToken(): string {
    const t = companionConfig()?.bearer_token ?? "";
    if (!t) return "—";
    if (tokenRevealed()) return t;
    return `${t.slice(0, 4)}${"•".repeat(Math.max(t.length - 8, 0))}${t.slice(-4)}`;
  }

  async function refreshErrors() {
    try {
      const log = await getErrorLog();
      setErrors(log);
    } catch (e) {
      console.error("[SettingsPanel] Failed to load error log:", e);
    }
  }

  async function handleSaveSessionName() {
    const draft = tmuxSessionDraft().trim();
    if (draft === tmuxSessionName()) {
      setSessionNameError(null);
      return;
    }
    setUpdating(true);
    setSessionNameError(null);
    try {
      const result = await updateTmuxSessionName(draft);
      setTmuxSessionName(result.tmux_session_name);
      setTmuxSessionDraft(result.tmux_session_name);
    } catch (e) {
      setSessionNameError(String(e));
      setTmuxSessionDraft(tmuxSessionName());
    } finally {
      setUpdating(false);
    }
  }

  async function handleClearLog() {
    try {
      await clearErrorLog();
      setErrors([]);
    } catch (e) {
      console.error("[SettingsPanel] Failed to clear error log:", e);
    }
  }

  function formatErrorTimestamp(ts: string): string {
    try {
      // Timestamps are epoch seconds
      const num = Number(ts);
      if (!isNaN(num) && num > 1000000000) {
        return new Date(num * 1000).toLocaleString();
      }
      return new Date(ts).toLocaleString();
    } catch {
      return ts;
    }
  }

  async function toggleAnimations() {
    const next = !showAnimations();
    setShowAnimations(next);
    await uiStore.set("show_on_top_animations", next);
    await uiStore.save();
    document.dispatchEvent(new CustomEvent("ui-pref-changed", { detail: { showAnimations: next } }));
  }

  async function toggleHotkeyHint() {
    const next = !showHotkeyHint();
    setShowHotkeyHint(next);
    await uiStore.set("show_hotkey_hint", next);
    await uiStore.save();
    document.dispatchEvent(new CustomEvent("ui-pref-changed", { detail: { showHotkeyHint: next } }));
  }

  return (
    <div class="settings-panel">
      <div class="settings-header-row">
        <h3>Settings</h3>
        <span class="settings-version">v0.4.0</span>
      </div>

      <div class="settings-section">
        <h4>UI Preferences</h4>
        <div class="settings-row">
          <label>Always-on-top reminder animations</label>
          <button class={`settings-toggle ${showAnimations() ? "active" : ""}`} onClick={toggleAnimations}>
            <span class="settings-toggle-pill"><span /></span>
            <span>{showAnimations() ? "On" : "Off"}</span>
          </button>
        </div>
        <div class="settings-row">
          <label>Show "Ctrl+Space to Hide/Show" hint</label>
          <button class={`settings-toggle ${showHotkeyHint() ? "active" : ""}`} onClick={toggleHotkeyHint}>
            <span class="settings-toggle-pill"><span /></span>
            <span>{showHotkeyHint() ? "On" : "Off"}</span>
          </button>
        </div>
      </div>

      <div class="settings-section">
        <h4>Terminal</h4>
        <div class="settings-row">
          <label for="tmux-session-name">tmux session name:</label>
          <input
            id="tmux-session-name"
            type="text"
            value={tmuxSessionDraft()}
            disabled={updating()}
            onInput={(e) => setTmuxSessionDraft(e.currentTarget.value)}
            onBlur={handleSaveSessionName}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.currentTarget as HTMLInputElement).blur();
            }}
            placeholder="main"
          />
        </div>
        <Show when={sessionNameError()}>
          <div class="settings-row-error">{sessionNameError()}</div>
        </Show>
      </div>

      <div class="settings-section">
        <h4>Mobile companion</h4>
        <Show when={companionConfig()} fallback={<p style={{ "opacity": 0.7 }}>Loading…</p>}>
          <div class="settings-row">
            <label>URL for phone</label>
            <input
              type="text"
              readOnly
              value={companionConfig()!.suggested_url}
              onFocus={(e) => (e.currentTarget as HTMLInputElement).select()}
            />
          </div>
          <div class="settings-row">
            <label>Bearer token</label>
            <input
              type="text"
              readOnly
              value={maskedToken()}
              onFocus={(e) => (e.currentTarget as HTMLInputElement).select()}
            />
            <button
              class="modal-btn"
              style={{ "margin-left": "6px" }}
              onClick={() => setTokenRevealed(!tokenRevealed())}
            >
              {tokenRevealed() ? "Hide" : "Reveal"}
            </button>
            <button
              class="modal-btn"
              style={{ "margin-left": "6px" }}
              onClick={() => {
                navigator.clipboard.writeText(companionConfig()!.bearer_token);
              }}
            >
              Copy
            </button>
          </div>
          <div class="settings-row">
            <label>ntfy topic</label>
            <input
              type="text"
              readOnly
              value={companionConfig()!.ntfy_topic}
              onFocus={(e) => (e.currentTarget as HTMLInputElement).select()}
            />
          </div>
          <Show when={companionQr()}>
            <div style={{ "display": "flex", "justify-content": "center", "padding": "12px" }}>
              <img
                src={companionQr()!}
                alt="Setup QR code"
                style={{ "width": "220px", "height": "220px", "background": "#fff", "border-radius": "4px" }}
              />
            </div>
          </Show>
          <div class="settings-row" style={{ "justify-content": "space-between" }}>
            <button class="modal-btn" onClick={loadCompanion}>Refresh</button>
            <button class="modal-btn" onClick={handleRotateToken} style={{ "color": "#e66" }}>
              Rotate token
            </button>
          </div>
        </Show>
      </div>

      <div class="settings-section">
        <h4>Remote hosts</h4>
        <p style={{ "opacity": 0.7, "font-size": "12px", "margin": "0 0 8px" }}>
          SSH aliases the app polls for multi-host panes. Each entry must resolve via{" "}
          <code>~/.ssh/config</code> (e.g. <code>mac</code>). Blank list defaults to{" "}
          <code>mac</code> at runtime.
        </p>
        <For each={remoteHostsDraft()}>
          {(host, idx) => (
            <div class="settings-row">
              <input
                type="text"
                value={host}
                onInput={(e) => updateRemoteHost(idx(), e.currentTarget.value)}
                placeholder="ssh alias (e.g. mac)"
              />
              <button
                class="modal-btn"
                style={{ "margin-left": "6px" }}
                onClick={() => removeRemoteHost(idx())}
                title="Remove this host"
              >
                Remove
              </button>
            </div>
          )}
        </For>
        <div class="settings-row" style={{ "justify-content": "space-between" }}>
          <button class="modal-btn" onClick={addRemoteHost}>
            + Add host
          </button>
          <button
            class="modal-btn primary"
            onClick={handleSaveRemoteHosts}
            disabled={remoteHostsSaving() || !remoteHostsDirty()}
            title="Persist the list to the Tauri store and trigger a refresh"
          >
            {remoteHostsSaving() ? "Saving…" : remoteHostsDirty() ? "Save" : "Saved"}
          </button>
        </div>
      </div>

      <div class="settings-section">

      <details class="error-log-details">
        <summary>
          Error Log ({errors().length} {errors().length === 1 ? "entry" : "entries"})
        </summary>

        <Show when={errors().length === 0}>
          <p class="no-errors">No errors logged.</p>
        </Show>

        <Show when={errors().length > 0}>
          <button class="clear-log-btn" onClick={handleClearLog}>
            Clear Log
          </button>
          <div class="error-log-list">
            <For each={errors()}>
              {(entry: ErrorLogEntry) => (
                <div class="error-entry">
                  <div class="error-entry-header">
                    <span class="error-timestamp">
                      {formatErrorTimestamp(entry.timestamp)}
                    </span>
                    <span class="error-terminal">[{entry.terminal}]</span>
                  </div>
                  <div class="error-message">{entry.error}</div>
                  <div class="error-path">{entry.project_path}</div>
                </div>
              )}
            </For>
          </div>
        </Show>
      </details>
      </div>
    </div>
  );
}
