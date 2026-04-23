use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSettings {
    #[serde(default = "default_tmux_session_name")]
    pub tmux_session_name: String,
}

fn default_tmux_session_name() -> String {
    "main".to_string()
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            tmux_session_name: default_tmux_session_name(),
        }
    }
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
    fn test_default_settings() {
        let settings = TerminalSettings::default();
        assert_eq!(settings.tmux_session_name, "main");
    }
}
