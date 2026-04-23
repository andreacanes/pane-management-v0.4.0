//! Generic read-modify-write helpers over the Tauri store `settings.json`.
//!
//! The store holds many unrelated top-level keys (`project_meta`,
//! `pane_presets`, `pane_assignments`, `session_order`, `pinned_order`,
//! `session_names`, `terminal_settings`, `error_log`, `companion.*`). The
//! canonical access pattern (see `.claude/rules/tauri-store-rmw.md`) is
//! `get(key) → mutate → set(key, new) → save()` per key, never
//! round-tripping one big struct and clobbering unrelated keys.
//!
//! Before these helpers existed, every `get_/set_` command-pair hand-wrote
//! its own load/save with the same five error strings ("Failed to open
//! settings store", "Failed to parse X", "Failed to serialize X", "Failed
//! to save store", etc.). This module collapses the shape to:
//!
//! ```ignore
//! let mut data = load_store_or_default::<HashMap<String, ProjectMeta>>(app, "project_meta")?;
//! data.insert(k, v);
//! save_store(app, "project_meta", &data)?;
//! ```
//!
//! Callers that need a special fallback (e.g. legacy-shape migration on
//! read) should keep their custom load path — `load_store` here is only
//! for the common "deserialize straight into T or return missing" case.

use serde::{de::DeserializeOwned, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "settings.json";

/// Load a single top-level key from the Tauri store and deserialize it
/// into `T`. Returns `Ok(None)` if the key is missing. Errors on any
/// store-open or deserialize failure with a human-readable context
/// string including the key name.
pub fn load_store<T: DeserializeOwned>(
    app: &AppHandle,
    key: &str,
) -> Result<Option<T>, String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get(key) {
        Some(value) => serde_json::from_value::<T>(value.clone())
            .map(Some)
            .map_err(|e| format!("Failed to parse {}: {}", key, e)),
        None => Ok(None),
    }
}

/// Load a single top-level key, returning `T::default()` if the key is
/// missing. Preferred over `load_store(...)?.unwrap_or_default()` at the
/// call site — same semantics, one line shorter, one `?` fewer.
pub fn load_store_or_default<T: DeserializeOwned + Default>(
    app: &AppHandle,
    key: &str,
) -> Result<T, String> {
    load_store(app, key).map(Option::unwrap_or_default)
}

/// Serialize `data` into the Tauri store under `key`, then persist to
/// disk. Does read-modify-write discipline via `store.set(key, ...)` —
/// unrelated keys in the same `settings.json` are untouched.
pub fn save_store<T: Serialize>(
    app: &AppHandle,
    key: &str,
    data: &T,
) -> Result<(), String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(data)
        .map_err(|e| format!("Failed to serialize {}: {}", key, e))?;

    store.set(key, value);
    store
        .save()
        .map_err(|e| format!("Failed to save store: {}", e))?;
    Ok(())
}
