pub mod warp;
pub mod powershell;
pub mod tmux;

use std::process::Child;

#[derive(Debug)]
pub struct LaunchResult {
    /// None for URI-scheme launches where PID is unknown (e.g., Warp)
    pub pid: Option<u32>,
    /// None for URI-launched processes
    pub child: Option<Child>,
}

#[derive(Debug)]
pub enum LaunchError {
    NotAvailable(String),
    SpawnFailed(String),
    ConfigError(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::NotAvailable(msg) => write!(f, "Terminal not available: {}", msg),
            LaunchError::SpawnFailed(msg) => write!(f, "Failed to spawn terminal: {}", msg),
            LaunchError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
        }
    }
}

pub trait TerminalLauncher: Send + Sync {
    /// Launch a terminal at the given directory, optionally running a command
    fn launch(&self, working_dir: &str, command: Option<&str>) -> Result<LaunchResult, LaunchError>;
    /// Check if a previously launched process is still alive
    fn is_alive(&self, pid: u32) -> bool;
    /// Human-readable name for settings UI
    fn name(&self) -> &str;
    /// Whether this terminal is available on the system
    fn is_available(&self) -> bool;
}

use crate::models::settings::{TerminalBackend, TerminalSettings};

pub fn create_launcher(backend: &TerminalBackend) -> Box<dyn TerminalLauncher> {
    match backend {
        TerminalBackend::Tmux => Box::new(tmux::TmuxLauncher::new()),
        TerminalBackend::Warp => Box::new(warp::WarpLauncher::new()),
        TerminalBackend::Powershell => Box::new(powershell::PowerShellLauncher::new()),
    }
}

pub fn create_launcher_from_settings(settings: &TerminalSettings) -> Box<dyn TerminalLauncher> {
    match settings.backend {
        TerminalBackend::Tmux => Box::new(tmux::TmuxLauncher::with_session_name(
            settings.tmux_session_name.clone(),
        )),
        TerminalBackend::Warp => Box::new(warp::WarpLauncher::new()),
        TerminalBackend::Powershell => Box::new(powershell::PowerShellLauncher::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_launcher_tmux() {
        let launcher = create_launcher(&TerminalBackend::Tmux);
        assert_eq!(launcher.name(), "tmux (WSL)");
    }

    #[test]
    fn test_create_launcher_warp() {
        let launcher = create_launcher(&TerminalBackend::Warp);
        assert_eq!(launcher.name(), "Warp");
    }

    #[test]
    fn test_create_launcher_powershell() {
        let launcher = create_launcher(&TerminalBackend::Powershell);
        assert_eq!(launcher.name(), "PowerShell");
    }

    #[test]
    fn test_launch_error_display() {
        let err = LaunchError::NotAvailable("test".to_string());
        assert_eq!(format!("{}", err), "Terminal not available: test");

        let err = LaunchError::SpawnFailed("spawn fail".to_string());
        assert_eq!(format!("{}", err), "Failed to spawn terminal: spawn fail");

        let err = LaunchError::ConfigError("bad config".to_string());
        assert_eq!(format!("{}", err), "Configuration error: bad config");
    }
}
