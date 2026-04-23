use std::collections::HashMap;

use crate::models::pane_assignment::{
    build_key, is_legacy_key, parse_key, PaneAssignment, RawAssignment,
};
use crate::models::pane_preset::PanePreset;
use crate::models::project_meta::{ProjectMeta, ProjectTier};
use crate::services::store::{load_store, load_store_or_default, save_store};

// ---------------------
// Store helpers
// ---------------------

fn load_project_meta(app: &tauri::AppHandle) -> Result<HashMap<String, ProjectMeta>, String> {
    load_store_or_default(app, "project_meta")
}

fn save_project_meta(
    app: &tauri::AppHandle,
    data: &HashMap<String, ProjectMeta>,
) -> Result<(), String> {
    save_store(app, "project_meta", data)
}

fn load_pane_presets(app: &tauri::AppHandle) -> Result<HashMap<String, PanePreset>, String> {
    load_store_or_default(app, "pane_presets")
}

fn save_pane_presets(
    app: &tauri::AppHandle,
    data: &HashMap<String, PanePreset>,
) -> Result<(), String> {
    save_store(app, "pane_presets", data)
}

/// Load pane assignments with two layered migrations:
///
/// 1. Value-shape: legacy bare-string values (`"C--..."`) get promoted
///    to full [`PaneAssignment`] structs via [`RawAssignment`]. Default
///    host = `"local"`, default account = `"andrea"`.
/// 2. Key-shape: legacy 3-segment keys (`"main|0|3"`) get rewritten to
///    4-segment (`"local|main|0|3"`) so local and remote panes that
///    share a coord cannot collide. The rewrite happens in-memory and
///    is flushed to the store only when at least one key changed; the
///    caller's next `save_pane_assignments` keeps it idempotent.
///
/// Corrupted keys (parse_key returns None) are dropped with a warning
/// rather than aborting load — losing one malformed entry beats an app
/// that won't start.
fn load_pane_assignments(app: &tauri::AppHandle) -> Result<HashMap<String, PaneAssignment>, String> {
    let raw: Option<HashMap<String, RawAssignment>> = load_store(app, "pane_assignments")?;
    let src: HashMap<String, PaneAssignment> = raw
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, v.into()))
        .collect();

    let mut needs_migration = false;
    let mut migrated: HashMap<String, PaneAssignment> = HashMap::with_capacity(src.len());
    for (k, v) in src {
        if is_legacy_key(&k) {
            needs_migration = true;
            match parse_key(&k) {
                Some((host, session, window, pane)) => {
                    let new_k = build_key(&host, &session, window, pane);
                    migrated.insert(new_k, v);
                }
                None => {
                    eprintln!("[project_meta] dropping malformed pane_assignment key: {}", k);
                }
            }
        } else if parse_key(&k).is_some() {
            migrated.insert(k, v);
        } else {
            eprintln!("[project_meta] dropping malformed pane_assignment key: {}", k);
            needs_migration = true;
        }
    }

    if needs_migration {
        // One-shot rewrite so future loads skip the migration path and
        // the sweep + frontend both see consistent 4-segment keys.
        if let Err(e) = save_pane_assignments(app, &migrated) {
            eprintln!(
                "[project_meta] pane_assignments migration save failed: {} (continuing with in-memory copy)",
                e
            );
        }
    }

    Ok(migrated)
}

fn save_pane_assignments(
    app: &tauri::AppHandle,
    data: &HashMap<String, PaneAssignment>,
) -> Result<(), String> {
    save_store(app, "pane_assignments", data)
}

/// Flatten the full-struct map down to the legacy `encoded_project`-only
/// shape used by the current Tauri IPC wire. Slice C widens the wire to
/// return the full struct; until then, readers get a plain string map
/// and are oblivious to host/account fields.
fn flatten_assignments(
    full: &HashMap<String, PaneAssignment>,
) -> HashMap<String, String> {
    full.iter()
        .map(|(k, v)| (k.clone(), v.encoded_project.clone()))
        .collect()
}

