---
description: WSL2 mirrored networking breaks localhost; always use the Tailscale IP for WSL to Windows calls
---

WSL2 is in mirrored networking mode (`/etc/wsl.conf` → `[wsl2] networkingMode=mirrored`). `localhost` from WSL binds IPv6 loopback and is invisible to the Windows interface stack.

For any WSL → Windows call (smoke tests, the Claude Code Notification hook, `adb reverse` fallbacks), use the Tailscale IP `100.110.47.29:8833`, not `127.0.0.1`. The Notification hook in `~/.claude/settings.json` must use the Tailscale IP.

If the companion becomes unreachable from WSL, check `sc query Tailscale` before suspecting code — the Windows Tailscale service can silently stop.
