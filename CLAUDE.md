# Pane Management

Andrea's personal fork of `sky-salsa/pane-management-v0.4.0`. A Tauri 2 desktop app (Windows) plus a nested Android companion that control Claude Code sessions across tmux on WSL2, routed over Tailscale with no cloud APIs.

**Terminology (Andrea's convention):** "**app**" = Tauri desktop app (`workspace-resume/`). "**apk**" = Android mobile companion (`pane-management-mobile/`). Do not use "app" to mean the mobile client.

## Tech Stack

| Tool | Version |
|---|---|
| Tauri | 2 |
| Rust | stable-x86_64-pc-windows-msvc |
| axum | 0.8 |
| tokio | full |
| SolidJS | 1.9 |
| Vite | 6 |
| Tailwind | v4 |
| Kotlin | 2.1.10 |
| Compose BOM | 2025.01.01 |
| Ktor | 3.0.3 |
| AGP | 8.7.3 |
| compileSdk | 36 |
| JDK | 21 |

## Project Structure

```
.                                WSL source-of-truth (/home/andrea/pane-management/)
├── sync.sh                      WSL → Windows scratch rsync — run before every build
├── workspace-resume/            Tauri 2 desktop app
│   ├── src-tauri/               Rust backend — see src-tauri/CLAUDE.md
│   └── src/                     SolidJS frontend — see src/CLAUDE.md
└── pane-management-mobile/      Android companion — nested git repo, see its CLAUDE.md
```

Windows build scratch: `C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\` — never edit, never git.

## Key Rules

- Edit in WSL only; run `sync.sh` before every build → see `windows-scratch-discipline` rule
- Tauri commands need a four-file lockstep edit → see `tauri-commands` rule
- Companion endpoints go in one of three routers (auth / public / hooks) → see `companion-endpoints` rule
- Rust and Kotlin DTOs must stay wire-compatible → see `android-dto-contract` rule
- Use Tailscale IP for WSL→Windows calls, never `localhost` → see `wsl-networking` rule

## Build Order

1. sync
2. build
3. run

See `build` skill for the scoped sync → cargo/npm/gradle → launch sequence.

## Status

Phase B + UX redesign Phases 1-3 shipped. Next: ntfy lockscreen push. Plan: `~/.claude-b/plans/fluttering-jumping-kahn.md`.

## Available Skills

| Skill | Purpose |
|---|---|
| `build` | Scoped sync + build + launch for Tauri / Android / both |
| `companion-smoke-test` | Probe /health /panes /usage with bearer from the Tauri store |
| `phone-deploy` | ADB-over-Tailscale install + launch + logcat for the OnePlus 13 |
| `rotate-companion-secrets` | Regen bearer/hook/ntfy, propagate to `~/.claude/settings.json`, resurface QR |
| `diagnostics` | Run `diagnostics.ps1` and summarise the environment report |

Plugin enabled in `.claude/settings.local.json`: `frontend-design@claude-plugins-official`.

## Primary References

- Plan file: `~/.claude-b/plans/fluttering-jumping-kahn.md`
- Auto-memory: `~/.claude-b/projects/-home-andrea-pane-management/memory/`
- Upstream design docs: `BACKLOG.md`, `DEPENDENCIES.md`, `SETUP-GUIDE.md`, `PORTABILITY-AUDIT.md`

## Config Maintenance

This root CLAUDE.md is a routing table, not a knowledge store. When you add a Tauri command, companion endpoint, or DTO, update the matching rule in `.claude/rules/` and the subdirectory CLAUDE.md that documents it. When you change build commands, update the `build` skill, not this file. When a new recurring workflow emerges, add a skill under `.claude/skills/`. Keep this file under ~100 lines.
