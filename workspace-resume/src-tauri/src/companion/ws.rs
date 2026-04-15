//! WebSocket broadcast for live pane and approval events.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};

use super::{
    models::{now_ms, EventDto},
    state::AppState,
};

pub async fn upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client_loop(socket, state))
}

async fn client_loop(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();

    // Send an initial snapshot so the client has full state on connect.
    {
        let panes = state.panes.read().await;
        let approvals = state.approvals.read().await;
        let snapshot = EventDto::Snapshot {
            panes: panes.values().map(|r| r.dto.clone()).collect(),
            approvals: approvals.values().map(|a| a.dto.clone()).collect(),
        };
        if let Ok(s) = serde_json::to_string(&snapshot) {
            let _ = socket.send(Message::Text(s.into())).await;
        }
    }

    // Send a synthetic "connected" event with timestamp for latency checks.
    {
        let hello = EventDto::Hello { at: now_ms() };
        if let Ok(s) = serde_json::to_string(&hello) {
            let _ = socket.send(Message::Text(s.into())).await;
        }
    }

    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if let Ok(s) = serde_json::to_string(&ev) {
                        if socket.send(Message::Text(s.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Ping(p))) => {
                    let _ = socket.send(Message::Pong(p)).await;
                }
                Some(Ok(Message::Close(_))) | None => break,
                _ => {}
            }
        }
    }
}
