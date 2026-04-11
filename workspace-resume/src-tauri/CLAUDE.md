# Tauri backend — `workspace-resume/src-tauri/`

Rust backend for the desktop app. Tauri 2 + axum 0.8 + tokio (full). Target is forced to `x86_64-pc-windows-msvc` via `.cargo/config.toml`, so `cargo check` does not run from WSL without a cross toolchain — all compilation happens on Windows.

## Architecture

```
┌──── WSL2 Ubuntu ──────────────────────────────────────────┐
│                                                            │
│  tmux session "main"                                       │
│  ├── main:1.1  claude (running)                            │
│  ├── main:2.1  claude (idle)                               │
│                                                            │
│  Claude Code                                               │
│  ~/.claude/settings.json  Notification hook →              │
│     curl http://100.110.47.29:8833/api/v1/hooks/notification│
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
│  pane-management-mobile (APK) — Kotlin + Compose, Ktor     │
└────────────────────────────────────────────────────────────┘
```

## Module tree

```
src/
├── lib.rs                        Tauri Builder + invoke_handler! (~60 commands) + companion::spawn() in .setup()
├── main.rs                       calls workspace_resume_lib::run()
├── commands/                     #[tauri::command] IPC targets — return Result<T, String>
│   ├── tmux.rs                   run_tmux_command() helper shells wsl.exe -e bash -c; 14 unit tests
│   ├── project_meta.rs           tier / display_name / pane_assignments / pane_presets store
│   ├── launcher.rs               resume_session, SessionTracker, terminal settings, error log
│   ├── discovery.rs              list_projects, list_sessions, scans WSL + Windows .claude/projects
│   ├── companion_admin.rs        get_companion_config, get_companion_qr, rotate_token
│   ├── usage.rs                  get_project_usage / get_all_usage / get_usage_summary
│   └── git.rs                    thin delegators to services::git
├── services/                     shared logic for commands + companion
│   ├── wsl.rs                    OnceLock-cached distro+user detection; UTF-16LE decode of wsl.exe --status
│   ├── usage.rs                  JSONL → tokens + USD cost (Anthropic pricing table, will drift)
│   ├── git.rs                    batched wsl.exe probe (one call walks many project paths)
│   ├── scanner.rs                session metadata parser
│   ├── watcher.rs                notify crate → session-changed broadcast
│   ├── path_decoder.rs           cwd extractor from JSONL first record (known upstream test failure)
│   └── terminal/{mod,warp,tmux,powershell}.rs
├── companion/                    embedded HTTP+WS API on 0.0.0.0:8833
│   ├── mod.rs                    spawn(app) entry — PORT=8833, BIND="0.0.0.0"
│   ├── http.rs                   axum Router; three sub-routers (api_auth/api_public/api_hooks) + ntfy
│   ├── state.rs                  AppState, PaneRecord, PendingApproval, NtfyMessage, load_or_init
│   ├── auth.rs                   bearer_mw (Authorization header OR ?token= query), hook_secret_mw (X-Hook-Secret)
│   ├── models.rs                 PaneDto / SessionDto / ApprovalDto / EventDto (tagged union) + PaneState enum
│   ├── tmux_poller.rs            2s loop; SHA-256 hash diff over capture-pane -p -t <id> -S -5
│   ├── hook_sink.rs              Claude Code notification hook park-and-wait on oneshot (120s TTL)
│   ├── ntfy_server.rs            embedded ntfy wire: POST /:topic, GET /:topic/json SSE
│   ├── ws.rs                     WebSocket upgrade + snapshot-on-connect + broadcast fanout
│   └── error.rs                  AppError + IntoResponse — only file using thiserror
└── models/                       shared serde structs consumed by commands + services
```

## Runtime WSL detection

`services/wsl.rs` uses `OnceLock<Option<WslInfo>>` with `get_or_init(detect)`. Do not hardcode `Ubuntu` or `andrea` — call `services::wsl::wsl_info()`. Key quirks:

- `wsl.exe --status` emits **UTF-16LE with BOM**; `wsl.exe -d <distro> -e whoami` emits **UTF-8**. Use `decode_utf16_lossy` for the former; never `String::from_utf8_lossy` on `--status` output
- Result is cached for the lifetime of the process

## Process spawning on Windows

Every `std::process::Command` — especially `wsl.exe`, `powershell.exe`, `taskkill.exe` — must set `creation_flags(0x08000000)` (CREATE_NO_WINDOW), or a `cmd` window flashes on every invocation. See `.claude/rules/wsl-process-spawn.md`.

