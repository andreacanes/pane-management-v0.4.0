use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanePreset {
    pub name: String,
    pub layout: String,
    pub pane_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pane_preset_serialize_round_trip() {
        let preset = PanePreset {
            name: "dev-layout".to_string(),
            layout: "main-vertical".to_string(),
            pane_count: 3,
        };
        let json = serde_json::to_string(&preset).unwrap();
        let deserialized: PanePreset = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "dev-layout");
        assert_eq!(deserialized.layout, "main-vertical");
        assert_eq!(deserialized.pane_count, 3);
    }

    #[test]
    fn test_pane_preset_all_layouts() {
        let layouts = [
            "even-horizontal",
            "even-vertical",
            "main-horizontal",
            "main-vertical",
            "tiled",
        ];
        for layout in layouts {
            let preset = PanePreset {
                name: "test".to_string(),
                layout: layout.to_string(),
                pane_count: 2,
            };
            let json = serde_json::to_string(&preset).unwrap();
            let deserialized: PanePreset = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.layout, layout);
        }
    }
}
