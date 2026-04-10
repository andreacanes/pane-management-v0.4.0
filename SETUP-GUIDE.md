# Pane Management — New Machine Setup Guide

**Purpose:** Step-by-step instructions for a Claude Code agent to walk a new user through full environment setup on Windows 11 — from stock OS to running Pane Management with Claude Code in WSL.

**Audience:** The human is not necessarily technical. The Claude Code agent executing these instructions should explain what each step does in plain language, verify each step before moving on, and handle errors gracefully.

**Time estimate:** ~30-45 minutes (longer if WSL requires a reboot).

---

## Pre-Flight

Before starting, the agent should determine:

```
1. What is the Windows username? (Check with: $env:USERNAME in PowerShell)
2. Is WSL already installed? (Check with: wsl --status)
3. Is this a fresh setup or are we resuming after a WSL reboot?
4. Does the user already have a GitHub account? (needed for repo clone)
5. Does the user have an Anthropic API key or Claude account? (needed for auth)
```

Store the username in a variable — many paths below depend on it:
```
USERNAME = <the Windows username>
WSL_HOME = /home/<lowercase linux username>
WIN_HOME = C:\Users\<USERNAME>
```

---

## Step 1: Install WSL + Ubuntu

**What this does:** Enables Windows Subsystem for Linux and installs Ubuntu as the default Linux distribution. This is how Claude Code runs without hitting a Windows-specific billing cache bug.

### From an Administrator PowerShell:

```powershell
wsl --install
```

**If WSL was not previously enabled:** This will require a reboot. Tell the user:
> "Windows needs to restart to finish setting up WSL. After reboot, Ubuntu will open automatically to finish setup. You'll create a Linux username and password — remember these, you'll need them for sudo commands later. Once that's done, come back to me."

**If WSL was already enabled but no distro installed:**
```powershell
wsl --install -d Ubuntu
```

**If WSL and Ubuntu are already installed:**
```powershell
wsl --status
# Should show: Default Distribution: Ubuntu, WSL version: 2
```

### Verify:
```powershell
wsl echo "WSL is working"
# Should print: WSL is working
```

**If resuming after reboot:** Ubuntu should have launched automatically and asked for a username/password. If it didn't:
```powershell
ubuntu
# This launches Ubuntu and triggers first-time setup
```

---

## Step 2: Install Node.js via nvm (inside WSL)

**What this does:** Installs Node Version Manager, then uses it to install Node.js LTS. Claude Code is a Node.js application.

### Enter WSL:
```powershell
wsl
```

### Inside WSL (bash):
```bash
# Install nvm
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.0/install.sh | bash

# Reload shell config so nvm is available
source ~/.bashrc

# Install Node.js LTS
nvm install --lts

# Verify
node --version    # Should print v24.x.x or similar
npm --version     # Should print 11.x.x or similar
```

**Common issue:** If `nvm: command not found` after the install, the shell profile wasn't reloaded. Run `source ~/.bashrc` again or close and reopen the terminal.

---

## Step 3: Install Claude Code

**What this does:** Installs the Claude Code CLI globally via npm.

### Inside WSL:
```bash
npm install -g @anthropic-ai/claude-code

# Verify
which claude       # Should print a path like /home/<user>/.nvm/.../bin/claude
claude --version   # Should print version info
```

---

## Step 4: Authenticate Claude Code

**What this does:** Links Claude Code to the user's Anthropic/Claude account so it can make API calls.

### Inside WSL:
```bash
claude
```

This will open a browser window for authentication. The user needs to:
1. Log in to their Claude/Anthropic account (or create one)
2. Authorize Claude Code
3. Return to the terminal — it should show "Authenticated successfully" or similar

### Verify:
```bash
# Claude Code should now launch without auth prompts
# Press Ctrl+C to exit after confirming it works
```

**Note:** If the user is using a paid API key instead of OAuth, they can set it directly:
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# Add to ~/.bashrc to persist:
echo 'export ANTHROPIC_API_KEY="sk-ant-..."' >> ~/.bashrc
```

---

## Step 5: Apply the Cache Header Patch

**What this does:** Fixes a billing cache bug in Claude Code where the API reloads the full conversation on every tool call instead of using KV cache. Without this patch, token usage can spike ~100x. The patch changes a cache header value from `00000` to `c0ded` in the Claude Code binary.

**This patch may become unnecessary in a future Claude Code version.** Check first:

### Inside WSL:
```bash
# Find the Claude Code binary
CLAUDE_BINARY="$(npm root -g)/@anthropic-ai/claude-code/cli.js"

