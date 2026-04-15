import { Show } from "solid-js";
import { useApp } from "../../contexts/AppContext";
import { PaneGrid } from "../pane/PaneGrid";
import { PanePresetPicker } from "../pane/PanePresetPicker";
import { QuickLaunch } from "./QuickLaunch";

export function MainArea() {
  const { state } = useApp();

  const hasSession = () => state.selectedTmuxSession != null;
  const hasWindow = () => state.selectedTmuxWindow != null;

  return (
    <main class="main-area">
      <div class="main-content">
        <Show when={hasSession() && hasWindow()}>
          <PanePresetPicker />
        </Show>
        <PaneGrid />
      </div>
      <QuickLaunch />
    </main>
  );
}
