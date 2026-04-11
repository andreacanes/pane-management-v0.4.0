---
description: Companion endpoints go in exactly one of three routers; auth compares are constant-time
paths: ["workspace-resume/src-tauri/src/companion/**/*.rs"]
---

`companion/http.rs::router` has three sub-routers plus ntfy. Put the new endpoint in the right one:

- `api_auth` (bearer via `auth::bearer_mw`) — all mobile API endpoints
- `api_public` (no auth) — `/api/v1/health` only
- `api_hooks` (shared secret via `auth::hook_secret_mw`) — Claude Code Notification hook only
- `/{topic}` and `/{topic}/json` — ntfy wire, topic randomness is the only secret

Secret comparison is always `subtle::ConstantTimeEq::ct_eq`, never `==`. Bearer tokens accept a `?token=` query param because browsers cannot set `Authorization` on WS handshakes. Shared mutable state lives in `AppState` (`state.rs`). Full companion module guide: see `workspace-resume/src-tauri/CLAUDE.md`.
