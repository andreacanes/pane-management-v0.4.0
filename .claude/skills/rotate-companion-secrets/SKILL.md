---
name: rotate-companion-secrets
description: >
  Rotate the companion bearer token, hook secret, and ntfy topic in the Tauri store,
  propagate the new hook secret to ~/.claude/settings.json so the Notification hook
  keeps authenticating, and resurface the QR for the phone. Use when the user says
  "rotate bearer", "rotate companion secrets", "new token", "regen secret", or after
  a suspected token leak.
allowed-tools:
  - Bash
  - Read
  - Edit
---

# Rotate companion secrets

The companion generates three independent secrets on first launch and stores them in the Tauri store at `C:\Users\Andrea\AppData\Roaming\com.pane-management.app\settings.json`. The hook secret is mirrored in plaintext in `~/.claude/settings.json` (Notification hook). If the store rotates and `~/.claude/settings.json` doesn't, the hook silently 401s.

## Secret locations

| Key | Store (Tauri) | Mirror | Consumer |
|---|---|---|---|
| `companion.bearer_token` | primary | phone DataStore (via QR) | Mobile API calls |
| `companion.hook_secret` | primary | `~/.claude/settings.json` header | Claude Code Notification hook |
| `companion.ntfy_topic` | primary | phone ntfy subscription (via QR) | Lockscreen push |

## Step 1 — rotate via the Tauri IPC

The app exposes `rotate_companion_token`, `rotate_companion_hook_secret`, and `rotate_companion_ntfy_topic`. Either call them from the Settings panel in the UI, or hit the underlying code path by restarting with the store keys cleared. The preferred path is the UI — it redraws the QR automatically.

## Step 2 — read the new hook secret

```bash
HOOK=$(cat /mnt/c/Users/Andrea/AppData/Roaming/com.pane-management.app/settings.json \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['companion.hook_secret'])")
echo "$HOOK"
```

## Step 3 — propagate to ~/.claude/settings.json

The hook config lives in `~/.claude/settings.json` under `hooks.Notification` (and other hook arrays). Each entry POSTs to `http://100.110.47.29:8833/api/v1/hooks/notification` with header `X-Hook-Secret: <secret>`. Update every occurrence:

```bash
python3 - <<EOF
import json, pathlib
p = pathlib.Path.home() / ".claude" / "settings.json"
data = json.loads(p.read_text())
new = "$HOOK"
changed = 0
def walk(obj):
    global changed
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k == "X-Hook-Secret" and v != new:
                obj[k] = new
                changed += 1
            else:
                walk(v)
    elif isinstance(obj, list):
        for x in obj: walk(x)
walk(data)
p.write_text(json.dumps(data, indent=2))
print(f"updated {changed} X-Hook-Secret occurrence(s)")
EOF
```

If the header lives under a different key (some hook shapes use `headers: { "X-Hook-Secret": ... }`), grep for it first and adjust the walker.

## Step 4 — resurface the QR to the phone

Open the Tauri app → Settings → Mobile companion → re-scan the QR on the phone. The QR encodes `url + bearer + ntfy_topic` in one blob; scanning it rewrites the phone's DataStore.

## Step 5 — smoke test

Run the `companion-smoke-test` skill end-to-end. `/health` should return 200, `/panes` should return 200 with the new bearer, and the optional hook probe should return 200 with the new secret.

## Why this skill exists

`rotate_companion_*` updates the store but does not write to `~/.claude/settings.json`. The Notification hook silently breaks until someone updates it by hand. This runbook closes that gap.