# Check if it even needs patching
grep -c 'cch=00000' "$CLAUDE_BINARY"
# If 0: check if already patched
grep -c 'cch=c0ded' "$CLAUDE_BINARY"
# If the c0ded check returns >= 1: already patched, skip to Step 6
# If neither is found: the binary format has changed, the patch may no longer be relevant — skip to Step 6
```

### If patching is needed (cch=00000 found):

Create the patch script:
```bash
mkdir -p ~/.claude/hooks
cat > ~/.claude/hooks/claude-wsl-patch.py << 'PATCHEOF'
import shutil
import sys
from pathlib import Path

SENTINEL = b"cch=00000"
OLD = b"00000"
NEW = b"c0ded"


def find_binary():
    """Auto-detect the latest Claude Code standalone binary."""
    versions_dir = Path.home() / ".local" / "share" / "claude" / "versions"
    if not versions_dir.exists():
        return None
    versions = sorted(versions_dir.iterdir(), key=lambda p: p.stat().st_mtime, reverse=True)
    return versions[0] if versions else None


def main():
    if len(sys.argv) > 1:
        binary = Path(sys.argv[1])
    else:
        binary = find_binary()
        if not binary:
            print("Could not auto-detect binary. Pass the path as an argument.")
            print("  Standalone: ~/.local/share/claude/versions/<version>")
            print("  npm: $(npm root -g)/@anthropic-ai/claude-code/cli.js")
            sys.exit(1)

    print(f"Binary: {binary} ({binary.stat().st_size / 1e6:.1f} MB)")

    backup = binary.with_suffix(".orig")
    if not backup.exists():
        shutil.copy2(binary, backup)
        print(f"Backup: {backup}")
    else:
        print(f"Backup: {backup} (already exists)")

    data = bytearray(binary.read_bytes())

    # Find all occurrences
    offsets = []
    start = 0
    while True:
        pos = data.find(SENTINEL, start)
        if pos == -1:
            break
        offsets.append(pos + 4)  # skip "cch=" to reach "00000"
        start = pos + 1

    if not offsets:
        # Check if already patched
        if data.find(b"cch=c0ded") != -1:
            print("Already patched (cch=c0ded found). Nothing to do.")
            sys.exit(0)
        print("Sentinel 'cch=00000' not found. Wrong binary or unsupported version.")
        sys.exit(1)

    print(f"Found {len(offsets)} occurrence(s) of cch=00000")

    for offset in offsets:
        assert data[offset:offset + 5] == OLD, f"Unexpected bytes at {offset}"
        data[offset:offset + 5] = NEW
        print(f"  Patched offset {offset}: 00000 -> c0ded")

    binary.write_bytes(data)
    shutil.copymode(backup, binary)
    print(f"\nDone. Restart Claude Code for the change to take effect.")


if __name__ == "__main__":
    main()
PATCHEOF
```

### Apply the patch:
```bash
python3 ~/.claude/hooks/claude-wsl-patch.py "$CLAUDE_BINARY"
```

### Verify:
```bash
grep -c 'cch=c0ded' "$CLAUDE_BINARY"
# Should print 1 or more (patched)
grep -c 'cch=00000' "$CLAUDE_BINARY"
# Should print 0 (no unpatched instances remain)
```

---

## Step 6: Install the Auto-Patch Hook

**What this does:** Claude Code auto-updates itself periodically, which overwrites the patched binary. This hook runs on every prompt submission and silently re-applies the patch if needed. It has safety mechanisms (cooldown, circuit breaker) to prevent lockout.

### Inside WSL:

First, determine the Claude Code binary path for this machine:
```bash
CLAUDE_BINARY="$(npm root -g)/@anthropic-ai/claude-code/cli.js"
echo "Binary path: $CLAUDE_BINARY"
```

Create the hook script (note: the BINARY variable below must be set to the path found above):
```bash
cat > ~/.claude/hooks/wsl-patch-prompt-check.sh << HOOKEOF
#!/usr/bin/env bash
# UserPromptSubmit: auto-reapply WSL billing cache patch if update overwrote it.
#
#   cch=c0ded  -> patched, silent exit
#   cch=00000  -> auto-reapply silently; block once if reapply fails
#   neither    -> sentinel gone, block once then cooldown
#
# SAFETY: blocks at most once per 30 minutes to prevent lockout.
# CIRCUIT BREAKER: after 6 blocks without resolution, disables itself.
#   To re-enable: rm ~/.claude/hooks/wsl-patch-hook-disabled

