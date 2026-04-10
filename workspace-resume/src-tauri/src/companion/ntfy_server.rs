//! Embedded ntfy-compatible publish + subscribe endpoints.
//!
//! Why embed instead of running a separate ntfy server?
//! - WSL2 mirrored-mode binds to IPv6 loopback only, so WSL ntfy isn't
//!   reachable from the Tailscale interface without `netsh portproxy`.
//! - Running ntfy on Windows adds a second long-lived process to manage.
//! - The companion already speaks HTTP; supporting the two ntfy endpoints
//!   the F-Droid app actually uses (`POST /<topic>` and `GET /<topic>/json`)
//!   is ~100 lines. Everything the app needs is here.
//!
//! Wire format matches https://docs.ntfy.sh/publish/:
//! - Publish via POST with headers: `Title`, `Priority`, `Tags`, `X-Actions`.
//! - Subscribe via GET /topic/json which emits newline-delimited JSON.
//!
//! Security model: topic is a random string stored in Tauri store.
//! Knowledge of the topic grants subscribe access; writes also require
//! knowing the topic. This is the same model ntfy.sh uses. Wrap the
//! whole thing behind Tailscale for network-level auth.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use base64::Engine;
use futures_util::stream::{self, StreamExt};
use rand::RngCore;
use tokio_stream::wrappers::BroadcastStream;

use super::state::{AppState, NtfyMessage};

const BACKLOG_MAX: usize = 64;

// ---------------------------------------------------------------------------
// Publish (POST /<topic>)
// ---------------------------------------------------------------------------

pub async fn publish(
    State(state): State<AppState>,
    Path(topic): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<axum::Json<NtfyMessage>, StatusCode> {
    // Only accept the registered topic
    if topic != *state.ntfy_topic {
        return Err(StatusCode::NOT_FOUND);
    }

    let title = headers
        .get("title")
        .or_else(|| headers.get("x-title"))
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let priority: u8 = headers
        .get("priority")
        .or_else(|| headers.get("x-priority"))
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let tags: Vec<String> = headers
        .get("tags")
        .or_else(|| headers.get("x-tags"))
        .and_then(|h| h.to_str().ok())
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
        .unwrap_or_default();

    let actions = headers
        .get("actions")
        .or_else(|| headers.get("x-actions"))
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let msg = NtfyMessage {
        id: short_id(),
        time: chrono::Utc::now().timestamp(),
        topic: topic.clone(),
        event: "message".to_string(),
        title,
        message: body,
        priority,
        tags,
        actions,
    };

    // Append to backlog + drop oldest past max
    {
        let mut backlog = state.ntfy_backlog.write().await;
        backlog.push(msg.clone());
        let overflow = backlog.len().saturating_sub(BACKLOG_MAX);
        if overflow > 0 {
            backlog.drain(0..overflow);
        }
    }
    let _ = state.ntfy_events.send(msg.clone());
    Ok(axum::Json(msg))
}

// ---------------------------------------------------------------------------
// Subscribe (GET /<topic>/json) — newline-delimited JSON stream
// ---------------------------------------------------------------------------

pub async fn subscribe_sse(
    State(state): State<AppState>,
    Path(topic): Path<String>,
) -> Result<Response, StatusCode> {
    if topic != *state.ntfy_topic {
        return Err(StatusCode::NOT_FOUND);
    }

    let rx = state.ntfy_events.subscribe();
    let backlog_snapshot: Vec<NtfyMessage> = state.ntfy_backlog.read().await.clone();

    // First emit an open-event, then the backlog, then live messages.
    let open_evt = serde_json::json!({
        "id": short_id(),
        "time": chrono::Utc::now().timestamp(),
        "event": "open",
        "topic": topic,
    });

    let open_stream = stream::iter(std::iter::once(Ok::<_, std::io::Error>(
        format!("{}\n", open_evt).into_bytes(),
    )));

    let backlog_stream = stream::iter(backlog_snapshot.into_iter().map(|m| {
        let j = serde_json::to_string(&m).unwrap_or_default();
        Ok::<_, std::io::Error>(format!("{}\n", j).into_bytes())
    }));

    let live_stream = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(msg) => {
                let j = serde_json::to_string(&msg).unwrap_or_default();
                Some(Ok::<_, std::io::Error>(format!("{}\n", j).into_bytes()))
            }
            Err(_lagged) => None,
        }
    });

    let combined = open_stream.chain(backlog_stream).chain(live_stream);
    let body = Body::from_stream(combined);

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(resp)
}

fn short_id() -> String {
    let mut buf = [0u8; 9];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}
