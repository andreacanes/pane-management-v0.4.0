import { createSignal, For, Show } from "solid-js";
import { CheckCircle2, AlertTriangle, X, Circle } from "./icons";

export type ToastKind = "success" | "error" | "info";

interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  detail?: string;
}

const [toasts, setToasts] = createSignal<Toast[]>([]);
let nextId = 1;

/** Push a toast from anywhere. Auto-dismisses after `ttlMs` (default 4s). */
export function showToast(kind: ToastKind, message: string, detail?: string, ttlMs = 4000) {
  const id = nextId++;
  setToasts([...toasts(), { id, kind, message, detail }]);
  setTimeout(() => {
    setToasts(toasts().filter((t) => t.id !== id));
  }, ttlMs);
}

export const toastSuccess = (msg: string, detail?: string) => showToast("success", msg, detail);
export const toastError   = (msg: string, detail?: string) => showToast("error",   msg, detail);
export const toastInfo    = (msg: string, detail?: string) => showToast("info",    msg, detail);

function iconFor(kind: ToastKind) {
  if (kind === "success") return <CheckCircle2 size={16} />;
  if (kind === "error")   return <AlertTriangle size={16} />;
  return <Circle size={16} />;
}

/** Mounted once in App.tsx. Renders the active toast stack. */
export function ToastHost() {
  function dismiss(id: number) {
    setToasts(toasts().filter((t) => t.id !== id));
  }
  return (
    <div class="toast-host" role="status" aria-live="polite">
      <For each={toasts()}>
        {(t) => (
          <div class={`toast toast-${t.kind}`}>
            <span class="toast-icon">{iconFor(t.kind)}</span>
            <div class="toast-body">
              <span class="toast-message">{t.message}</span>
              <Show when={t.detail}>
                <span class="toast-detail">{t.detail}</span>
              </Show>
            </div>
            <button class="toast-dismiss" onClick={() => dismiss(t.id)} aria-label="Dismiss">
              <X size={12} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
