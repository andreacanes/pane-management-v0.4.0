//! Background poller that reads per-account Anthropic rate limit data.
//!
//! Reads from the Usage Monitor's cache file at
//! `%APPDATA%\ClaudeCodeUsageMonitor\usage_cache.json` — the Usage
//! Monitor already polls the Anthropic API and writes this file after
//! each successful fetch. Reading the cache avoids competing for the
//! API's per-token rate limit.
//!
//! The HTTP endpoint (`GET /api/v1/rate-limits`) serves the cached data
//! instantly — no API call is made in the request path.

use std::time::Duration;

use super::{accounts::ACCOUNTS, models::AccountRateLimit, state::AppState};

const POLL_INTERVAL: Duration = Duration::from_secs(15);

pub async fn run(state: AppState) {
    tokio::time::sleep(Duration::from_secs(2)).await;
    tracing::info!("rate_limit_poller started (reading Usage Monitor cache)");

    loop {
        let results = read_cache();
        if !results.is_empty() {
            *state.rate_limits.write().await = results;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read the Usage Monitor's cache file and map entries to our account
/// registry. The cache is a JSON array with objects like:
/// ```json
/// [
///   {"label":"A","five_hour_pct":13.0,"seven_day_pct":32.0,"five_hour_resets_at":1776052800},
///   {"label":"B","five_hour_pct":5.0,"seven_day_pct":12.0}
/// ]
/// ```
/// The Monitor uses "A"/"B" labels; we map them to accounts by index
/// (same order as [`ACCOUNTS`]).
fn read_cache() -> Vec<AccountRateLimit> {
    let cache_path = match dirs::data_dir() {
        Some(d) => d.join("ClaudeCodeUsageMonitor").join("usage_cache.json"),
        None => return Vec::new(),
    };

    let content = match std::fs::read_to_string(&cache_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let entries: Vec<serde_json::Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::with_capacity(ACCOUNTS.len());

    for (i, acct) in ACCOUNTS.iter().enumerate() {
        let entry = entries.get(i);
        let five_hour_pct = entry
            .and_then(|e| e.get("five_hour_pct"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let five_hour_resets_at = entry
            .and_then(|e| e.get("five_hour_resets_at"))
            .and_then(|v| v.as_i64());
        let seven_day_pct = entry
            .and_then(|e| e.get("seven_day_pct"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let seven_day_resets_at = entry
            .and_then(|e| e.get("seven_day_resets_at"))
            .and_then(|v| v.as_i64());

        out.push(AccountRateLimit {
            account: acct.key.to_string(),
            label: acct.label.to_string(),
            five_hour_pct,
            five_hour_resets_at,
            seven_day_pct,
            seven_day_resets_at,
        });
    }

    out
}