BINARY="$CLAUDE_BINARY"
[ ! -f "\$BINARY" ] && BINARY="\$(npm root -g 2>/dev/null)/@anthropic-ai/claude-code/cli.js"
[ ! -f "\$BINARY" ] && exit 0

LOGFILE="\$HOME/.claude/hooks/patch-check-hook.log"
PATCHER="\$HOME/.claude/hooks/claude-wsl-patch.py"
COOLDOWN_FILE="/tmp/claude-patch-block-cooldown"
BLOCK_COUNT_FILE="/tmp/claude-patch-block-count"
DISABLED_FILE="\$HOME/.claude/hooks/wsl-patch-hook-disabled"
COOLDOWN_SECONDS=1800
MAX_BLOCKS=6
MAX_LINES=20

log() {
  echo "\$(date '+%Y-%m-%d %H:%M:%S') \$1" >> "\$LOGFILE"
  tail -n "\$MAX_LINES" "\$LOGFILE" > "\$LOGFILE.tmp" && mv "\$LOGFILE.tmp" "\$LOGFILE"
}

# Circuit breaker: if disabled, exit immediately
if [ -f "\$DISABLED_FILE" ]; then
  exit 0
fi

# Block once, then cooldown. Returns 0 if block should fire, 1 if in cooldown.
should_block() {
  if [ ! -f "\$COOLDOWN_FILE" ]; then
    return 0
  fi
  LAST_BLOCK=\$(cat "\$COOLDOWN_FILE" 2>/dev/null)
  NOW=\$(date +%s 2>/dev/null) || return 1
  case "\$LAST_BLOCK" in ''|*[!0-9]*) return 0 ;; esac
  ELAPSED=\$((NOW - LAST_BLOCK))
  [ "\$ELAPSED" -gt "\$COOLDOWN_SECONDS" ] && return 0
  return 1
}

fire_block() {
  # Update cooldown timestamp
  date +%s > "\$COOLDOWN_FILE" 2>/dev/null

  # Increment block count and check circuit breaker
  BCOUNT=0
  [ -f "\$BLOCK_COUNT_FILE" ] && BCOUNT=\$(cat "\$BLOCK_COUNT_FILE" 2>/dev/null)
  case "\$BCOUNT" in ''|*[!0-9]*) BCOUNT=0 ;; esac
  BCOUNT=\$((BCOUNT + 1))
  echo "\$BCOUNT" > "\$BLOCK_COUNT_FILE" 2>/dev/null

  if [ "\$BCOUNT" -ge "\$MAX_BLOCKS" ]; then
    log "CIRCUIT-BREAKER tripped after \$BCOUNT blocks -- disabling hook"
    echo "disabled after \$BCOUNT blocks at \$(date '+%Y-%m-%d %H:%M:%S')" > "\$DISABLED_FILE" 2>/dev/null
    echo "{\"decision\": \"block\", \"reason\": \"WSL patch hook has blocked \$BCOUNT prompts without resolution. Disabling itself. To re-enable: rm ~/.claude/hooks/wsl-patch-hook-disabled\"}"
    exit 0
  fi

  log "BLOCKED \$1 (\$BCOUNT of \$MAX_BLOCKS before circuit breaker)"
  echo "\$2"
  exit 0
}

# Happy path -- already patched. Reset block count since the issue is resolved.
if grep -qa 'cch=c0ded' "\$BINARY" 2>/dev/null; then
  rm -f "\$BLOCK_COUNT_FILE" "\$COOLDOWN_FILE" 2>/dev/null
  exit 0
fi

# Needs patching
if grep -qa 'cch=00000' "\$BINARY" 2>/dev/null; then
  OUTPUT=\$(python3 "\$PATCHER" "\$BINARY" 2>&1)
  if [ \$? -eq 0 ]; then
    log "AUTO-PATCHED"
    rm -f "\$BLOCK_COUNT_FILE" "\$COOLDOWN_FILE" 2>/dev/null
    exit 0
  fi
  # Patch failed -- block once then cooldown
  if should_block; then
    fire_block "patch-failed" "{\"decision\": \"block\", \"reason\": \"WSL patch auto-reapply failed. Will remind in 30 min. Auto-disables after 6 blocks. Error: \$OUTPUT\"}"
  fi
  exit 0
