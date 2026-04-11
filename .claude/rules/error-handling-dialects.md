---
description: Three Rust error-handling dialects — one per layer, do not mix
paths: ["workspace-resume/src-tauri/src/**/*.rs"]
---

- `commands/*.rs` → `Result<T, String>` with `.map_err(|e| format!("..."))`. Required because Tauri IPC only serializes strings across the bridge.
- `companion/*.rs` HTTP handlers → `Result<_, AppError>` using the `thiserror` enum in `companion/error.rs`. This is the only file in the crate that uses `thiserror`.
- `companion::spawn`, `AppState::load_or_init`, other setup code → `anyhow::Result<T>`.

Do not `anyhow` inside a Tauri command or a companion handler. Do not return `String` errors from companion handlers.
