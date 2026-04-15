import { createMemo } from "solid-js";
import { useApp } from "../../contexts/AppContext";

/**
 * 12px circle showing fleet-wide health at a glance.
 *   Green pulse  = all Claude panes are running or idle (everything fine)
 *   Amber pulse  = at least one pane is waiting for approval/input
 *   Grey         = no Claude sessions running at all
 */
export function StatusOrb() {
  const { state } = useApp();

  const status = createMemo(() => {
    const panes = state.tmuxPanes;
    if (panes.length === 0) return "none";
    const hasWaiting = Object.values(state.windowStatuses).some(
      (ws) => ws.waiting_panes && ws.waiting_panes.length > 0,
    );
    if (hasWaiting) return "waiting";
    const hasRunning = panes.some(
      (p) => p.current_command?.toLowerCase().includes("claude"),
    );
    return hasRunning ? "running" : "idle";
  });

  return (
    <span
      class={`status-orb status-orb-${status()}`}
      title={
        status() === "waiting"
          ? "A pane needs attention"
          : status() === "running"
            ? "Claude sessions active"
            : status() === "idle"
              ? "All sessions idle"
              : "No sessions"
      }
    />
  );
}
