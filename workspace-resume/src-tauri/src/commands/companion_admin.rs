//! Tauri commands that expose companion config (bearer token, ntfy topic,
//! bind addr) + QR code for phone onboarding + token rotation.

use serde::Serialize;
use tauri::Manager;
use tauri_plugin_store::StoreExt;

#[derive(Debug, Serialize)]
pub struct CompanionConfig {
    pub bearer_token: String,
    pub hook_secret: String,
    pub ntfy_topic: String,
    pub port: u16,
    pub bind: String,
    /// Best-guess URL to show the user for phone setup.
    /// Prefers a Tailscale IP if present, otherwise returns the LAN IP.
    pub suggested_url: String,
}

/// Read the companion config from the Tauri store.
#[tauri::command]
pub async fn get_companion_config(app: tauri::AppHandle) -> Result<CompanionConfig, String> {
    let store = app
        .store("settings.json")
        .map_err(|e| format!("store open failed: {}", e))?;

    let bearer_token = store
        .get("companion.bearer_token")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    let hook_secret = store
        .get("companion.hook_secret")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    let ntfy_topic = store
        .get("companion.ntfy_topic")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    let port = crate::companion::COMPANION_PORT;
    let bind = format!("{}:{}", crate::companion::BIND_ADDR, port);
    let suggested_url = find_suggested_url(port);

    Ok(CompanionConfig {
        bearer_token,
        hook_secret,
        ntfy_topic,
        port,
        bind,
        suggested_url,
    })
}

/// Generate a setup QR code as an SVG data URL.
/// Encodes: `pmgmt://setup?url=<base>&token=<bearer>`
#[tauri::command]
pub async fn get_companion_qr(app: tauri::AppHandle) -> Result<String, String> {
    let cfg = get_companion_config(app).await?;
    let uri = format!(
        "pmgmt://setup?url={}&token={}",
        urlencoding::encode(&cfg.suggested_url),
        urlencoding::encode(&cfg.bearer_token),
    );
    let code = qrcode::QrCode::new(uri.as_bytes())
        .map_err(|e| format!("qrcode error: {}", e))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .dark_color(qrcode::render::svg::Color("#111"))
        .light_color(qrcode::render::svg::Color("#fff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(svg.as_bytes())
        }
    ))
}

/// Rotate the bearer token. In-flight WS clients will start getting 401s.
/// Also updates the runtime state so the new token takes effect immediately
/// (without restarting the app) and purges stale ntfy backlog entries whose
/// embedded X-Actions headers reference the old token.
#[tauri::command]
pub async fn rotate_companion_token(app: tauri::AppHandle) -> Result<CompanionConfig, String> {
    use base64::Engine;
    use rand::RngCore;

    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let new_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);

    let store = app
        .store("settings.json")
        .map_err(|e| format!("store open failed: {}", e))?;
    store.set("companion.bearer_token", serde_json::json!(new_token.clone()));
    let _ = store.save();

    // Update the runtime bearer so auth middleware uses the new token
    // immediately, and purge ntfy backlog entries that embed the old
    // bearer in their action buttons. Attention notifications (no
    // actions) are kept; only approval notifications are purged.
    if let Some(state) = app.try_state::<crate::companion::state::AppState>() {
        let state: &crate::companion::state::AppState = &state;
        *state.bearer.write().await = new_token;
        let mut backlog = state.ntfy_backlog.write().await;
        backlog.retain(|m| m.actions.is_none());
    }

    get_companion_config(app).await
}

/// Find a reasonable URL to suggest to the user for phone setup.
/// Uses the Windows network interfaces to find either a Tailscale IP
/// (100.x.y.z range by convention) or the first private LAN IP.
fn find_suggested_url(port: u16) -> String {
    #[cfg(windows)]
    {
        if let Some(ip) = detect_best_local_ip() {
            return format!("http://{}:{}", ip, port);
        }
    }
    format!("http://127.0.0.1:{}", port)
}

#[cfg(windows)]
fn detect_best_local_ip() -> Option<String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;

    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-Command",
        "Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.PrefixOrigin -ne 'WellKnown' -and $_.IPAddress -ne '127.0.0.1' } | Sort-Object -Property @{Expression={if ($_.IPAddress -like '100.*') {0} else {1}}} | Select-Object -First 1 -ExpandProperty IPAddress",
    ]);
    cmd.creation_flags(0x08000000);
    let output = cmd.output().ok()?;
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        None
    } else {
        Some(ip)
    }
}
