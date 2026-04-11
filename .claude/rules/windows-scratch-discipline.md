---
description: WSL is source of truth; Windows scratch is output-only; run sync.sh before every build
---

Source-of-truth: `/home/andrea/pane-management/`. Windows build scratch: `C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\`.

- Never edit files in the scratch — the next `sync.sh` runs with `--delete` and will wipe your edits
- Never run `git` in the scratch — `.git/` directories are deliberately removed; if they reappear, delete them again
- Always run `/home/andrea/pane-management/sync.sh` before any build. If an edit "didn't take", you forgot this step
- The iteration loop is `edit in WSL → sync.sh → build on Windows → run`, not `edit → build`
- `rm -rf` the scratch is always safe — the next sync rebuilds it (cold first build)
