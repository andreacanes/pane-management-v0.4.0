//! Pane-slot persistence record.
//!
//! Grew from a bare `String` (encoded project) to a struct that also
//! carries per-slot `host` and `account` selection. Legacy stores written
//! before this evolution contain plain strings — they are tolerated via
//! [`RawAssignment`] and promoted to [`PaneAssignment`] on read. The next
//! write migrates them to the modern shape automatically.

use serde::{Deserialize, Serialize};

fn default_host() -> String {
    "local".to_string()
}

fn default_account() -> String {
    "andrea".to_string()
}

/// A pane's configured intent: which project, which host, which account.
///
/// Stored under the Tauri store's `pane_assignments` key, scoped by
/// `"<session>|<window>|<pane_index>"`.
///
/// * `host` — `"local"` (WSL) or a remote SSH alias such as `"mac"`.
/// * `account` — `"andrea"` | `"bravura"` | `"sully"`. Matches the wire
///   vocabulary used by `companion::accounts` and `src/lib/account.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneAssignment {
    pub encoded_project: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_account")]
    pub account: String,
}

impl PaneAssignment {
    pub fn new_local(encoded_project: String) -> Self {
        Self {
            encoded_project,
            host: default_host(),
            account: default_account(),
        }
    }
}

/// Deserialization shim tolerating both the legacy string-only shape
/// (`pane_assignments: { "main|0|0": "C--..." }`) and the modern struct
/// shape. Converts to `PaneAssignment` via [`From`] / [`Into`] so callers
/// can treat both uniformly.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RawAssignment {
    Modern(PaneAssignment),
    LegacyString(String),
}

impl From<RawAssignment> for PaneAssignment {
    fn from(raw: RawAssignment) -> Self {
        match raw {
            RawAssignment::Modern(m) => m,
            RawAssignment::LegacyString(s) => PaneAssignment::new_local(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn modern_round_trip() {
        let a = PaneAssignment {
            encoded_project: "C--Users-Andrea-foo".into(),
            host: "mac".into(),
            account: "sully".into(),
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: PaneAssignment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn modern_missing_fields_use_defaults() {
        let json = r#"{"encoded_project":"foo"}"#;
        let a: PaneAssignment = serde_json::from_str(json).unwrap();
        assert_eq!(a.encoded_project, "foo");
        assert_eq!(a.host, "local");
        assert_eq!(a.account, "andrea");
    }

    #[test]
    fn legacy_string_promotes_to_local_andrea() {
        let json = r#""C--Users-Andrea-foo""#;
        let raw: RawAssignment = serde_json::from_str(json).unwrap();
        let promoted: PaneAssignment = raw.into();
        assert_eq!(promoted.encoded_project, "C--Users-Andrea-foo");
        assert_eq!(promoted.host, "local");
        assert_eq!(promoted.account, "andrea");
    }

    #[test]
    fn raw_map_mixes_legacy_and_modern() {
        let json = r#"{
            "main|0|0": "C--legacy",
            "main|0|1": {"encoded_project":"C--modern","host":"mac","account":"sully"}
        }"#;
        let raw: HashMap<String, RawAssignment> = serde_json::from_str(json).unwrap();
        let promoted: HashMap<String, PaneAssignment> =
            raw.into_iter().map(|(k, v)| (k, v.into())).collect();

        let legacy = &promoted["main|0|0"];
        assert_eq!(legacy.encoded_project, "C--legacy");
        assert_eq!(legacy.host, "local");
        assert_eq!(legacy.account, "andrea");

        let modern = &promoted["main|0|1"];
        assert_eq!(modern.encoded_project, "C--modern");
        assert_eq!(modern.host, "mac");
        assert_eq!(modern.account, "sully");
    }

    #[test]
    fn new_local_sets_defaults() {
        let a = PaneAssignment::new_local("enc".into());
        assert_eq!(a.encoded_project, "enc");
        assert_eq!(a.host, "local");
        assert_eq!(a.account, "andrea");
    }
}
