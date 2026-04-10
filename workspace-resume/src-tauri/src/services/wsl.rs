//! WSL distro + path detection.
//!
//! Runs Windows-side wsl.exe commands and parses their UTF-16 output
//! to discover the default distro name and the WSL user's home path.
//! Results are cached for the lifetime of the process since these
//! values don't change while the app is running.

use std::path::PathBuf;
use std::sync::OnceLock;

static WSL_INFO: OnceLock<Option<WslInfo>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct WslInfo {
    pub distro: String,
    pub user: String,
}

impl WslInfo {
    /// UNC path to the WSL user's home directory, e.g.
    /// `\\wsl.localhost\Ubuntu\home\andrea`
    pub fn home_unc(&self) -> PathBuf {
        PathBuf::from(format!(r"\\wsl.localhost\{}\home\{}", self.distro, self.user))
    }

    /// UNC path to the Claude Code projects directory.
    pub fn claude_projects_unc(&self) -> PathBuf {
        self.home_unc().join(".claude").join("projects")
    }
}

/// Get cached WSL info, detecting on first call.
/// Returns `None` if WSL isn't installed or detection fails.
pub fn wsl_info() -> Option<&'static WslInfo> {
    WSL_INFO.get_or_init(detect).as_ref()
}

fn detect() -> Option<WslInfo> {
    let distro = detect_default_distro()?;
    let user = detect_wsl_user(&distro)?;
    Some(WslInfo { distro, user })
}

/// Parse `wsl.exe --status` to find the default distribution name.
/// Output is UTF-16LE and looks like:
///   Default Distribution: Ubuntu
///   Default Version: 2
fn detect_default_distro() -> Option<String> {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.arg("--status");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = decode_utf16_lossy(&output.stdout);
    for line in text.lines() {
        // Match both English and localized output by looking for any colon-separated line
        // whose key contains "Default" — in English: "Default Distribution: Ubuntu".
        if let Some((key, value)) = line.split_once(':') {
            let key_lower = key.trim().to_lowercase();
            if key_lower.contains("default") && key_lower.contains("distribution") {
                let name = value.trim().to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    None
}

/// Get the current user inside the given WSL distro via `wsl.exe -d <distro> -e whoami`.
/// This returns plain text (no UTF-16 wrapping) because `whoami` is executed inside Linux.
fn detect_wsl_user(distro: &str) -> Option<String> {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-d", distro, "-e", "whoami"]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    // The Linux-side `whoami` emits plain UTF-8.
    let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if user.is_empty() {
        None
    } else {
        Some(user)
    }
}

/// Decode a byte buffer as UTF-16LE, skipping a BOM if present.
/// Falls back to UTF-8 lossy if the buffer isn't even-length or doesn't decode cleanly.
fn decode_utf16_lossy(bytes: &[u8]) -> String {
    let start = if bytes.starts_with(&[0xFF, 0xFE]) { 2 } else { 0 };
    let slice = &bytes[start..];
    if slice.len() % 2 != 0 {
        return String::from_utf8_lossy(bytes).to_string();
    }
    let units: Vec<u16> = slice
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_utf16_simple() {
        // "Hi" as UTF-16LE: 0x48 0x00 0x69 0x00
        let bytes = vec![0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_utf16_lossy(&bytes), "Hi");
    }

    #[test]
    fn test_decode_utf16_with_bom() {
        let bytes = vec![0xFF, 0xFE, 0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_utf16_lossy(&bytes), "Hi");
    }

    #[test]
    fn test_home_unc_format() {
        let info = WslInfo {
            distro: "Ubuntu".to_string(),
            user: "andrea".to_string(),
        };
        assert_eq!(
            info.home_unc().to_string_lossy(),
            r"\\wsl.localhost\Ubuntu\home\andrea"
        );
    }

    #[test]
    fn test_claude_projects_unc() {
        let info = WslInfo {
            distro: "Ubuntu".to_string(),
            user: "andrea".to_string(),
        };
        assert_eq!(
            info.claude_projects_unc().to_string_lossy(),
            r"\\wsl.localhost\Ubuntu\home\andrea\.claude\projects"
        );
    }
}
