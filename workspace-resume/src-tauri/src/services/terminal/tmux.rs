use super::{LaunchError, LaunchResult, TerminalLauncher};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub struct TmuxLauncher {
    session_name: String,
}

impl TmuxLauncher {
    pub fn new() -> Self {
        Self {
            session_name: "workspace".to_string(),
        }
    }

    pub fn with_session_name(session_name: String) -> Self {
        Self { session_name }
    }

    /// Convert a Windows path to a WSL mount path
    /// C:\Users\USERNAME\Documents -> /mnt/c/Users/USERNAME/Documents
    pub fn windows_to_wsl_path(win_path: &str) -> String {
        let path = win_path.replace('\\', "/");
        // Handle drive letter: C:/... -> /mnt/c/...
        if path.len() >= 2 && path.as_bytes()[1] == b':' {
            let drive = path.as_bytes()[0].to_ascii_lowercase() as char;
            format!("/mnt/{}{}", drive, &path[2..])
        } else {
            // Already a Unix path or relative path
            path
        }
    }

    /// Generate a sanitized tmux session name from a project path
    pub fn session_name_from_path(path: &str) -> String {
        let last_segment = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("session");

        // tmux session names can't contain dots or colons
        last_segment
            .chars()
            .map(|c| match c {
                '.' | ':' | ' ' => '-',
                c if c.is_alphanumeric() || c == '-' || c == '_' => c,
                _ => '_',
            })
            .collect()
    }
}

impl TerminalLauncher for TmuxLauncher {
    fn launch(&self, working_dir: &str, command: Option<&str>) -> Result<LaunchResult, LaunchError> {
        if !self.is_available() {
            return Err(LaunchError::NotAvailable(
                "WSL is not available (wsl.exe not found)".into(),
            ));
        }

        let wsl_path = Self::windows_to_wsl_path(working_dir);
        let window_name = Self::session_name_from_path(working_dir);

        eprintln!("[TmuxLauncher] working_dir: {}", working_dir);
        eprintln!("[TmuxLauncher] wsl_path: {}", wsl_path);
        eprintln!("[TmuxLauncher] window_name: {}", window_name);
        eprintln!("[TmuxLauncher] command: {:?}", command);

        // Use the configured session name for all workspace-resume windows.
        let session = &self.session_name;

        // Single bash script that does everything in one wsl.exe call:
        // 1. Create 'workspace' session if it doesn't exist
        // 2. Create a new window with the right name
        // 3. Send cd + command to that window via send-keys (avoids quoting issues)
        // 4. Switch the user's active tmux client to see the new window
        let send_cmd = if let Some(cmd) = command {
            format!("cd \\\"{}\\\" && {}", wsl_path, cmd)
        } else {
            format!("cd \\\"{}\\\"", wsl_path)
        };

        let script = format!(
            concat!(
                "tmux has-session -t {sess} 2>/dev/null || tmux new-session -d -s {sess}; ",
                "tmux new-window -t {sess}: -n {name}; ",
                "tmux send-keys -t {sess}:{name} \"{cmd}\" Enter; ",
                "tmux switch-client -t {sess}:{name} 2>/dev/null; ",
                "echo OK"
            ),
            sess = session,
            name = window_name,
            cmd = send_cmd,
        );

        eprintln!("[TmuxLauncher] script: {}", script);

        let output = std::process::Command::new("wsl.exe")
            .args(["-e", "bash", "-c", &script])
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| LaunchError::SpawnFailed(format!("Failed to run tmux commands: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        eprintln!("[TmuxLauncher] exit: {:?}, stdout: {}, stderr: {}", output.status.code(), stdout, stderr);

        if !output.status.success() {
            return Err(LaunchError::SpawnFailed(format!("tmux failed: {}", stderr)));
        }

        eprintln!("[TmuxLauncher] success — window '{}' created in session '{}'", window_name, session);

        Ok(LaunchResult {
            pid: None,
            child: None,
        })
    }

    fn is_alive(&self, pid: u32) -> bool {
        is_pid_alive(pid)
    }

    fn name(&self) -> &str {
        "tmux (WSL)"
    }

    fn is_available(&self) -> bool {
        // Check if wsl.exe exists and WSL is functional
        std::process::Command::new("wsl.exe")
            .args(["--status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Check if a process is alive via Win32 OpenProcess
#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::Foundation::CloseHandle;

    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(not(windows))]
fn is_pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_to_wsl_path_c_drive() {
        assert_eq!(
            TmuxLauncher::windows_to_wsl_path("C:\\Users\\USERNAME\\Documents"),
            "/mnt/c/Users/USERNAME/Documents"
        );
    }

    #[test]
    fn test_windows_to_wsl_path_forward_slashes() {
        assert_eq!(
            TmuxLauncher::windows_to_wsl_path("C:/Users/USERNAME/Documents"),
            "/mnt/c/Users/USERNAME/Documents"
        );
    }

    #[test]
    fn test_windows_to_wsl_path_d_drive() {
        assert_eq!(
            TmuxLauncher::windows_to_wsl_path("D:\\Projects"),
            "/mnt/d/Projects"
        );
    }

    #[test]
    fn test_windows_to_wsl_path_already_unix() {
        assert_eq!(
            TmuxLauncher::windows_to_wsl_path("/mnt/c/Users/USERNAME"),
            "/mnt/c/Users/USERNAME"
        );
    }

    #[test]
    fn test_windows_to_wsl_path_with_spaces() {
        assert_eq!(
            TmuxLauncher::windows_to_wsl_path("C:\\Users\\USERNAME\\My Documents\\AI Workspace"),
            "/mnt/c/Users/USERNAME/My Documents/AI Workspace"
        );
    }

    #[test]
    fn test_session_name_simple() {
        let name = TmuxLauncher::session_name_from_path("C:\\Users\\USERNAME\\project-foo");
        assert_eq!(name, "project-foo");
    }

    #[test]
    fn test_session_name_with_spaces() {
        let name = TmuxLauncher::session_name_from_path("C:\\Users\\USERNAME\\AI Workspace");
        assert_eq!(name, "AI-Workspace");
    }

    #[test]
    fn test_session_name_with_dots() {
        let name = TmuxLauncher::session_name_from_path("C:\\Users\\USERNAME\\v1.0.release");
        assert_eq!(name, "v1-0-release");
    }

    #[test]
    fn test_tmux_launcher_name() {
        let launcher = TmuxLauncher::new();
        assert_eq!(launcher.name(), "tmux (WSL)");
    }
}
