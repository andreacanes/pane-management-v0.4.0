---
description: Tauri store has 8+ top-level keys; always read-modify-write per key, never overwrite the whole JSON
paths: ["workspace-resume/src-tauri/src/**/*.rs"]
---

`C:\Users\Andrea\AppData\Roaming\com.pane-management.app\settings.json` holds multiple unrelated top-level keys: `companion.bearer_token`, `companion.hook_secret`, `companion.ntfy_topic`, `project_meta`, `pane_presets`, `pane_assignments`, `session_order`, `pinned_order`, plus others.

Always: `store.get(key)` → mutate the value → `store.set(key, new_value)` → `store.save()`. Never serialize one big struct and overwrite the store — you will wipe unrelated keys. See `commands/companion_admin.rs` and `companion/state.rs::load_or_init` for the canonical pattern.