`commands/tmux.rs::run_tmux_command` is the canonical wsl.exe wrapper and is reused across layers — even `companion/http.rs` and `companion/tmux_poller.rs` call `crate::commands::tmux::run_tmux_command` directly. This is the one place where "services → commands" is inverted and it's deliberate.

## Companion router structure

`companion/http.rs::router` has three sub-routers plus ntfy. Adding an endpoint is a rule (`.claude/rules/companion-endpoints.md`); pick the right one:

- `api_auth` (bearer via `bearer_mw`) — `/sessions`, `/panes`, `/panes/{id}/{capture,input,voice,cancel}`, `/approvals`, `/approvals/{id}`, `/usage`, `/usage/projects/{encoded_name}`, `/events` (WS)
- `api_public` — `/api/v1/health` only
- `api_hooks` (shared secret via `hook_secret_mw`) — `/api/v1/hooks/notification` only
- ntfy mounted at root: `/{topic}` (POST) and `/{topic}/json` (GET SSE)

Secret comparison is always `subtle::ConstantTimeEq::ct_eq`. Bearer accepts `?token=` query param because browsers can't set `Authorization` on WS handshakes (Android client uses this path in `CompanionClient.kt:133`).

## tmux_poller state machine

2-second loop. Format string: `'#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_current_command}|#{pane_current_path}|#{pane_pid}|#{pane_start_command}'`. Per-pane `tmux capture-pane -p -t <id> -S -5` for the preview, SHA-256 over the output for change detection. Idle timeout 3s.

Pipe-delimited parsing uses the shared `parse_lines<T, F>` helper in `commands/tmux.rs:84`. Last-field join: `parts[n..].join("|")` so fields that can contain literal `|` (current_path, pane_start_command) don't get truncated.

## Error handling dialects

Three Rust error dialects, one per layer — do not mix (`.claude/rules/error-handling-dialects.md`):

- `commands/*.rs` → `Result<T, String>` with `.map_err(|e| format!(...))` — required by Tauri IPC
- `companion/*.rs` handlers → `Result<_, AppError>` using the `thiserror` enum in `companion/error.rs`
- `companion::spawn`, `AppState::load_or_init`, setup → `anyhow::Result<T>`

## Tauri store read-modify-write

`C:\Users\Andrea\AppData\Roaming\com.pane-management.app\settings.json` has 8+ unrelated top-level keys: `companion.bearer_token`, `companion.hook_secret`, `companion.ntfy_topic`, `project_meta`, `pane_presets`, `pane_assignments`, `session_order`, `pinned_order`. Always `store.get(key)` → mutate → `store.set(key, ...)` → `store.save()`. Never build one struct and overwrite the store — you will wipe unrelated keys.

## Logging

Mixed dialects by module (de-facto rule, not aspirational):

- `companion/*` uses `tracing::{info, debug, warn}` (6 call sites across 4 files)
- `commands/*` and `services/*` use `eprintln!("[module] ...")` with a bracketed module prefix (18 call sites)

New `companion/` code uses `tracing`; new `commands/`/`services/` code uses bracketed `eprintln!`.

## Secrets

Generated on first launch in `companion::state::load_or_init` via `rand::thread_rng().fill_bytes(&mut [0u8; 32])`, stored in the Tauri store. Surfaced to the UI via `get_companion_config` and `get_companion_qr`. Never hardcode, never log, never commit.

## Known upstream test failures

`src/services/path_decoder.rs:42` `test_extract_cwd_from_valid_jsonl` and `src/services/terminal/warp.rs:216` `test_build_uri_simple_path` hardcode `Sky` (upstream's username). They are not ours — do not "fix" them without explicit approval.

## DTO serde conventions

- Optional fields: `#[serde(skip_serializing_if = "Option::is_none")]`
- State enums: `#[serde(rename_all = "lowercase")]`
- Tagged event enums: `#[serde(tag = "type", rename_all = "snake_case")]`
- Default-true booleans: a free `fn default_submit() -> bool { true }` + `#[serde(default = "default_submit")]`. Plain `#[serde(default)]` would yield `false`
- `companion/models.rs::now_ms()` is the only wall-clock source for wire timestamps

Any DTO change here requires a matching update in `pane-management-mobile/app/src/main/java/com/andreacanes/panemgmt/data/models/Dtos.kt` — see `.claude/rules/android-dto-contract.md`.
