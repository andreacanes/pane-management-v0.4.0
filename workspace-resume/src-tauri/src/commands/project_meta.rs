use std::collections::HashMap;

use crate::models::pane_preset::PanePreset;
use crate::models::project_meta::{ProjectMeta, ProjectTier};

// ---------------------
// Store helpers
// ---------------------

fn load_project_meta(app: &tauri::AppHandle) -> Result<HashMap<String, ProjectMeta>, String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("project_meta") {
        Some(value) => serde_json::from_value::<HashMap<String, ProjectMeta>>(value.clone())
            .map_err(|e| format!("Failed to parse project_meta: {}", e)),
        None => Ok(HashMap::new()),
    }
}

fn save_project_meta(
    app: &tauri::AppHandle,
    data: &HashMap<String, ProjectMeta>,
) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(data)
        .map_err(|e| format!("Failed to serialize project_meta: {}", e))?;

    store.set("project_meta", value);
    store
        .save()
        .map_err(|e| format!("Failed to save store: {}", e))?;
    Ok(())
}

fn load_pane_presets(app: &tauri::AppHandle) -> Result<HashMap<String, PanePreset>, String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("pane_presets") {
        Some(value) => serde_json::from_value::<HashMap<String, PanePreset>>(value.clone())
            .map_err(|e| format!("Failed to parse pane_presets: {}", e)),
        None => Ok(HashMap::new()),
    }
}

fn save_pane_presets(
    app: &tauri::AppHandle,
    data: &HashMap<String, PanePreset>,
) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(data)
        .map_err(|e| format!("Failed to serialize pane_presets: {}", e))?;

    store.set("pane_presets", value);
    store
        .save()
        .map_err(|e| format!("Failed to save store: {}", e))?;
    Ok(())
}

fn load_pane_assignments(app: &tauri::AppHandle) -> Result<HashMap<String, String>, String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("pane_assignments") {
        Some(value) => serde_json::from_value::<HashMap<String, String>>(value.clone())
            .map_err(|e| format!("Failed to parse pane_assignments: {}", e)),
        None => Ok(HashMap::new()),
    }
}

fn save_pane_assignments(
    app: &tauri::AppHandle,
    data: &HashMap<String, String>,
) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(data)
        .map_err(|e| format!("Failed to serialize pane_assignments: {}", e))?;

    store.set("pane_assignments", value);
    store
        .save()
        .map_err(|e| format!("Failed to save store: {}", e))?;
    Ok(())
}

// ---------------------
// Session order IPC Commands
// ---------------------

fn load_session_order(app: &tauri::AppHandle) -> Result<Vec<String>, String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("session_order") {
        Some(value) => serde_json::from_value::<Vec<String>>(value.clone())
            .map_err(|e| format!("Failed to parse session_order: {}", e)),
        None => Ok(vec![]),
    }
}

fn save_session_order(app: &tauri::AppHandle, order: &[String]) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(order)
        .map_err(|e| format!("Failed to serialize session_order: {}", e))?;

    store.set("session_order", value);
    store
        .save()
        .map_err(|e| format!("Failed to save store: {}", e))?;
    Ok(())
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
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    match store.get("pinned_order") {
        Some(value) => serde_json::from_value::<Vec<String>>(value.clone())
            .map_err(|e| format!("Failed to parse pinned_order: {}", e)),
        None => Ok(vec![]),
    }
}

fn save_pinned_order(app: &tauri::AppHandle, order: &[String]) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;

    let store = app
        .store("settings.json")
        .map_err(|e| format!("Failed to open settings store: {}", e))?;

    let value = serde_json::to_value(order)
        .map_err(|e| format!("Failed to serialize pinned_order: {}", e))?;

    store.set("pinned_order", value);
    store
        .save()
        .map_err(|e| format!("Failed to save store: {}", e))?;
    Ok(())
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

/// Build a scoped key for pane assignments: "session|window|pane_index"
fn pane_key(session: &str, window: u32, pane: u32) -> String {
    format!("{}|{}|{}", session, window, pane)
}

/// Filter assignments for a specific session+window, returning just pane_index → project.
fn filter_assignments(
    all: &HashMap<String, String>,
    session: &str,
    window: u32,
) -> HashMap<String, String> {
    let prefix = format!("{}|{}|", session, window);
    all.iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(k, v)| {
            let pane_idx = k.strip_prefix(&prefix).unwrap_or(k).to_string();
            (pane_idx, v.clone())
        })
        .collect()
}

#[tauri::command]
pub async fn get_pane_assignments(
    session_name: String,
    window_index: u32,
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    let all = load_pane_assignments(&app)?;
    Ok(filter_assignments(&all, &session_name, window_index))
}

/// Return ALL pane assignments unfiltered (for resurrect — needs full cross-session state).
#[tauri::command]
pub async fn get_pane_assignments_raw(
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    load_pane_assignments(&app)
}

#[tauri::command]
pub async fn set_pane_assignment(
    session_name: String,
    window_index: u32,
    pane_index: u32,
    encoded_project: Option<String>,
    app: tauri::AppHandle,
) -> Result<HashMap<String, String>, String> {
    let mut all = load_pane_assignments(&app)?;
    let key = pane_key(&session_name, window_index, pane_index);

    match encoded_project {
        Some(project) => {
            all.insert(key, project);
        }
        None => {
            all.remove(&key);
        }
    }

    save_pane_assignments(&app, &all)?;
    Ok(filter_assignments(&all, &session_name, window_index))
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
