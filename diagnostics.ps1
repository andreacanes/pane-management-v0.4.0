# Pane Management — Diagnostics Collector
# Run this script when something isn't working right.
# It collects system info, dependency versions, and recent errors
# into a single text file you can send to the developer.
#
# Usage: powershell -File diagnostics.ps1
# Output: diagnostics-report-<timestamp>.txt in the current directory

$timestamp = Get-Date -Format "yyyy-MM-dd_HH-mm-ss"
$reportFile = "diagnostics-report-$timestamp.txt"

function Write-Section($title) {
    "`n{'='*60}" | Out-File -Append $reportFile
    "  $title" | Out-File -Append $reportFile
    "{'='*60}`n" | Out-File -Append $reportFile
}

function Run-Check($label, $command) {
    "$label`:" | Out-File -Append $reportFile
    try {
        $output = Invoke-Expression $command 2>&1
        "  $output" | Out-File -Append $reportFile
    } catch {
        "  ERROR: $($_.Exception.Message)" | Out-File -Append $reportFile
    }
    "" | Out-File -Append $reportFile
}

# Header
"Pane Management Diagnostics Report" | Out-File $reportFile
"Generated: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')" | Out-File -Append $reportFile
"Computer: $env:COMPUTERNAME" | Out-File -Append $reportFile
"User: $env:USERNAME" | Out-File -Append $reportFile

# Windows
Write-Section "Windows Environment"
Run-Check "Windows Version" "(Get-CimInstance Win32_OperatingSystem).Caption + ' Build ' + (Get-CimInstance Win32_OperatingSystem).BuildNumber"
Run-Check "PowerShell Version" "`$PSVersionTable.PSVersion.ToString()"

# WSL
Write-Section "WSL Status"
Run-Check "WSL Status" "wsl --status 2>&1 | Out-String"
Run-Check "WSL Distros" "wsl -l -v 2>&1 | Out-String"

# Dependencies (Windows side)
Write-Section "Windows Dependencies"
Run-Check "Git" "git --version 2>&1"
Run-Check "Rust (rustc)" "rustc --version 2>&1"
Run-Check "Cargo" "cargo --version 2>&1"
Run-Check "Node.js (Windows)" "node --version 2>&1"
Run-Check "npm (Windows)" "npm --version 2>&1"

# Dependencies (WSL side)
Write-Section "WSL Dependencies"
Run-Check "Node.js (WSL)" "wsl -e bash -c 'node --version 2>&1'"
Run-Check "npm (WSL)" "wsl -e bash -c 'npm --version 2>&1'"
Run-Check "tmux" "wsl -e bash -c 'tmux -V 2>&1'"
Run-Check "Claude Code" "wsl -e bash -c 'claude --version 2>&1'"
Run-Check "Claude Code path" "wsl -e bash -c 'which claude 2>&1'"
Run-Check "Python3" "wsl -e bash -c 'python3 --version 2>&1'"

# tmux state
Write-Section "tmux State"
Run-Check "tmux sessions" "wsl -e bash -c 'tmux list-sessions 2>&1'"
Run-Check "tmux windows (all)" "wsl -e bash -c 'tmux list-windows -a 2>&1'"
Run-Check "tmux panes (all)" "wsl -e bash -c 'tmux list-panes -a 2>&1'"

# Claude Code cache patch status
Write-Section "Cache Patch Status"
Run-Check "Patch check (c0ded)" "wsl -e bash -c 'CLI=`$(npm root -g 2>/dev/null)/@anthropic-ai/claude-code/cli.js; [ -f `"`$CLI`" ] && grep -c cch=c0ded `"`$CLI`" || echo cli.js not found'"
Run-Check "Patch check (00000)" "wsl -e bash -c 'CLI=`$(npm root -g 2>/dev/null)/@anthropic-ai/claude-code/cli.js; [ -f `"`$CLI`" ] && grep -c cch=00000 `"`$CLI`" || echo cli.js not found'"
Run-Check "Patch hook exists" "wsl -e bash -c '[ -f ~/.claude/hooks/wsl-patch-prompt-check.sh ] && echo YES || echo NO'"
Run-Check "Patch hook log (last 10)" "wsl -e bash -c 'tail -10 ~/.claude/hooks/patch-check-hook.log 2>/dev/null || echo no log file'"

# Pane Management app
Write-Section "Pane Management App"
$appDataPath = "$env:APPDATA\com.pane-management.app"
Run-Check "App data directory" "if (Test-Path '$appDataPath') { 'EXISTS' } else { 'NOT FOUND' }"
Run-Check "Settings file" "if (Test-Path '$appDataPath\settings.json') { 'EXISTS (' + (Get-Item '$appDataPath\settings.json').Length + ' bytes)' } else { 'NOT FOUND' }"

# Check if the app is currently running
Run-Check "App process" "Get-Process -Name 'Pane Management' -ErrorAction SilentlyContinue | Select-Object Id, CPU, WorkingSet64 | Format-Table | Out-String"

# Recent errors from npm/cargo if available
Write-Section "Recent Build Output (if available)"
$projectRoot = Split-Path -Parent $PSScriptRoot
if (Test-Path "$projectRoot\workspace-resume") {
    Run-Check "npm audit" "cd '$projectRoot\workspace-resume'; npm audit --production 2>&1 | Select-Object -First 20 | Out-String"
} else {
    "  workspace-resume directory not found at expected location" | Out-File -Append $reportFile
}

# Disk space
Write-Section "System Resources"
Run-Check "Disk space (C:)" "Get-PSDrive C | Select-Object Used, Free | Format-Table @{n='Used (GB)';e={[math]::Round(`$_.Used/1GB,1)}}, @{n='Free (GB)';e={[math]::Round(`$_.Free/1GB,1)}} | Out-String"
Run-Check "Memory" "(Get-CimInstance Win32_OperatingSystem | Select-Object @{n='Total (GB)';e={[math]::Round(`$_.TotalVisibleMemorySize/1MB,1)}}, @{n='Free (GB)';e={[math]::Round(`$_.FreePhysicalMemory/1MB,1)}}) | Format-Table | Out-String"

# Footer
Write-Section "End of Report"
"Send this file to the developer for troubleshooting." | Out-File -Append $reportFile
"File: $reportFile" | Out-File -Append $reportFile

Write-Host ""
Write-Host "Diagnostics collected successfully!" -ForegroundColor Green
Write-Host "Report saved to: $reportFile" -ForegroundColor Cyan
Write-Host ""
Write-Host "Please send this file to the developer." -ForegroundColor Yellow
