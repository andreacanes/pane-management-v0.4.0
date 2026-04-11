---
name: companion-smoke-test
description: >
  Smoke-test the embedded companion HTTP API by reading the bearer token from the
  Tauri store and probing /api/v1/health, /panes, and /usage. Use when the user says
  "smoke test", "check the companion", "is the companion alive", "test the API",
  or when they report that the Android app can't connect. Diagnoses Tailscale service
  health before blaming code.
allowed-tools:
  - Bash
  - Read
---

# Companion smoke test

The companion runs on `0.0.0.0:8833` on Windows. From WSL, it must be reached via the Tailscale IP `100.110.47.29` — NOT `localhost`, because WSL2 mirrored networking hides Windows `localhost`.

## Step 1 — check Tailscale is up first

Before blaming code: the Windows Tailscale service can silently stop. Check it first.

```bash
cmd.exe /c "sc query Tailscale" 2>&1 | head -20
```

Expected: `STATE : 4 RUNNING`. If `STOPPED`, start it with `cmd.exe /c "net start Tailscale"` (non-admin start is permitted).

## Step 2 — read bearer from the Tauri store

The token is generated on first launch and lives in the Tauri store. Read it without spawning python every time:

```bash
TOK=$(cat /mnt/c/Users/Andrea/AppData/Roaming/com.pane-management.app/settings.json \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['companion.bearer_token'])")
```

If the file doesn't exist, the desktop app has never been run — launch it first with the `build` skill.

## Step 3 — probe the three endpoints

```bash
curl -sS -m 5 http://100.110.47.29:8833/api/v1/health
curl -sS -m 5 -H "Authorization: Bearer $TOK" http://100.110.47.29:8833/api/v1/panes
curl -sS -m 5 -H "Authorization: Bearer $TOK" http://100.110.47.29:8833/api/v1/usage
```

Expected responses:

- `/health` → `{"status":"ok","version":"0.4.0","uptime_s":N}` (public, no auth)
- `/panes` → `{"panes":[...]}` — empty array is OK if tmux has no panes
- `/usage` → `{"projects":[...],"summary":{...}}`

## Step 4 — interpret failures

| Symptom | Cause | Fix |
|---|---|---|
| curl timeout on `/health` | Tailscale service down, or app not running | Check `sc query Tailscale`, then check `taskkill /IM workspace-resume.exe` returns "not found" (= not running) |
| 401 on `/panes` | Bearer is wrong; store file was wiped and regenerated | Re-read `$TOK` after the app regenerates |
| 401 on `/health` | Never happens — `/health` is in `api_public` | If you see this, `companion/http.rs` was misedited |
| Empty `panes[]` but tmux has sessions | `tmux_poller` hasn't ticked yet (2s interval) or tmux socket isn't reachable from Windows | Retry after 3s; then check `commands/tmux.rs::run_tmux_command` |
| `/usage` returns stale numbers | Expected — usage is scanned every refresh, not live-streamed |

## Step 5 — optional hook probe

The Notification hook has its own secret and lives on `/api/v1/hooks/notification`:

```bash
HOOK=$(cat /mnt/c/Users/Andrea/AppData/Roaming/com.pane-management.app/settings.json \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['companion.hook_secret'])")
curl -sS -m 5 -X POST \
  -H "X-Hook-Secret: $HOOK" \
  -H "Content-Type: application/json" \
  -d '{"session_id":"test","cwd":"/tmp","message":"smoke"}' \
  http://100.110.47.29:8833/api/v1/hooks/notification
```

This parks for up to 120s waiting for a resolution — don't leave it running unless you mean to.