// ---------------------
// Session order IPC Commands
// ---------------------

fn load_session_order(app: &tauri::AppHandle) -> Result<Vec<String>, String> {
    load_store_or_default(app, "session_order")
}

fn save_session_order(app: &tauri::AppHandle, order: &[String]) -> Result<(), String> {
    save_store(app, "session_order", &order)
}

#[tauri::command]
pub async fn get_session_order(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    load_session_order(&app)
}

#[tauri::command]
pub async fn set_session_order(
    order: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    save_session_order(&app, &order)
}

// ---------------------
// Pinned order IPC Commands
// ---------------------

fn load_pinned_order(app: &tauri::AppHandle) -> Result<Vec<String>, String> {
    load_store_or_default(app, "pinned_order")
}

fn save_pinned_order(app: &tauri::AppHandle, order: &[String]) -> Result<(), String> {
    save_store(app, "pinned_order", &order)
}

#[tauri::command]
pub async fn get_pinned_order(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    load_pinned_order(&app)
}

#[tauri::command]
pub async fn set_pinned_order(
    order: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    save_pinned_order(&app, &order)
}

// ---------------------
// Project metadata IPC Commands
// ---------------------

#[tauri::command]
pub async fn get_all_project_meta(
    app: tauri::AppHandle,
) -> Result<HashMap<String, ProjectMeta>, String> {
    load_project_meta(&app)
}

#[tauri::command]
pub async fn set_project_tier(
    encoded_name: String,
    tier: String,
    app: tauri::AppHandle,
) -> Result<ProjectMeta, String> {
    // Parse tier string into ProjectTier enum
    let tier_enum: ProjectTier = serde_json::from_value(serde_json::Value::String(tier))
        .map_err(|e| format!("Invalid tier value: {}", e))?;

    let mut meta_map = load_project_meta(&app)?;
    let entry = meta_map
        .entry(encoded_name)
        .or_insert_with(ProjectMeta::default);
    entry.tier = tier_enum;
    let result = entry.clone();
    save_project_meta(&app, &meta_map)?;
    Ok(result)
}

#[tauri::command]
pub async fn set_display_name(
    encoded_name: String,
    name: Option<String>,
    app: tauri::AppHandle,
) -> Result<ProjectMeta, String> {
    let mut meta_map = load_project_meta(&app)?;
    let entry = meta_map
        .entry(encoded_name)
        .or_insert_with(ProjectMeta::default);
    entry.display_name = name;
    let result = entry.clone();
    save_project_meta(&app, &meta_map)?;
    Ok(result)
}

