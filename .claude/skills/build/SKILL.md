---
name: build
description: >
  Run the edit → sync → build → launch dance for the Tauri desktop app or the nested
  Android companion. Use when the user says "build", "rebuild", "sync and build",
  "tauri build", "android build", "check rust", "check frontend", or asks to verify
  recent edits compile. Handles sync.sh, killing the running exe before a Rust rebuild,
  and scope detection (rust-only vs frontend-only vs mobile-only vs full).
allowed-tools:
  - Bash
  - Read
  - Glob
---

# Build dance

The project source-of-truth lives in WSL at `/home/andrea/pane-management/`. Builds run on Windows against a rsync'd scratch at `C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\`. Every build starts with `sync.sh` or you are building stale source.

## Detect scope from the edit

Before picking a build command, figure out which subtree was touched:

| Edited path | Scope | Fastest check |
|---|---|---|
| `workspace-resume/src-tauri/**/*.rs` or `Cargo.toml` | Rust-only | `cargo check` |
| `workspace-resume/src/**` or `package.json` | Frontend-only | `npm run build` |
| `pane-management-mobile/**` | Mobile-only | `gradlew.bat assembleDebug` |
| Both Rust and frontend | Full Tauri | `npm run tauri build` |
| `workspace-resume/src-tauri/src/companion/models.rs` AND `pane-management-mobile/...Dtos.kt` | DTO change | Build both sides |

If multiple scopes are touched, build each narrow scope in parallel rather than jumping to `tauri build` — it is 10× slower and usually unnecessary.

## Tauri Rust check (narrow)

```bash
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\workspace-resume\src-tauri && cargo check"
```

**Do not use `cargo build --release` to produce the launchable exe** even when only Rust changed. Bare cargo skips the Tauri CLI step that switches the asset resolver from `devUrl` (localhost:1420) to the embedded `frontendDist`, so the exe boots in dev mode pointing at a Vite server that isn't running and the WebView shows `ERR_CONNECTION_REFUSED`. For a runnable exe always use the full Tauri release build below.

## Tauri frontend check (narrow)

```bash
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\workspace-resume && npm run build"
```

## Full Tauri release build (produces .exe + MSI + NSIS)

The exe is locked while running. Kill it first:

```bash
cmd.exe /c "taskkill /IM workspace-resume.exe /F" 2>/dev/null || true
/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\workspace-resume && npm run tauri build"
```

Launch the built exe from WSL:

```bash
cd /mnt/c/Users/Andrea && nohup \
  "/mnt/c/Users/Andrea/Desktop/Botting/pane-management-v0.4.0/workspace-resume/src-tauri/target/x86_64-pc-windows-msvc/release/workspace-resume.exe" \
  > /tmp/pm.log 2>&1 & disown
```

## Android debug APK + install + launch

```bash
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe

/home/andrea/pane-management/sync.sh && \
  cmd.exe /c "cd /d C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\pane-management-mobile && gradlew.bat assembleDebug" && \
$ADB install -r 'C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\pane-management-mobile\app\build\outputs\apk\debug\app-debug.apk' && \
$ADB shell am start -n com.andreacanes.panemgmt/.MainActivity
```

If `adb` says "device not found", restart: `$ADB kill-server && $ADB start-server` and check the phone for the re-authorize prompt.

Filter logcat by app PID only (SurfaceFlinger spam is noise):

```bash
$ADB logcat --pid=$($ADB shell pidof com.andreacanes.panemgmt)
```

## Common failure modes

- "my edit didn't take" → you forgot `sync.sh`
- "cargo check fails mysteriously" → you edited the scratch directly instead of WSL; it's been wiped. Re-edit in WSL, sync, try again
- "taskkill said 'process not found'" → fine, the exe wasn't running, proceed
- "`compileSdk 35` missing" → only `android-32` and `android-36` are installed; target is 36 intentionally
- "Tailscale timeout during smoke test" → see the `companion-smoke-test` skill; check `sc query Tailscale` first
- "WebView shows `ERR_CONNECTION_REFUSED` to localhost when the exe launches" → you ran `cargo build --release` instead of `npm run tauri build`. Bare cargo doesn't flip Tauri's asset resolver to the embedded dist. Re-run the full Tauri release build above
