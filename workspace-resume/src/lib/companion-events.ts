import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CompanionEvent } from "./types";

/**
 * Subscribe to the companion's live event bus as forwarded through the
 * Tauri bridge (`companion/mod.rs::bridge_rx → app.emit("companion-event", …)`).
 *
 * The Rust companion's `broadcast::Sender<EventDto>` receives pane
 * state changes, window focus changes, and approval notifications
 * from the 1-second `tmux_poller::run` loop; this function plugs the
 * desktop frontend into the same stream so it no longer has to poll
 * for changes that the backend already knows about.
 *
 * Returns the Tauri `UnlistenFn` so callers can clean up on unmount.
 * Events the handler doesn't recognise (new Rust variants landing
 * before the TS type is updated) are routed through the `type: string`
 * catch-all in `CompanionEvent` — callers can `switch`-then-default
 * without TS complaining.
 */
export async function subscribeCompanionEvents(
  onEvent: (ev: CompanionEvent) => void,
): Promise<UnlistenFn> {
  return listen<CompanionEvent>("companion-event", (evt) => {
    onEvent(evt.payload);
  });
}
