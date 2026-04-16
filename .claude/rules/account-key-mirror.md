---
description: Account keys "andrea"/"bravura" are mirrored across 5 sites; the Rust registry is the source of truth
paths: ["workspace-resume/src-tauri/src/companion/accounts.rs", "workspace-resume/src/lib/account.ts", "pane-management-mobile/app/src/main/java/com/andreacanes/panemgmt/data/models/Dtos.kt", "pane-management-mobile/app/src/main/java/com/andreacanes/panemgmt/ui/theme/StatusColors.kt"]
---

Account keys live in `workspace-resume/src-tauri/src/companion/accounts.rs`. Adding or renaming an account requires a matching edit in all four mirror sites or the UI will silently fall back to a default badge:

1. `src-tauri/src/companion/accounts.rs` — registry + detection
2. `src/lib/account.ts` — TS label / colour / badge lookup
3. `pane-management-mobile/.../data/models/Dtos.kt` — wire DTO field / KDoc
4. `pane-management-mobile/.../ui/theme/StatusColors.kt` — Compose colour tokens

There is no compile-time check tying these together. Ship a dual-repo commit (outer + nested mobile). Full mirror table: see `workspace-resume/src-tauri/CLAUDE.md` §Accounts.
