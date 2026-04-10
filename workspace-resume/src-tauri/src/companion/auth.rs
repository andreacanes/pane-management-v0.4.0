//! Bearer token middleware + hook shared-secret middleware.

use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use super::state::AppState;

pub async fn bearer_mw(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Allow WebSocket connections to carry the token via ?token= query
    // (browsers can't set Authorization headers on WS upgrade).
    let token_from_query = req.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.into_owned())
    });

    let token_from_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    let presented = token_from_header
        .or(token_from_query)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let expected = state.bearer.read().await.clone();
    if presented.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() != 1 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

pub async fn hook_secret_mw(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let secret = req
        .headers()
        .get("x-hook-secret")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if secret.as_bytes().ct_eq(state.hook_secret.as_bytes()).unwrap_u8() != 1 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}
