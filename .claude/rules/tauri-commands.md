---
description: Tauri IPC commands require a four-file lockstep edit; casing rule is strict
paths: ["workspace-resume/src-tauri/src/commands/**/*.rs", "workspace-resume/src-tauri/src/lib.rs", "workspace-resume/src/lib/tauri-commands.ts", "workspace-resume/src/lib/types.ts"]
---

Adding or renaming one Tauri command touches four files. Miss any and it silently fails.

1. `src-tauri/src/commands/<domain>.rs` — `#[tauri::command] async fn` (async even if body is sync)
2. `src-tauri/src/lib.rs` — append to `tauri::generate_handler![...]` (~60 entries, grouped by domain)
3. `src/lib/tauri-commands.ts` — typed `invoke()` wrapper; Rust `snake_case` fn name passed as a string to `invoke()`, TS wrapper name is `camelCase`, args object uses `camelCase` keys (Tauri auto-converts)
4. `src/lib/types.ts` — DTO interface with `snake_case` field names matching serde output

Mixing casing (camelCase DTO fields or snake_case arg keys) produces silently-undefined fields with no type error. Full backend module guide: see `workspace-resume/src-tauri/CLAUDE.md`.
