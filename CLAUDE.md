# Pane Management

Two tightly-coupled repos that let Andrea control Claude Code sessions across tmux from a Windows desktop app and an Android phone, with everything routed over Tailscale — **no cloud APIs**.

## Source-of-truth layout

**Canonical source lives on WSL**: `/home/andrea/pane-management/`
**Build scratch lives on Windows**: `C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\`

Why the split: Tauri (MSVC) and Android (Gradle) toolchains must run on Windows. Cross-filesystem builds from WSL via `\\wsl.localhost\` are 3–5× slower and prone to file-lock issues. Source-of-truth lives in WSL for fast git / rg / editing; the Windows scratch is a disposable mirror that holds the persistent `target/`, `node_modules/`, `.gradle/` caches so cargo/gradle get fast incremental builds.

**Sync direction is always WSL → scratch**, never the reverse. The scratch's `.git` directories have been deliberately removed to prevent accidental commits; all git operations (commit / push / pull / rebase) run from `/home/andrea/pane-management/`.

To sync before a build:
```bash
/home/andrea/pane-management/sync.sh
```
Takes ~1–3s typically. `rm -rf` the scratch anytime — next sync rebuilds it, and the first build after a clean will be cold.

## Repos

Both repos are nested in the WSL source-of-truth directory. They track independent git history.

| Path (WSL) | Git origin | Purpose |
|---|---|---|
| `/home/andrea/pane-management/` | `andreacanes/pane-management-v0.4.0` (fork of `sky-salsa/pane-management-v0.4.0`) | Tauri 2 + SolidJS + Rust desktop app. Runs on Windows, talks to WSL tmux via `wsl.exe`. Embeds the companion HTTP/WebSocket API on port 8833. |
| `/home/andrea/pane-management/pane-management-mobile/` | `andreacanes/pane-management-mobile` | Kotlin + Jetpack Compose + Ktor Android app. Connects to the companion over Tailscale (or ADB reverse tunnel for dev). Voice input via Android `SpeechRecognizer`. |

The mobile repo is nested here (ignored by the outer git via `.gitignore`) so both projects can be worked on from one Claude Code session — they share DTOs, the API contract, and pricing/state-machine logic.

## Architecture

```
┌──── WSL2 Ubuntu ──────────────────────────────────────────┐
│                                                            │
│  tmux session "main"                                       │
│  ├── main:1.1  claude (running)                            │
│  ├── main:2.1  claude (idle)                               │
│  └── ...                                                   │
│                                                            │
│  Claude Code                                               │
│  ~/.claude/settings.json  Notification hook →              │
│     curl http://100.110.47.29:8833/api/v1/hooks/notification
│                                                            │
└─────────────────────────▲──────────────────────────────────┘
                          │ wsl.exe