fi

# Neither sentinel -- something changed
if should_block; then
  fire_block "sentinel-missing" '{"decision": "block", "reason": "WSL patch sentinel missing -- cch= value is neither 00000 nor c0ded. Format may have changed. Will remind in 30 min. Auto-disables after 6 blocks."}'
fi
exit 0
HOOKEOF

chmod +x ~/.claude/hooks/wsl-patch-prompt-check.sh
```

### Register the hook in Claude Code settings:

Check if settings.json exists and has hooks configured:
```bash
cat ~/.claude/settings.json 2>/dev/null || echo "{}"
```

The settings.json needs a hooks entry. If the file doesn't exist or doesn't have hooks, create/update it. The final settings.json should include (merge with any existing content):
```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "bash ~/.claude/hooks/wsl-patch-prompt-check.sh"
          }
        ]
      }
    ]
  }
}
```

**Important:** If settings.json already has other content (permissions, preferences, other hooks), merge — don't overwrite. The hooks key should be added alongside existing keys.

### Verify the hook:
```bash
# Start Claude Code and send any message
claude
# Type something like "hello" and press Enter
# Check the hook ran:
cat ~/.claude/hooks/patch-check-hook.log
# Should show an entry (or be empty if already patched — that's the silent happy path)
```

---

## Step 7: Install Pane Management

**What this does:** Installs the Pane Management desktop app — a visual dashboard for managing tmux panes, Claude Code sessions, and project assignments.

**Prerequisites for building from source:** Pane Management is a Tauri v2 app (Rust backend + SolidJS frontend). Building requires:
- Rust toolchain (rustc + cargo)
- Visual Studio Build Tools 2022 with C++ workload
- Node.js (already installed in Step 2, but needed on Windows side too for npm)
- Git for Windows

### Install Windows-side prerequisites:

From an **Administrator PowerShell**:
```powershell
# Install Git for Windows (if not already present)
winget install Git.Git

# Install Rust via rustup
# IMPORTANT: Visual Studio Build Tools must be installed FIRST
# Download and install from https://visualstudio.microsoft.com/visual-cpp-build-tools/
# Select "Desktop development with C++" workload
# Then install Rust:
winget install Rustlang.Rustup

# Install Node.js on Windows side (needed for npm run tauri)
winget install OpenJS.NodeJS.LTS

# Verify (open a NEW PowerShell window after installs):
rustc --version     # Should print rustc 1.x.x
cargo --version     # Should print cargo 1.x.x
node --version      # Should print v24.x.x
npm --version       # Should print 11.x.x
git --version       # Should print git version 2.x.x
```

**Note on Visual Studio Build Tools:** This is the most annoying prerequisite. The Rust compiler on Windows needs the MSVC linker. The user needs to:
1. Download Visual Studio Build Tools 2022 from https://visualstudio.microsoft.com/visual-cpp-build-tools/
2. Run the installer
3. Select "Desktop development with C++" workload
4. Install (this is a large download, ~2-5 GB)

If they already have Visual Studio 2022 installed with C++ support, this step can be skipped.

### Install tmux (inside WSL):
```bash
wsl
sudo apt update && sudo apt install -y tmux

# Verify
tmux -V    # Should print tmux 3.x
```

### Optional: Install tmux plugins (recommended):
```bash
# Install TPM (tmux plugin manager)
git clone https://github.com/tmux-plugins/tpm ~/.tmux/plugins/tpm

# Create minimal tmux config
cat > ~/.tmux.conf << 'TMUXEOF'
# Mouse support
set -g mouse on

# 256 color
set -g default-terminal "screen-256color"

# Start windows at 1
set -g base-index 1

# Scrollback
set -g history-limit 10000

# Plugins
set -g @plugin 'tmux-plugins/tpm'
set -g @plugin 'tmux-plugins/tmux-resurrect'
set -g @plugin 'tmux-plugins/tmux-continuum'

# Initialize TPM (keep at bottom)
run '~/.tmux/plugins/tpm/tpm'
TMUXEOF

