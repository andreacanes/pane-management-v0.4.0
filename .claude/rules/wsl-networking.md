---
description: WSL2 mirrored networking breaks localhost; always use the Tailscale IP for WSL to Windows calls
---

WSL2 is in mirrored networking mode (`/etc/wsl.conf` → `[wsl2] networkingMode=mirrored`). `localhost` from WSL binds IPv6 loopback and is invisible to the Windows interface stack.

## Tailscale IPs

| Device | Tailscale IP | Purpose |
|---|---|---|
| Windows desktop | `100.110.47.29` | Companion on `:8833`, hooks target this |
| OnePlus 13 | `100.83.163.105` | ADB over Tailscale on `:5555` |
| Mediaserver | `100.94.215.7` | Linux server |

For any WSL → Windows call (smoke tests, the Claude Code Notification hook), use `100.110.47.29:8833`, not `127.0.0.1`. The Notification hook in `~/.claude/settings.json` must use the Tailscale IP.

ADB connects to the phone wirelessly via Tailscale: `adb connect 100.83.163.105:5555`. No USB cable needed. Connection drops when the phone sleeps — always reconnect before install.

If the companion becomes unreachable from WSL, check `sc query Tailscale` before suspecting code — the Windows Tailscale service can silently stop.
