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

/// Build a `pane_assignments` store key from its four coordinate parts.
/// The 4-segment key `"<host>|<session>|<window>|<pane>"` replaces the
/// legacy 3-segment form that implicitly assumed `host = "local"`.
/// Local keys now get an explicit `"local"` prefix so a local pane at
/// `main:0.3` and a Mac pane at `main:0.3` (different tmux servers)
/// don't collide.
pub fn build_key(host: &str, session: &str, window: u32, pane: u32) -> String {
    format!("{}|{}|{}|{}", host, session, window, pane)
}

/// Inverse of [`build_key`] that tolerates both the current 4-segment
/// form and the pre-refactor 3-segment form (legacy stores written
/// before the host-aware coordinate system landed). Legacy keys are
/// promoted to host `"local"` so callers never see the distinction.
///
/// Returns `None` for any key that parses into neither shape — e.g.
/// hand-corrupted store contents. The caller should skip-and-log
/// rather than abort load.
pub fn parse_key(key: &str) -> Option<(String, String, u32, u32)> {
    let parts: Vec<&str> = key.splitn(4, '|').collect();
    match parts.len() {
        4 => Some((
            parts[0].to_string(),
            parts[1].to_string(),
            parts[2].parse().ok()?,
            parts[3].parse().ok()?,
        )),
        3 => Some((
            "local".to_string(),
            parts[0].to_string(),
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        )),
        _ => None,
    }
}

/// Return `true` when a raw store key uses the legacy 3-segment form.
/// Drives the one-time migration rewrite in `load_pane_assignments`.
pub fn is_legacy_key(key: &str) -> bool {
    key.matches('|').count() == 2
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

    #[test]
    fn build_key_four_segments() {
        assert_eq!(build_key("local", "main", 0, 3), "local|main|0|3");
        assert_eq!(build_key("mac", "akamai", 0, 0), "mac|akamai|0|0");
    }

    #[test]
    fn parse_key_round_trip_four_segments() {
        let (h, s, w, p) = parse_key("mac|akamai-v3-bestbuy|0|2").unwrap();
        assert_eq!(h, "mac");
        assert_eq!(s, "akamai-v3-bestbuy");
        assert_eq!(w, 0);
        assert_eq!(p, 2);
    }

    #[test]
    fn parse_key_legacy_three_segments_promotes_to_local() {
        let (h, s, w, p) = parse_key("main|0|3").unwrap();
        assert_eq!(h, "local");
        assert_eq!(s, "main");
        assert_eq!(w, 0);
        assert_eq!(p, 3);
    }

    #[test]
    fn parse_key_rejects_garbage() {
        assert!(parse_key("nope").is_none());
        assert!(parse_key("one|two").is_none());
        assert!(parse_key("mac|main|not-a-number|3").is_none());
    }

    #[test]
    fn is_legacy_key_only_three_segments() {
        assert!(is_legacy_key("main|0|3"));
        assert!(!is_legacy_key("local|main|0|3"));
        assert!(!is_legacy_key("one|two"));
    }

    #[test]
    fn parse_key_malformed_extra_segment_rejects() {
        // tmux forbids `|` inside session names, so we treat any extra
        // segment as corruption rather than trying to round-trip it.
        // `splitn(4)` keeps the overflow in the last segment, making
        // the pane-index parse fail → parse_key returns None.
        assert!(parse_key("local|weird|session|0|3").is_none());
    }
}