# Start tmux and install plugins
tmux
# Inside tmux: press Ctrl+B then Shift+I (capital I) to install plugins
# Wait for "Installing..." to complete, then press Enter
```

---

## Step 8: Clone and Build the Repo

**What this does:** Clones the Pane Management source code and builds the app.

### Clone the repo (from PowerShell, on the Windows filesystem):
```powershell
cd "$env:USERPROFILE\Documents"
git clone https://github.com/sky-salsa/pane-management.git
cd pane-management\workspace-resume
```

**If the repo is private:** The user needs to be added as a collaborator on GitHub first. They'll need to authenticate git:
```powershell
# Option A: GitHub CLI (easiest)
winget install GitHub.cli
gh auth login
# Follow prompts, then clone will work

# Option B: Git credential manager (bundled with Git for Windows)
# Just clone — it will prompt for GitHub credentials in a browser popup
```

### Install frontend dependencies:
```powershell
cd "$env:USERPROFILE\Documents\pane-management\workspace-resume"
npm install
```

### Build and run (dev mode):
```powershell
npm run tauri dev
```

**First build will be slow** (~3-5 minutes) as Cargo compiles all Rust dependencies. Subsequent builds are fast (~10-20 seconds).

### Or build the installer:
```powershell
npm run tauri build
```

The .msi installer will be at:
```
workspace-resume\src-tauri\target\release\bundle\msi\Pane Management_0.4.0_x64_en-US.msi
```

### Verify the app works:

1. App window should appear with the Pane Management UI
2. If WSL/tmux are set up (Steps 1-2 + 7), the app should detect tmux sessions
3. If no tmux session is running, the app should show "Select a tmux session and window above"

### First-time usage:

Tell the user to start a tmux session in WSL first:
```bash
wsl
tmux new -s workspace
```

Then in the app:
- The "workspace" session should appear in the top bar
- Click it to select it
- Projects from `~/.claude/projects/` will appear in the sidebar
- They can drag projects into pane slots, click Resume, etc.

---

## Troubleshooting

### WSL won't install
- Requires Windows 11 (or Windows 10 build 19041+)
- Requires virtualization enabled in BIOS (VT-x / AMD-V)
- Try: `dism.exe /online /enable-feature /featurename:Microsoft-Windows-Subsystem-Linux /all /norestart`

### Rust build fails with linker errors
- Visual Studio Build Tools not installed, or C++ workload not selected
- Open Visual Studio Installer → Modify → ensure "Desktop development with C++" is checked

### `claude: command not found` in WSL
- nvm not loaded: `source ~/.bashrc`
- Node not installed: `nvm install --lts`
- Claude Code not installed: `npm install -g @anthropic-ai/claude-code`

### Patch script says "Sentinel not found"
- Claude Code version may have changed the binary format
- Check if the bug is still relevant: https://github.com/anthropics/claude-code/issues/40524
- If the issue is closed/fixed, the patch is no longer needed

### App builds but shows blank white window
- WebView2 issue. Usually resolved by Windows Update.
- Check: `npx tauri info` for WebView2 version

### tmux not found by the app
- Make sure tmux is installed inside WSL: `wsl -e which tmux`
- Make sure a tmux session is running: `wsl -e tmux list-sessions`

### Something else is broken
Run the diagnostics script to collect system info for the developer:
```powershell
cd path\to\pane-management
powershell -File diagnostics.ps1
```
This creates a `diagnostics-report-<timestamp>.txt` file. Send it to the developer — it contains dependency versions, WSL status, tmux state, patch status, and system resources. No personal data or code is collected.

---

## Summary Checklist

After completing all steps, verify:

- [ ] `wsl echo "ok"` prints "ok" from PowerShell
- [ ] `wsl -e node --version` prints a version number
- [ ] `wsl -e which claude` prints a path
- [ ] `wsl -e tmux -V` prints tmux version
- [ ] Claude Code binary is patched: `wsl -e grep -c 'cch=c0ded' "$(wsl -e npm root -g)/@anthropic-ai/claude-code/cli.js"` returns >= 1
- [ ] Hook is registered in `~/.claude/settings.json` (inside WSL)
- [ ] Pane Management app launches (either via `npm run tauri dev` or installed .msi)
- [ ] App detects tmux session when one is running
- [ ] `rustc --version` works in PowerShell (needed for future builds)
- [ ] `git --version` works in PowerShell
