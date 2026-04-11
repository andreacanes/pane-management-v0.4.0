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
│   ├── theme/                         FaeParticles, FaeSigils, FaeVines, NeonSigns
│   └── SettingsPanel.tsx              Terminal / Mobile companion / Error log
└── themes/
```

## Hard rules (break one = real bug)

- **Every Rust command gets a wrapper in `lib/tauri-commands.ts`** — no component imports `@tauri-apps/api/core` directly. Grep check: `invoke(` only appears in `tauri-commands.ts`. See `.claude/rules/tauri-commands.md` for the four-file lockstep edit
- **DTO interfaces in `lib/types.ts` use `snake_case` field names** to match serde output (`encoded_name`, `path_exists`, `session_count`). Do not camelCase them
- **`invoke()` arg objects use `camelCase` keys** because Tauri auto-converts them (`invoke("resume_session", { encodedProject, sessionId })`). Mixing this with snake_case produces silently-undefined fields
- **Store reads go through the Tauri store plugin**, not `localStorage`. See `lib/store-keys.ts` for the canonical key list

## SolidJS idioms

- `createSignal` for local state — signals are called as functions (`editing()`)
- `createStore` + `produce` + `reconcile` for global state in `AppContext` — store fields are accessed as properties (`state.projects`)
- `createResource` for async derived data
- Mutation flow: component → wrapper from `tauri-commands.ts` → refresh via `useApp()` (`refreshProjects`/`refreshTmuxState`). Components never re-fetch directly
- Error idiom: `catch (e) { setError(e instanceof Error ? e.message : String(e)) }` + `<Show when={error()}>`

## Styling

- Plain CSS class strings only. No CSS modules, no styled-components, no `clsx`/`cn` helpers
- Inline `style={}` only for one-off dynamic values
- Tailwind v4 config is in-CSS, not `tailwind.config.js`

## Logging

`console.log` is a prune target, not an add target — upstream left ~40 of them scattered. New code should not add to the count. If you need tracing, add it on the Rust side and read the result.

## Known inconsistency

`CompanionConfig` is defined inline in `lib/tauri-commands.ts:98-105` instead of in `lib/types.ts`. Leave it alone unless you're deliberately fixing it in the same PR as a related change.
