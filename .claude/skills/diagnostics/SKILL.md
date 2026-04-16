---
name: diagnostics
description: >
  Run the repo's diagnostics.ps1 to collect environment data (WSL + Windows deps,
  tmux state, patch status, disk) and summarise the output. Use when the user says
  "diagnostics", "environment check", "collect diag", "why won't it build", or when
  a build / companion failure resists the usual smoke tests.
allowed-tools:
  - Bash
  - Read
---

# Diagnostics

The repo ships a comprehensive PowerShell diagnostic script at `/home/andrea/pane-management/diagnostics.ps1`. Agents tend to reimplement the same shell-outs (`wsl --status`, `tmux ls`, `sc query`); use this instead.

## Run

```bash
cd /home/andrea/pane-management
cmd.exe /c "powershell.exe -ExecutionPolicy Bypass -File C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\diagnostics.ps1" 2>&1 | tee /tmp/pm-diag.txt
```

The script writes a timestamped report (`diagnostics-report-<ts>.txt`) to whatever directory it runs from on Windows. The `tee` above also captures to `/tmp/pm-diag.txt` so you can grep from WSL.

If the script doesn't exist in the scratch, run `/home/andrea/pane-management/sync.sh` first.

## What the report contains

- WSL distro / kernel / version
- Node / Rust / Cargo / npm / Java versions on both sides
- Tmux sessions and pane counts
- Claude Code binary path + patch status (cch= marker)
- Windows Tailscale service state
- Disk usage for the scratch directory and `%APPDATA%` store

## Summarise for the user

After the run, read `/tmp/pm-diag.txt` and surface only the anomalies — missing dependencies, stopped services, patch drift. Do not reprint the full 100+ line report unless asked.
