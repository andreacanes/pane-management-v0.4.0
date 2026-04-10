use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TerminalBackend {
    Tmux,
    Warp,
    Powershell,
}

impl Default for TerminalBackend {
    fn default() -> Self {
        TerminalBackend::Tmux
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSettings {
    pub backend: TerminalBackend,
    #[serde(default = "default_tmux_session_name")]
    pub tmux_session_name: String,
}

fn default_tmux_session_name() -> String {
    "workspace".to_string()
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            backend: TerminalBackend::default(),
            tmux_session_name: default_tmux_session_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResumeResult {
    pub pid: Option<u32>,
    pub terminal: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorLogEntry {
    pub timestamp: String,
    pub terminal: String,
    pub error: String,
    pub project_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_backend_is_tmux() {
        assert_eq!(TerminalBackend::default(), TerminalBackend::Tmux);
    }

    #[test]
    fn test_default_settings() {
        let settings = TerminalSettings::default();
        assert_eq!(settings.backend, TerminalBackend::Tmux);
        assert_eq!(settings.tmux_session_name, "workspace");
    }

    #[test]
    fn test_backend_serialization() {
        let backend = TerminalBackend::Tmux;
        let json = serde_json::to_string(&backend).unwrap();
        assert_eq!(json, "\"tmux\"");

        let backend = TerminalBackend::Warp;
        let json = serde_json::to_string(&backend).unwrap();
        assert_eq!(json, "\"warp\"");

        let backend = TerminalBackend::Powershell;
        let json = serde_json::to_string(&backend).unwrap();
        assert_eq!(json, "\"powershell\"");
    }

    #[test]
    fn test_backend_deserialization() {
        let backend: TerminalBackend = serde_json::from_str("\"tmux\"").unwrap();
        assert_eq!(backend, TerminalBackend::Tmux);

        let backend: TerminalBackend = serde_json::from_str("\"warp\"").unwrap();
        assert_eq!(backend, TerminalBackend::Warp);

        let backend: TerminalBackend = serde_json::from_str("\"powershell\"").unwrap();
        assert_eq!(backend, TerminalBackend::Powershell);
    }
}
