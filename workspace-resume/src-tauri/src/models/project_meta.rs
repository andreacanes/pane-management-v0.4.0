use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTier {
    Pinned,
    Active,
    Paused,
    Archived,
}

impl Default for ProjectTier {
    fn default() -> Self {
        ProjectTier::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub display_name: Option<String>,
    pub tier: ProjectTier,
    pub bound_session: Option<String>,
    /// Filesystem inode — stable across renames/moves on the same volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
    /// All Claude Code encoded directory names associated with this project.
    /// Populated when a directory rename is detected — old + new dirs under one project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_project_dirs: Option<Vec<String>>,
}

impl Default for ProjectMeta {
    fn default() -> Self {
        Self {
            display_name: None,
            tier: ProjectTier::default(),
            bound_session: None,
            inode: None,
            claude_project_dirs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_tier_default_is_active() {
        assert_eq!(ProjectTier::default(), ProjectTier::Active);
    }

    #[test]
    fn test_project_tier_serialize() {
        let json = serde_json::to_string(&ProjectTier::Pinned).unwrap();
        assert_eq!(json, "\"pinned\"");

        let json = serde_json::to_string(&ProjectTier::Active).unwrap();
        assert_eq!(json, "\"active\"");

        let json = serde_json::to_string(&ProjectTier::Paused).unwrap();
        assert_eq!(json, "\"paused\"");

        let json = serde_json::to_string(&ProjectTier::Archived).unwrap();
        assert_eq!(json, "\"archived\"");
    }

    #[test]
    fn test_project_tier_deserialize() {
        let tier: ProjectTier = serde_json::from_str("\"pinned\"").unwrap();
        assert_eq!(tier, ProjectTier::Pinned);

        let tier: ProjectTier = serde_json::from_str("\"active\"").unwrap();
        assert_eq!(tier, ProjectTier::Active);

        let tier: ProjectTier = serde_json::from_str("\"paused\"").unwrap();
        assert_eq!(tier, ProjectTier::Paused);

        let tier: ProjectTier = serde_json::from_str("\"archived\"").unwrap();
        assert_eq!(tier, ProjectTier::Archived);
    }

    #[test]
    fn test_project_tier_round_trip() {
        for tier in [
            ProjectTier::Pinned,
            ProjectTier::Active,
            ProjectTier::Paused,
            ProjectTier::Archived,
        ] {
            let json = serde_json::to_string(&tier).unwrap();
            let deserialized: ProjectTier = serde_json::from_str(&json).unwrap();
            assert_eq!(tier, deserialized);
        }
    }

    #[test]
    fn test_project_meta_default() {
        let meta = ProjectMeta::default();
        assert_eq!(meta.display_name, None);
        assert_eq!(meta.tier, ProjectTier::Active);
        assert_eq!(meta.bound_session, None);
    }

    #[test]
    fn test_project_meta_serialize_round_trip() {
        let meta = ProjectMeta {
            display_name: Some("My Project".to_string()),
            tier: ProjectTier::Pinned,
            bound_session: Some("session-123".to_string()),
            inode: None,
            claude_project_dirs: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: ProjectMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.display_name, Some("My Project".to_string()));
        assert_eq!(deserialized.tier, ProjectTier::Pinned);
        assert_eq!(deserialized.bound_session, Some("session-123".to_string()));
    }

    #[test]
    fn test_project_meta_default_serialize_round_trip() {
        let meta = ProjectMeta::default();
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: ProjectMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.display_name, None);
        assert_eq!(deserialized.tier, ProjectTier::Active);
        assert_eq!(deserialized.bound_session, None);
    }

    #[test]
    fn test_project_meta_with_null_fields() {
        let json = r#"{"display_name":null,"tier":"paused","bound_session":null}"#;
        let meta: ProjectMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.display_name, None);
        assert_eq!(meta.tier, ProjectTier::Paused);
        assert_eq!(meta.bound_session, None);
    }
}