#[tauri::command]
pub async fn set_session_binding(
    encoded_name: String,
    session_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<ProjectMeta, String> {
    let mut meta_map = load_project_meta(&app)?;
    let entry = meta_map
        .entry(encoded_name)
        .or_insert_with(ProjectMeta::default);
    entry.bound_session = session_id;
    let result = entry.clone();
    save_project_meta(&app, &meta_map)?;
    Ok(result)
}

/// Update a project's inode and/or claude_project_dirs.
/// Used by the orphan detection system to store inodes on discovery
/// and link renamed directories.
#[tauri::command]
pub async fn update_project_inode(
    encoded_name: String,
    inode: Option<u64>,
    claude_project_dirs: Option<Vec<String>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut meta_map = load_project_meta(&app)?;
    let entry = meta_map
        .entry(encoded_name)
        .or_insert_with(ProjectMeta::default);
    if let Some(i) = inode {
        entry.inode = Some(i);
    }
    if let Some(dirs) = claude_project_dirs {
        entry.claude_project_dirs = Some(dirs);
    }
    save_project_meta(&app, &meta_map)?;
    Ok(())
}

// ---------------------
// Pane preset IPC Commands
// ---------------------

#[tauri::command]
pub async fn get_pane_presets(
    app: tauri::AppHandle,
) -> Result<Vec<PanePreset>, String> {
    let presets = load_pane_presets(&app)?;
    Ok(presets.into_values().collect())
}

#[tauri::command]
pub async fn save_pane_preset(
    name: String,
    layout: String,
    pane_count: u32,
    app: tauri::AppHandle,
) -> Result<PanePreset, String> {
    let preset = PanePreset {
        name: name.clone(),
        layout,
        pane_count,
    };
    let mut presets = load_pane_presets(&app)?;
    presets.insert(name, preset.clone());
    save_pane_presets(&app, &presets)?;
    Ok(preset)
}

#[tauri::command]
pub async fn delete_pane_preset(
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut presets = load_pane_presets(&app)?;
    presets.remove(&name);
    save_pane_presets(&app, &presets)?;
    Ok(())
}

/// Scoped key for pane assignments: "host|session|window|pane_index".
/// Thin alias over [`build_key`] so every call site in this module has
/// the same naming as the other pane-coordinate helpers that sit
/// alongside it.
fn pane_key(host: &str, session: &str, window: u32, pane: u32) -> String {
    build_key(host, session, window, pane)
}

// ---------------------
// Session-name store helpers
// ---------------------
//
// Top-level key `session_names` holds a map-of-maps keyed on encoded
// project name, with the inner map mapping session-UUID → user-chosen
// display name. Pre-cleanup this state lived in localStorage under keys
// like `session-names:<encoded>`; moving it here brings it under the
// same read-modify-write discipline as the rest of the settings store
// and ensures it survives a webview origin change or store backup.

fn load_session_names_all(
    app: &tauri::AppHandle,
) -> Result<HashMap<String, HashMap<String, String>>, String> {
    load_store_or_default(app, "session_names")
}

fn save_session_names_all(
    app: &tauri::AppHandle,
    data: &HashMap<String, HashMap<String, String>>,
) -> Result<(), String> {
    save_store(app, "session_names", data)
}

#[tauri::command]
pub async fn get_session_names(
    encoded_project: String,
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    let all = load_session_names_all(&app)?;
    Ok(all.get(&encoded_project).cloned().unwrap_or_default())
}

#[tauri::command]
pub async fn set_session_names(
    encoded_project: String,
    names: HashMap<String, String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut all = load_session_names_all(&app)?;
    if names.is_empty() {
        all.remove(&encoded_project);
    } else {
        all.insert(encoded_project, names);
    }
    save_session_names_all(&app, &all)
}

/// Filter the (already-flattened) 4-segment-keyed map down to one
/// `(host, session, window)` scope, returning `pane_index → encoded_project`.
/// The pane_index is the only distinguishing coordinate left after the
/// scope is fixed, so callers can continue treating the result as a
/// per-window map keyed by pane_index (as the pre-refactor wire did).
fn filter_assignments(
    all: &HashMap<String, String>,
    host: &str,
    session: &str,
    window: u32,
) -> HashMap<String, String> {
    all.iter()
        .filter_map(|(k, v)| {
            let (h, s, w, p) = parse_key(k)?;
            if h == host && s == session && w == window {
                Some((p.to_string(), v.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Per-scope pane-index → encoded_project map for one `(host, session, window)`.
/// Preserves the pre-refactor wire shape so every caller that used the
/// old `get_pane_assignments(session, window)` just gains a required
/// leading `host` arg. For local panes pass `host = "local"`.
#[tauri::command]
pub async fn get_pane_assignments(
    host: String,
    session_name: String,
    window_index: u32,
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    let full = load_pane_assignments(&app)?;
    Ok(filter_assignments(
        &flatten_assignments(&full),
        &host,
        &session_name,
        window_index,
    ))
}

/// Return ALL pane assignments unfiltered, keyed on the full 4-segment
/// coord (`host|session|window|pane`). Used by resurrect and by the
/// AppContext grid to know every assigned slot across every host.
#[tauri::command]
pub async fn get_pane_assignments_raw(
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    let full = load_pane_assignments(&app)?;
    Ok(flatten_assignments(&full))
}

/// Return ALL pane assignments as full [`PaneAssignment`] structs —
/// used by the tmux_poller to discover active remote hosts and
/// synthesize `claude_account` for remote panes.
pub fn get_pane_assignments_full_sync(
    app: &tauri::AppHandle,
) -> Result<HashMap<String, PaneAssignment>, String> {
    load_pane_assignments(app)
}

/// Per-scope full-struct map for one `(host, session, window)`, keyed
/// by pane_index. Drives the frontend's per-slot host/account badges
/// and the launcher's per-slot host/account resolution.
#[tauri::command]
pub async fn get_pane_assignments_full(
    host: String,
    session_name: String,
    window_index: u32,
    app: tauri::AppHandle,
) -> Result<HashMap<String, PaneAssignment>, String> {
    let all = load_pane_assignments(&app)?;
    Ok(all
        .into_iter()
        .filter_map(|(k, v)| {
            let (h, s, w, p) = parse_key(&k)?;
            if h == host && s == session_name && w == window_index {
                Some((p.to_string(), v))
            } else {
                None
            }
        })
        .collect())
}

/// All assignments keyed by the full 4-segment coord string. Used by
/// the frontend to merge local + remote assignments in one reactive
/// store; reads once and filters per-pane in JS rather than calling
/// the Tauri bridge once per slot.
#[tauri::command]
pub async fn get_all_pane_assignments_full(
    app: tauri::AppHandle,
) -> Result<HashMap<String, PaneAssignment>, String> {
    load_pane_assignments(&app)
}

/// Create / update / delete the assignment at `(host, session, window, pane)`.
/// Host must be part of the coordinate now — a pane at `main:0.3` on
/// Mac is a different slot from a pane at `main:0.3` on local WSL.
///
/// When `encoded_project` is `Some`, host and account on an existing
/// entry are preserved; a new entry starts with the passed host and
/// `account = "andrea"`. When `None`, the slot is cleared.
///
/// Returns the post-mutation pane_index → encoded_project map for the
/// same `(host, session, window)` scope so the frontend can refresh
/// state in one round-trip.
#[tauri::command]
pub async fn set_pane_assignment(
    host: String,
    session_name: String,
    window_index: u32,
    pane_index: u32,
    encoded_project: Option<String>,
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    let mut all = load_pane_assignments(&app)?;
    let key = pane_key(&host, &session_name, window_index, pane_index);

    match encoded_project {
        Some(project) => match all.get_mut(&key) {
            Some(existing) => {
                existing.encoded_project = project;
            }
            None => {
                let mut fresh = PaneAssignment::new_local(project);
                // Preserve the caller's host intent on first create —
                // the default of `"local"` only kicks in when the
                // frontend omits host (legacy callers in transition).
                fresh.host = if host.is_empty() { "local".to_string() } else { host.clone() };
                all.insert(key, fresh);
            }
        },
        None => {
            all.remove(&key);
        }
    }

    save_pane_assignments(&app, &all)?;
    Ok(filter_assignments(
        &flatten_assignments(&all),
        &host,
        &session_name,
        window_index,
    ))
}

/// Mutate the `host` / `account` metadata of an existing assignment at
/// `(host, session, window, pane)`. The `host` param here is part of
/// the **lookup key**; changing it is only supported via delete+recreate
/// (this command errors if you try to mutate host to a different value
/// than the slot already has). Use `set_pane_assignment(None)` then
/// `set_pane_assignment(Some(project))` with the new host to move a
/// slot between hosts.
#[tauri::command]
pub async fn set_pane_assignment_meta(
    host: String,
    session_name: String,
    window_index: u32,
    pane_index: u32,
    account: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut all = load_pane_assignments(&app)?;
    let key = pane_key(&host, &session_name, window_index, pane_index);

    match all.get_mut(&key) {
        Some(entry) => {
            // Host is part of the key, so a found entry already matches
            // the caller's host. We only mutate account here — the slot's
            // host is immutable under the host-aware coord model (move
            // a slot between hosts via delete+recreate in
            // `set_pane_assignment` instead).
            entry.account = account;
        }
        None => {
            return Err(format!(
                "no pane assignment at {} — assign a project before setting account",
                key
            ));
        }
    }

    save_pane_assignments(&app, &all)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::project_meta::{ProjectMeta, ProjectTier};
    use crate::models::pane_preset::PanePreset;

    #[test]
    fn test_project_meta_default_for_new_entry() {
        let meta = ProjectMeta::default();
        assert_eq!(meta.display_name, None);
        assert_eq!(meta.tier, ProjectTier::Active);
        assert_eq!(meta.bound_session, None);
    }

    #[test]
    fn test_project_tier_from_string_via_serde() {
        let tier: ProjectTier =
            serde_json::from_value(serde_json::Value::String("pinned".to_string())).unwrap();
        assert_eq!(tier, ProjectTier::Pinned);

        let tier: ProjectTier =
            serde_json::from_value(serde_json::Value::String("archived".to_string())).unwrap();
        assert_eq!(tier, ProjectTier::Archived);
    }

    #[test]
    fn test_project_tier_invalid_string_fails() {
        let result: Result<ProjectTier, _> =
            serde_json::from_value(serde_json::Value::String("invalid".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_project_meta_hashmap_serialization() {
        let mut map = HashMap::new();
        map.insert(
            "C--Users-USERNAME-project".to_string(),
            ProjectMeta {
                display_name: Some("My Project".to_string()),
                tier: ProjectTier::Pinned,
                bound_session: None,
                inode: None,
                claude_project_dirs: None,
            },
        );
        let json = serde_json::to_string(&map).unwrap();
        let deserialized: HashMap<String, ProjectMeta> = serde_json::from_str(&json).unwrap();
        let entry = deserialized.get("C--Users-USERNAME-project").unwrap();
        assert_eq!(entry.display_name, Some("My Project".to_string()));
        assert_eq!(entry.tier, ProjectTier::Pinned);
    }

    #[test]
    fn test_pane_preset_hashmap_serialization() {
        let mut map = HashMap::new();
        map.insert(
            "dev-layout".to_string(),
            PanePreset {
                name: "dev-layout".to_string(),
                layout: "main-vertical".to_string(),
                pane_count: 3,
            },
        );
        let json = serde_json::to_string(&map).unwrap();
        let deserialized: HashMap<String, PanePreset> = serde_json::from_str(&json).unwrap();
        let preset = deserialized.get("dev-layout").unwrap();
        assert_eq!(preset.layout, "main-vertical");
        assert_eq!(preset.pane_count, 3);
    }

    #[test]
    fn test_pane_assignments_hashmap_serialization() {
        let mut map = HashMap::new();
        map.insert("0".to_string(), "C--Users-USERNAME-project".to_string());
        map.insert("1".to_string(), "C--Users-USERNAME-other".to_string());
        let json = serde_json::to_string(&map).unwrap();
        let deserialized: HashMap<String, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.get("0").unwrap(),
            "C--Users-USERNAME-project"
        );
        assert_eq!(
            deserialized.get("1").unwrap(),
            "C--Users-USERNAME-other"
        );
    }

    #[test]
    fn test_pane_assignment_remove_logic() {
        // Test the assignment insert/remove logic without the store
        let mut assignments: HashMap<String, String> = HashMap::new();

        // Insert
        let key = "0".to_string();
        assignments.insert(key.clone(), "project-a".to_string());
        assert_eq!(assignments.get("0").unwrap(), "project-a");

        // Update
        assignments.insert(key.clone(), "project-b".to_string());
        assert_eq!(assignments.get("0").unwrap(), "project-b");

        // Remove
        assignments.remove(&key);
        assert!(assignments.get("0").is_none());
    }
}
