//! Claude Code usage aggregation and cost estimation.
//!
//! Parses `~/.claude/projects/<proj>/<uuid>.jsonl` files for the
//! `message.usage` field on assistant records and sums tokens + cost
//! per session and per project. Pricing table matches Anthropic's
//! published rates (April 2026, USD per 1M tokens). Opus 4.7 ships at
//! the same rates as Opus 4.6, with the 1M context window included at
//! standard pricing (no long-context premium).
//!
//! Models are matched by prefix to absorb future point revisions
//! (e.g. `claude-opus-4-7` matches `opus-4-7-<datestamp>` etc.).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Pricing {
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_write_per_mtok: f64,
    cache_read_per_mtok: f64,
}

/// Anthropic public pricing (April 2026). Update when prices change.
fn pricing_for(model: &str) -> Pricing {
    let m = model.to_lowercase();
    if m.contains("opus-4-7")
        || m.contains("opus-4-6")
        || m.contains("opus-4-5")
        || m.contains("opus-4.7")
        || m.contains("opus-4.6")
    {
        Pricing {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_write_per_mtok: 18.75,
            cache_read_per_mtok: 1.50,
        }
    } else if m.contains("opus-4") || m.contains("opus-3") {
        Pricing {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_write_per_mtok: 18.75,
            cache_read_per_mtok: 1.50,
        }
    } else if m.contains("sonnet-4") || m.contains("sonnet-4-6") || m.contains("sonnet-4.6") {
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_per_mtok: 3.75,
            cache_read_per_mtok: 0.30,
        }
    } else if m.contains("haiku-4") || m.contains("haiku-4-5") {
        Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_write_per_mtok: 1.25,
            cache_read_per_mtok: 0.10,
        }
    } else if m.contains("haiku-3") {
        Pricing {
            input_per_mtok: 0.80,
            output_per_mtok: 4.0,
            cache_write_per_mtok: 1.0,
            cache_read_per_mtok: 0.08,
        }
    } else {
        // Unknown model: default to Sonnet rates as a conservative middle
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_per_mtok: 3.75,
            cache_read_per_mtok: 0.30,
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionUsage {
    pub session_id: String,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub message_count: u64,
    /// USD estimate. Uses the most-recently-seen model for every token;
    /// if a session mixes models we cap at the last one for simplicity.
    pub cost_usd: f64,
}

impl SessionUsage {
    // Sum of all token categories — used by tests and intended for a
    // future session-card "total tokens" label on the UI. Kept public
    // so the sibling `ProjectUsage::total_tokens` stays symmetric.
    #[allow(dead_code)]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_write_tokens + self.cache_read_tokens
    }

    fn accumulate(&mut self, tokens: &TokenBreakdown, model: &str) {
        self.input_tokens += tokens.input;
        self.output_tokens += tokens.output;
        self.cache_write_tokens += tokens.cache_write;
        self.cache_read_tokens += tokens.cache_read;
        self.message_count += 1;
        self.model = Some(model.to_string());

        let p = pricing_for(model);
        self.cost_usd += (tokens.input as f64 / 1_000_000.0) * p.input_per_mtok
            + (tokens.output as f64 / 1_000_000.0) * p.output_per_mtok
            + (tokens.cache_write as f64 / 1_000_000.0) * p.cache_write_per_mtok
            + (tokens.cache_read as f64 / 1_000_000.0) * p.cache_read_per_mtok;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectUsage {
    pub encoded_name: String,
    pub sessions: Vec<SessionUsage>,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_write: u64,
    pub total_cache_read: u64,
    pub total_cost_usd: f64,
    pub total_messages: u64,
}

impl ProjectUsage {
    // Sum across all sessions; test-covered and intended for an
    // aggregate project view that's on the backlog but not shipped.
    #[allow(dead_code)]
    pub fn total_tokens(&self) -> u64 {
        self.total_input + self.total_output + self.total_cache_write + self.total_cache_read
    }

    fn add_session(&mut self, s: SessionUsage) {
        self.total_input += s.input_tokens;
        self.total_output += s.output_tokens;
        self.total_cache_write += s.cache_write_tokens;
        self.total_cache_read += s.cache_read_tokens;
        self.total_cost_usd += s.cost_usd;
        self.total_messages += s.message_count;
        self.sessions.push(s);
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct TokenBreakdown {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const MAX_USAGE_PARSE_SIZE: u64 = 200 * 1024 * 1024; // 200 MB cap

/// Parse a single JSONL session file for usage. Returns None if the
/// file is too large, empty, or unreadable.
pub fn parse_session_usage(path: &Path) -> Option<SessionUsage> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 || meta.len() > MAX_USAGE_PARSE_SIZE {
        return None;
    }

    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut usage = SessionUsage::default();
    usage.session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Only assistant records carry usage
        if val.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(msg) = val.get("message") else {
            continue;
        };
        let model = msg
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let Some(u) = msg.get("usage") else {
            continue;
        };
        let tokens = TokenBreakdown {
            input: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            output: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            cache_write: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read: u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };
        usage.accumulate(&tokens, &model);
    }

    if usage.message_count == 0 {
        return None;
    }
    Some(usage)
}

/// Walk a project's JSONL directory and aggregate usage across all sessions.
pub fn parse_project_usage(project_dir: &Path, encoded_name: &str) -> ProjectUsage {
    let mut project = ProjectUsage {
        encoded_name: encoded_name.to_string(),
        ..Default::default()
    };

    let entries = match std::fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return project,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().map(|e| e != "jsonl").unwrap_or(true) {
            continue;
        }
        if let Some(session) = parse_session_usage(&path) {
            project.add_session(session);
        }
    }

    project
}

/// Aggregate usage across every project directory in one of the
/// given root paths. Used for the cross-project "all my usage" view.
pub fn parse_all_projects_usage(project_roots: &[&Path]) -> HashMap<String, ProjectUsage> {
    let mut out = HashMap::new();
    for root in project_roots {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let encoded_name = entry.file_name().to_string_lossy().to_string();
            if out.contains_key(&encoded_name) {
                continue; // first scanned root wins
            }
            let usage = parse_project_usage(&path, &encoded_name);
            if usage.total_messages > 0 {
                out.insert(encoded_name, usage);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_opus_4_6() {
        let p = pricing_for("claude-opus-4-6");
        assert!((p.input_per_mtok - 15.0).abs() < 1e-9);
        assert!((p.output_per_mtok - 75.0).abs() < 1e-9);
        assert!((p.cache_read_per_mtok - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_opus_4_7_matches_4_6() {
        let p = pricing_for("claude-opus-4-7");
        assert!((p.input_per_mtok - 15.0).abs() < 1e-9);
        assert!((p.output_per_mtok - 75.0).abs() < 1e-9);
        assert!((p.cache_write_per_mtok - 18.75).abs() < 1e-9);
        assert!((p.cache_read_per_mtok - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_opus_4_7_dated_suffix() {
        let p = pricing_for("claude-opus-4-7-20260415");
        assert!((p.input_per_mtok - 15.0).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_sonnet_4() {
        let p = pricing_for("claude-sonnet-4-6-20260304");
        assert!((p.input_per_mtok - 3.0).abs() < 1e-9);
        assert!((p.output_per_mtok - 15.0).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_unknown_falls_back_to_sonnet() {
        let p = pricing_for("claude-unknown-model-xyz");
        assert!((p.input_per_mtok - 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_session_usage_accumulate_opus() {
        let mut s = SessionUsage::default();
        s.accumulate(
            &TokenBreakdown {
                input: 1_000_000,
                output: 1_000_000,
                cache_write: 1_000_000,
                cache_read: 1_000_000,
            },
            "claude-opus-4-6",
        );
        // $15 + $75 + $18.75 + $1.50 = $110.25
        assert!((s.cost_usd - 110.25).abs() < 1e-6);
        assert_eq!(s.total_tokens(), 4_000_000);
        assert_eq!(s.message_count, 1);
    }
}
