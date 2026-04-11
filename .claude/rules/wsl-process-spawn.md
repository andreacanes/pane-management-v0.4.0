---
description: Any Command spawn on Windows must set CREATE_NO_WINDOW or a console flashes
paths: ["workspace-resume/src-tauri/src/**/*.rs"]
---

Every `std::process::Command` invocation (especially `wsl.exe`, `powershell.exe`, `taskkill.exe`) must set `creation_flags(0x08000000)` (`CREATE_NO_WINDOW`) on Windows targets:

```rust
#[cfg(windows)]
{
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000);
}
```

Missing it flashes a `cmd` window on every call. The canonical `run_tmux_command` helper in `commands/tmux.rs` already handles this — prefer routing tmux calls through it rather than spawning `wsl.exe` directly. Companion code (`companion/http.rs`, `companion/tmux_poller.rs`) is allowed to call `crate::commands::tmux::run_tmux_command` — the usual "services → commands" direction is relaxed for this one helper.