┌─────────────────────────┴──── Windows ─────────────────────┐
│                                                            │
│  workspace-resume.exe (Tauri)                              │
│  ├── SolidJS UI: project cards, pane grid, settings        │
│  └── Rust: companion service bound 0.0.0.0:8833            │
│      ├── tmux_poller (2s loop)                             │
│      ├── HTTP /api/v1/* + WS /events + ntfy server         │
│      └── hook_sink park-and-wait                           │
│                                                            │
└─────────────────────────▲──────────────────────────────────┘
                          │ Tailscale
┌─────────────────────────┴──── Phone (oneplus-13) ──────────┐
│  pane-management-mobile (APK)                              │
│  Kotlin + Compose, Ktor → :8833                            │
└────────────────────────────────────────────────────────────┘
```

## Environment assumptions

The fork is personalized for Andrea's single machine. Things that are **hardcoded for this environment** but detected at runtime wherever possible:

- **WSL2 mirrored networking mode** (`/etc/wsl.conf` → `[wsl2] networkingMode=mirrored`). `localhost` from WSL does NOT reach Windows — always use the Tailscale IP for WSL → Windows cross-talk.
- **WSL distro/user**: detected at runtime via `wsl.exe --status` + `whoami`. Do not hardcode `Ubuntu` or `andrea` — use `services::wsl::wsl_info()` which parses the UTF-16 output and caches in a `OnceLock`.
- **Tailscale Windows tailnet**: `desktop-ovgmai7` = `100.110.47.29`. Phone `oneplus-13` = `100.83.163.105`. Android build tools at `C:\Users\Andrea\AppData\Local\Android\Sdk\`. JDK 21 at `C:\Program Files\Java\jdk-21\`.
- **Rust toolchain on Windows**: `stable-x86_64-pc-windows-msvc` at `C:\Users\Andrea\.cargo\bin\`.
- **Node on Windows**: `C:\Program Files\nodejs\`.

## Code layout

```
workspace-resume/                        ← Tauri project root
├── src-tauri/
│   ├── Cargo.toml                       ← Rust deps (axum, tower-http, tokio full,
│   │                                       serde, uuid, base64, reqwest, notify,
│   │                                       qrcode, sha2, chrono, ...)
│   └── src/
│       ├── lib.rs                       ← Tauri Builder + companion::spawn() in .setup()
│       ├── commands/                    ← Tauri IPC commands (invoke targets)
│       │   ├── discovery.rs             ← list_projects, scans WSL+Windows .claude/projects
│       │   ├── launcher.rs              ← resume_session + terminal settings
│       │   ├── tmux.rs                  ← list_tmux_*, create_pane, send_keys,
│       │   │                              list_active_claude_panes
│       │   ├── project_meta.rs          ← tier/display_name/pane_assignments store
│       │   ├── usage.rs                 ← get_project_usage, get_all_usage, summary
│       │   ├── git.rs                   ← get_git_info, create_worktree
│       │   └── companion_admin.rs       ← get_companion_config, qr, rotate_token
│       ├── services/                    ← Logic shared by commands + companion
│       │   ├── wsl.rs                   ← Distro/user detection (OnceLock cached)
│       │   ├── scanner.rs               ← Session metadata parser
│       │   ├── path_decoder.rs          ← Extract cwd from JSONL first record
│       │   ├── watcher.rs               ← notify-based session-changed events
│       │   ├── usage.rs                 ← JSONL → tokens + USD cost (Anthropic pricing)
│       │   ├── git.rs                   ← Batched wsl.exe probe (branches + worktrees)
│       │   └── terminal/                ← TmuxLauncher / PowerShellLauncher / WarpLauncher
│       ├── companion/                   ← Embedded HTTP+WS API on :8833
│       │   ├── mod.rs                   ← spawn() entry, tokio::spawn from Tauri setup
│       │   ├── http.rs                  ← axum Router + AppState wiring
│       │   ├── state.rs                 ← PaneRecord, approvals, broadcast channels
│       │   ├── auth.rs                  ← bearer_mw + hook_secret_mw (subtle::ct_eq)
│       │   ├── ws.rs                    ← WebSocket event stream
│       │   ├── hook_sink.rs             ← Notification hook park-and-wait
│       │   ├── ntfy_server.rs           ← Embedded ntfy wire protocol (POST /:topic, GET /:topic/json)
│       │   ├── tmux_poller.rs           ← 2s loop scraping pane state via SHA-256 hash diff
│       │   └── models.rs                ← DTOs (PaneDto, SessionDto, ApprovalDto, EventDto, ...)
│       └── models/                      ← Shared Serde structs
│
├── src/                                 ← SolidJS frontend (Tauri webview)
│   ├── App.tsx, index.tsx
│   ├── lib/
│   │   ├── tauri-commands.ts            ← invoke() wrappers. New commands go here too.
│   │   ├── types.ts                     ← ProjectInfo, PaneDto, EventDto, ...
│   │   └── launch.ts                    ← launchToPane / newSessionInPane
│   ├── contexts/AppContext.tsx          ← Global state, polling, pane assignments
│   └── components/
│       ├── layout/
│       │   ├── TopBar.tsx               ← Session tabs, window tabs, ⚙/✵ buttons
│       │   ├── Sidebar.tsx, MainArea.tsx
│       │   └── GlobalActivePanel.tsx    ← "All active Claudes" modal
│       ├── project/ProjectCard.tsx      ← Cards with usage + git branch + worktree button
│       ├── pane/PaneSlot.tsx            ← Individual tmux pane UI
│       └── SettingsPanel.tsx            ← Terminal / Mobile companion / Error log
│
└── package.json                         ← vite-solid build → dist/, consumed by Tauri

pane-management-mobile/                  ← Android repo (nested, separate git)
├── app/src/main/java/com/andreacanes/panemgmt/
│   ├── MainActivity.kt, PaneMgmtApp.kt (NavHost)
│   ├── data/
│   │   ├── CompanionClient.kt           ← Ktor HTTP + WebSocket client
│   │   ├── AuthStore.kt                 ← DataStore Preferences (URL + bearer)
│   │   └── models/Dtos.kt                ← Must match Rust companion wire format
│   ├── ui/
│   │   ├── setup/SetupScreen.kt          ← URL + bearer, test, save
│   │   ├── grid/PaneGridScreen.kt        ← LazyColumn of pane cards, live via WS
│   │   └── detail/PaneDetailScreen.kt    ← Capture + input + mic + approval dialog
│   └── voice/VoiceInputController.kt     ← SpeechRecognizer wrapper (offline preferred)
├── gradle/libs.versions.toml             ← Kotlin 2.1, Compose BOM 2025.01.01, Ktor 3
└── app/build.gradle.kts                  ← compileSdk 36, minSdk 26, JDK 21
```

## Build / run / test

All edits happen in WSL (`/home/andrea/pane-management/`). Before any build, run `sync.sh` to refresh the Windows scratch. Builds then run native on Windows where the toolchains live.

### Desktop (Tauri)
```bash
# Sync + full release build — produces workspace-resume.exe + MSI + NSIS
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\workspace-resume && npm run tauri build"

# Sync + fast Rust check only (no bundle)
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\workspace-resume\src-tauri && cargo check"

# Sync + fast frontend check
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\workspace-resume && npm run build"

# Kill a running instance before rebuilding (required — exe is locked while running)
cmd.exe /c "taskkill /IM workspace-resume.exe /F"

# Run the built exe from WSL
cd /mnt/c/Users/Andrea && nohup \
  "/mnt/c/Users/Andrea/Desktop/Botting/pane-management-v0.4.0/workspace-resume/src-tauri/target/x86_64-pc-windows-msvc/release/workspace-resume.exe" \
  > /tmp/pm.log 2>&1 & disown
```

### Android app
```bash
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe

# Sync + build debug APK
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\pane-management-mobile && gradlew.bat assembleDebug"

# Install + launch
$ADB install -r 'C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\pane-management-mobile\app\build\outputs\apk\debug\app-debug.apk'
$ADB shell am start -n com.andreacanes.panemgmt/.MainActivity

# App URL options:
#   Tailscale (no tether needed):  http://100.110.47.29:8833
#   ADB reverse tunnel (dev):      $ADB reverse tcp:8833 tcp:8833 ; then http://localhost:8833
```

### git from WSL (never from the scratch)
```bash
# The scratch has no .git directories on purpose.
# All git operations run from /home/andrea/pane-management/ or the nested
# /home/andrea/pane-management/pane-management-mobile/.
cd /home/andrea/pane-management && git status
cd /home/andrea/pane-management/pane-management-mobile && git push
```

### Smoke-test the companion from WSL
```bash
TOK=$(cat /mnt/c/Users/Andrea/AppData/Roaming/com.pane-management.app/settings.json \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['companion.bearer_token'])")
curl -sS http://100.110.47.29:8833/api/v1/health
curl -sS -H "Authorization: Bearer $TOK" http://100.110.47.29:8833/api/v1/panes
curl -sS -H "Authorization: Bearer $TOK" http://100.110.47.29:8833/api/v1/usage
```

`localhost` from WSL does **not** reach the Windows-bound companion because of WSL2 mirrored networking — always use the Tailscale IP or the LAN IP.

## Secrets

**Never hardcode.** They're generated on first companion startup and stored in the Tauri store at:
```
C:\Users\Andrea\AppData\Roaming\com.pane-management.app\settings.json
```

Keys:
- `companion.bearer_token` — 32 bytes base64url, required on all `/api/v1/*` calls
- `companion.hook_secret` — separate shared secret for the Claude Code Notification hook (`X-Hook-Secret` header), kept out of `~/.claude/settings.json`'s public-ish config
- `companion.ntfy_topic` — random topic name for the embedded ntfy server

To surface them in the app, read via the `get_companion_config` Tauri command (used by the Settings panel and QR generator). Do not put them in log lines or commit them.

## Conventions

- **Commit style**: `feat:` / `fix:` / `refactor:` / `chore:` / `test:` with a single-sentence subject and a body that explains *why*. Bundle related changes into one commit; don't split for split's sake. Always co-author trailer:
  ```
  Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
  ```
- **Tauri commands**: one file per domain under `src-tauri/src/commands/`. Register in `lib.rs` inside `invoke_handler![...]`. Expose via `src/lib/tauri-commands.ts` wrapper **and** a matching TypeScript interface in `src/lib/types.ts`.
- **Companion endpoints**: add to `companion/http.rs` under `api_auth` (bearer-protected), `api_public` (no auth), or `api_hooks` (shared-secret). If the endpoint needs state, put it in `AppState` (`state.rs`). Mirror the DTO in the Android app's `data/models/Dtos.kt`.
- **Android DTOs**: any change to `companion/models.rs` needs a matching `@Serializable` update in `pane-management-mobile/app/src/main/java/com/andreacanes/panemgmt/data/models/Dtos.kt`. Serde is permissive (`ignoreUnknownKeys = true`) so adding fields is non-breaking, but renames are not.
- **Logging**: Rust side uses `tracing::{info, debug, warn}`. Frontend uses `console.log` sparingly — there are ~40 leftover from upstream we want to prune, not add to.
- **Don't** add dead tests or speculative abstractions. Don't add comments that restate code.
- **Don't** run `git push --force` or `--no-verify`. Always create new commits.

## Gotchas (don't re-learn)

1. **WSL2 mirrored networking**: `localhost` from WSL binds IPv6 loopback and is **invisible** to the Windows interface stack. Always use `100.110.47.29:8833` (Tailscale IP) for WSL → Windows calls. The Claude Code Notification hook **must** use the Tailscale IP, not `127.0.0.1`.
2. **Tailscale Windows service can silently stop**. Symptom: app times out, `adb shell curl` also times out. Check: `sc query Tailscale`. Fix: `net start Tailscale` (no admin needed for start; AUTO_START is set but failure-recovery policy needs admin).
3. **Android `run-as` drops network caps**. `adb shell run-as com.andreacanes.panemgmt curl ...` will fail network calls even though the real app works fine. Don't use `run-as` to debug connectivity — test via the actual app or `adb shell curl`.
4. **`input text` typing into the wrong field**. When the Android soft keyboard opens, layout compresses and button coordinates shift. If you `adb shell input tap` immediately after typing, you may hit the wrong button. Always dismiss the keyboard first (tap a non-input area), re-dump the UI to get current bounds, then tap.
5. **OnePlus "Writing Tools" popup** hijacks taps on input fields that look like AI-augmentable text. Dismiss with BACK before interacting.
6. **Tauri button bounds include neither icon nor label text** — they're an invisible `<View clickable=true>` at a different Y range from the `<Text>` inside. When finding button centers via `uiautomator dump`, filter `clickable=true` elements.
7. **Keyboard dismissal via BACK can also navigate back**. On the Setup screen, pressing BACK with no keyboard visible exits the activity. Prefer tapping a non-input area over BACK.
8. **ADB can drop the phone** mid-session (screen lock, USB power blip). Restart with `adb kill-server && adb start-server` and re-authorize on the phone if needed.
9. **SurfaceFlinger "out of order buffers" noise in logcat** is harmless. Filter by our PID: `adb logcat --pid=$(adb shell pidof com.andreacanes.panemgmt)`.
10. **Android `compileSdk 35` fails** on this box — only `android-32` and `android-36` are installed. We target 36. If you see `Platform SDK with path: platforms;android-35`, that's why.
11. **AGP 8.7.3 warns about compileSdk 36** — version-mismatch warning, not an error. Suppress with `android.suppressUnsupportedCompileSdk=36` in `gradle.properties` if it gets noisy.
12. **Pre-existing test failures** in the Rust side: `test_extract_cwd_from_valid_jsonl` and `test_build_uri_simple_path` hardcode `Sky` (upstream's username). These are **not ours** — don't "fix" them without user approval.
13. **Tauri store writes lose unrelated keys** if you overwrite the whole object. Always read-modify-write with `StoreExt::get` → mutate → `StoreExt::set` → `save`.
14. **Never edit files in the Windows scratch directly**. The scratch is output-only from `sync.sh`'s perspective — next sync's `--delete` will wipe any edits you made there. Always edit in `/home/andrea/pane-management/`.
15. **Never `git` in the Windows scratch**. Its `.git` dirs have been deliberately removed so accidental commits aren't even possible. If you somehow recreate them, delete again; canonical git lives in WSL only.
16. **Forgetting to run `sync.sh`** before a build = you're building stale source. Symptom: your edit "didn't take". The iteration loop is `edit → sync.sh → build`, not `edit → build`.

## Current state (April 2026)

**Working end-to-end**:
- Discovery, project cards with usage + git branch, pane grid, session resume
- Companion HTTP/WS API on `0.0.0.0:8833`, state machine via tmux_poller
- Android app grid + detail + voice + approval overlay, connected over Tailscale
- Claude Code Notification hook wired, park-and-wait tested with curl
- Barkeep status line in both `~/.claude` configs

**Partially wired**:
- Android app doesn't show usage or git branch yet (DTOs + UI additions needed)
- ntfy F-Droid app not installed on phone → lock-screen approval push path exists but untested
- JSONL-based state transitions (`tool_use` / `stop_reason`) — currently only the output-hash path is used

**Explicitly deferred** (on the roadmap, not in scope unless requested):
- B5: pre-resume compaction (needs `claude --compact` flag verification)
- B6: destructive-command safety gate (needs pattern matcher)
- Scored Claude-session-id resolver (PID + cwd fallback)
- ANSI color preservation in Android pane capture view
- Release signing + APK distribution

## Primary references

- **Plan file** (session roadmap): `~/.claude-b/plans/fluttering-jumping-kahn.md`
- **Research reports** (background context): `~/.claude-b/plans/fluttering-jumping-kahn-agent-*.md`
- **Auto-memory** for this workspace: `~/.claude-b/projects/-home-andrea-pane-management/memory/`
- **Upstream design docs** (kept for reference): `BACKLOG.md`, `DEPENDENCIES.md`, `SETUP-GUIDE.md`, `PORTABILITY-AUDIT.md`
