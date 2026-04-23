# Tauri frontend — `workspace-resume/src/`

SolidJS 1.9 + TypeScript + Tailwind v4 (CSS-in-JS config, no `tailwind.config.js`). Build: Vite 6 + `vite-plugin-solid` + `@tailwindcss/vite`. Dev server on `1420` / HMR `1421`. No test framework, no eslint, no typecheck script — type errors only surface during `vite build`.

## Directory map

```
src/
├── App.tsx                            top-level shell
├── index.tsx                          render entry
├── lib/
│   ├── tauri-commands.ts              invoke() wrappers (one per Rust command)
│   ├── types.ts                       DTO interfaces with snake_case fields
│   ├── launch.ts                      launchToPane / newSessionInPane
│   ├── account.ts                     Andrea/Bravura label + colour lookup (mirrors src-tauri accounts.rs)
│   ├── path.ts, store-keys.ts, time.ts
│   └── solid-dnd.d.ts
├── contexts/AppContext.tsx            743 lines — global store, polling, refresh authority
├── components/
│   ├── layout/
│   │   ├── TopBar.tsx                 session tabs, window tabs, ⚙/✵ buttons
│   │   ├── Sidebar.tsx, MainArea.tsx
│   │   ├── GlobalActivePanel.tsx      "all active Claudes" modal
│   │   └── QuickLaunch.tsx
│   ├── pane/
│   │   ├── PaneGrid.tsx, PaneSlot.tsx, PanePresetPicker.tsx
│   ├── project/
│   │   ├── ProjectCard.tsx            canonical component idioms
│   │   ├── ProjectDetailModal.tsx, NewProjectFlow.tsx, SessionList.tsx, SessionItem.tsx
│   ├── ui/                            Button, Card, Badge, StatusChip, icons (Lucide)
│   └── SettingsPanel.tsx              Terminal / Mobile companion / Error log
```

## Hard rules (break one = real bug)

- **Every Rust command gets a wrapper in `lib/tauri-commands.ts`** — no component imports `@tauri-apps/api/core` directly. Grep check: `invoke(` only appears in `tauri-commands.ts`. See `.claude/rules/tauri-commands.md` for the four-file lockstep edit
- **DTO interfaces in `lib/types.ts` use `snake_case` field names** to match serde output (`encoded_name`, `path_exists`, `session_count`). Do not camelCase them
- **`invoke()` arg objects use `camelCase` keys** because Tauri auto-converts them (`invoke("launch_in_pane", { session, window, pane, projectPath })`). Mixing this with snake_case produces silently-undefined fields
- **Store reads go through the Tauri store plugin**, not `localStorage`. See `lib/store-keys.ts` for the canonical key list

## SolidJS idioms

- `createSignal` for local state — signals are called as functions (`editing()`)
- `createStore` + `produce` + `reconcile` for global state in `AppContext` — store fields are accessed as properties (`state.projects`)
- `reconcile` calls for project arrays MUST pass `{ key: "encoded_name" }` (e.g. `AppContext.tsx:161`). Naive `setState` regresses `getProjectUsage` perf because the store loses stable identity and refetches per card
- `createResource` for async derived data
- Mutation flow: component → wrapper from `tauri-commands.ts` → refresh via `useApp()` (`refreshProjects`/`refreshTmuxState`). Components never re-fetch directly
- Error idiom: `catch (e) { setError(e instanceof Error ? e.message : String(e)) }` + `<Show when={error()}>`
- Error-log convention: `console.error("[<Component>] <action> error:", e)` with the bracketed component prefix (12 call sites)
- Usage data is expensive — never `await getProjectUsage` on a hot path (poll / render). Let `AppContext` own the cadence

## Styling

- Plain CSS class strings only. No CSS modules, no styled-components, no `clsx`/`cn` helpers
- Inline `style={}` only for one-off dynamic values
- Tailwind v4 config is in-CSS, not `tailwind.config.js`
- Lucide icons are re-exported through `components/ui/icons.ts` — import from there, never from `lucide-solid` directly

## Account labels

Account-key literals `"andrea"` / `"bravura"` in UI code must read through `lib/account.ts` (label, colour, badge). The canonical registry is Rust-side in `workspace-resume/src-tauri/src/companion/accounts.rs`. See `.claude/rules/account-key-mirror.md`.

## Logging

`console.log` is a prune target, not an add target — upstream left ~40 of them scattered. New code should not add to the count. If you need tracing, add it on the Rust side and read the result.

