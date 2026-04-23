use crate::models::project::ProjectInfo;
use crate::models::session::SessionInfo;
use crate::services::path_decoder;
use crate::services::scanner;
use crate::services::wsl;

/// Convert a WSL path like /mnt/c/Users/... to a Windows path C:\Users\...
/// so that path_exists checks work from the Windows side.
fn wsl_path_to_windows(path: &str) -> Option<String> {
    if path.starts_with("/mnt/") && path.len() >= 7 && path.as_bytes()[5].is_ascii_alphabetic() && path.as_bytes()[6] == b'/' {
        let drive = path.as_bytes()[5].to_ascii_uppercase() as char;
        Some(format!("{}:{}", drive, path[6..].replace('/', "\\")))
    } else {
        None
    }
}

/// Check if a path exists, handling WSL paths by converting to Windows format.
fn check_path_exists(actual_path: &str) -> bool {
    // Try the path as-is first (works for Windows paths like C:\Users\...)
    if std::path::Path::new(actual_path).exists() {
        return true;
    }
    // If it's a WSL /mnt/ path, convert to Windows and try again
    if let Some(win_path) = wsl_path_to_windows(actual_path) {
        return std::path::Path::new(&win_path).exists();
    }
    // For WSL-native paths like /home/andrea/..., check via UNC
    if actual_path.starts_with('/') {
        if let Some(info) = wsl::wsl_info() {
            let unc = format!(
                r"\\wsl.localhost\{}{}",
                info.distro,
                actual_path.replace('/', "\\")
            );
            return std::path::Path::new(&unc).exists();
        }
    }
    false
}

/// Returns true if the given actual_path looks like the WSL user's home directory
/// itself (not a sub-project). Claude Code creates a `-home-<user>` JSONL dir when
/// running `claude` from the home dir; it's not a real project.
fn is_home_root(actual_path: &str) -> bool {
    if let Some(info) = wsl::wsl_info() {
        let home = format!("/home/{}", info.user);
        if actual_path == home || actual_path == format!("{}/", home) {
            return true;
        }
    }
    false
}

/// Scan a single projects directory and collect ProjectInfo entries.
fn scan_projects_dir(
    projects_dir: &std::path::Path,
    seen: &mut std::collections::HashSet<String>,
    projects: &mut Vec<(ProjectInfo, std::time::SystemTime)>,
) {
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[discovery] Cannot read {}: {}", projects_dir.display(), e);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Warning: skipping unreadable directory entry: {}", e);
                continue;
            }
        };

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let encoded_name = entry.file_name().to_string_lossy().to_string();

        // Skip if we've already seen this encoded name (dedup across scan dirs)
        if seen.contains(&encoded_name) {
            continue;
        }
        seen.insert(encoded_name.clone());

        // Count .jsonl files and find the most recently modified one
        let mut session_count = 0usize;
        let mut latest_modified = std::time::SystemTime::UNIX_EPOCH;
        if let Ok(rd) = std::fs::read_dir(&path) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() && p.extension().map(|ext| ext == "jsonl").unwrap_or(false) {
                    session_count += 1;
                    if let Ok(meta) = p.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if modified > latest_modified {
                                latest_modified = modified;
                            }
                        }
                    }
                }
            }
        }

        // Skip empty project dirs (no sessions)
        if session_count == 0 {
            continue;
        }

        // Get actual path from cwd field in first JSONL record
        let actual_path = path_decoder::extract_cwd_from_first_record(&path)
            .unwrap_or_else(|| {
                format!("[unresolved] {}", encoded_name)
            });

        // Skip the bogus "home root" project that Claude Code creates when run from $HOME
        if is_home_root(&actual_path) {
            continue;
        }

        // Per locked decision: missing folders still returned, frontend prompts user
        let path_exists = check_path_exists(&actual_path);

        // Git info is filled in later by a single batched wsl.exe call
        projects.push((ProjectInfo {
            encoded_name,
            actual_path,
            session_count,
            path_exists,
            git_branch: None,
            is_linked_worktree: false,
            worktree_count: 0,
        }, latest_modified));
    }
}

#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectInfo>, String> {
    let mut projects: Vec<(ProjectInfo, std::time::SystemTime)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // 1. Scan WSL home ~/.claude/projects/ via UNC path (primary — where active sessions live)
    if let Some(info) = wsl::wsl_info() {
        let wsl_projects = info.claude_projects_unc();
        if wsl_projects.exists() {
            scan_projects_dir(&wsl_projects, &mut seen, &mut projects);
        }
    }

    // 2. Scan Windows home ~/.claude/projects/ (secondary — legacy or Windows-native sessions)
    if let Some(win_home) = dirs::home_dir() {
        let win_projects = win_home.join(".claude").join("projects");
        if win_projects.exists() {
            scan_projects_dir(&win_projects, &mut seen, &mut projects);
        }
    }

    // Sort by most recently modified session file (newest first)
    projects.sort_by(|a, b| b.1.cmp(&a.1));
    let mut projects: Vec<ProjectInfo> = projects.into_iter().map(|(p, _)| p).collect();

    // Batch-probe git info for every resolvable path in a single wsl.exe call.
    // ~1 wsl.exe spawn total vs N spawns if we did it per-project.
    let paths: Vec<&str> = projects
        .iter()
        .filter(|p| p.path_exists && !p.actual_path.starts_with("[unresolved]"))
        .map(|p| p.actual_path.as_str())
        .collect();
    if !paths.is_empty() {
        let git_map = crate::services::git::probe_many(&paths);
        for proj in projects.iter_mut() {
            if let Some(info) = git_map.get(&proj.actual_path) {
                proj.git_branch = info.branch.clone();
                proj.is_linked_worktree = info.is_linked_worktree;
                proj.worktree_count = info.worktree_count;
            }
        }
    }

    Ok(projects)
}

/// Find the project directory by checking WSL first, then Windows home.
fn find_project_dir(encoded_project: &str) -> Option<std::path::PathBuf> {
    // Check WSL home first (primary)
    if let Some(info) = wsl::wsl_info() {
        let wsl_dir = info.claude_projects_unc().join(encoded_project);
        if wsl_dir.exists() {
            return Some(wsl_dir);
        }
    }
    // Check Windows home (secondary)
    if let Some(win_home) = dirs::home_dir() {
        let win_dir = win_home.join(".claude").join("projects").join(encoded_project);
        if win_dir.exists() {
            return Some(win_dir);
        }
    }
    None
}

#[tauri::command]
pub async fn list_sessions(encoded_project: String) -> Result<Vec<SessionInfo>, String> {
    let project_dir = find_project_dir(&encoded_project)
        .ok_or_else(|| format!("Project directory not found: {}", encoded_project))?;

    let mut sessions = Vec::new();

    let entries = std::fs::read_dir(&project_dir)
        .map_err(|e| format!("Cannot read project directory: {}", e))?;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Warning: skipping unreadable entry: {}", e);
                continue;
            }
        };

        let path = entry.path();

        // Only process .jsonl files that are actual files (not directories)
        if !path.is_file() || path.extension().map(|e| e != "jsonl").unwrap_or(true) {
            continue;
        }

        let session_id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let file_size_bytes = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Per locked decision: corrupted sessions shown with warning, never skipped
        match scanner::parse_session_metadata(&path) {
            Ok(meta) => {
                sessions.push(SessionInfo {
                    session_id,
                    first_timestamp: meta.first_timestamp,
                    last_timestamp: meta.last_timestamp,
                    last_user_message: meta.last_user_message,
                    is_corrupted: meta.is_corrupted,
                    file_size_bytes,
                });
            }
            Err(e) => {
                eprintln!("Warning: failed to parse session {}: {}", session_id, e);
                sessions.push(SessionInfo {
                    session_id,
                    first_timestamp: None,
                    last_timestamp: None,
                    last_user_message: None,
                    is_corrupted: true,
                    file_size_bytes,
                });
            }
        }
    }

    // Sort by last_timestamp descending (most recent first)
    // Sessions with no timestamp sort to the end
    sessions.sort_by(|a, b| {
        match (&b.last_timestamp, &a.last_timestamp) {
            (Some(b_ts), Some(a_ts)) => b_ts.cmp(a_ts),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    Ok(sessions)
}

#[tauri::command]
pub async fn delete_session(encoded_project: String, session_id: String) -> Result<(), String> {
    let project_dir = find_project_dir(&encoded_project)
        .ok_or_else(|| format!("Project directory not found: {}", encoded_project))?;

    let session_file = project_dir.join(format!("{}.jsonl", session_id));
    if !session_file.exists() {
        return Err(format!("Session file not found: {}", session_id));
    }

    std::fs::remove_file(&session_file)
        .map_err(|e| format!("Failed to delete session: {}", e))?;

    // Also remove the session subdirectory (subagent transcripts) if it exists
    let session_dir = project_dir.join(&session_id);
    if session_dir.is_dir() {
        let _ = std::fs::remove_dir_all(&session_dir);
    }

    Ok(())
}

#[tauri::command]
pub async fn check_continuity_exists(path: String) -> Result<bool, String> {
    // Convert WSL path to Windows if needed (Rust runs on Windows, not WSL)
    let win_path = if path.starts_with("/mnt/") {
        let rest = &path[5..]; // strip "/mnt/"
        if rest.len() >= 2 && rest.as_bytes()[1] == b'/' {
            let drive = rest.as_bytes()[0].to_ascii_uppercase() as char;
            format!("{}:{}", drive, rest[1..].replace('/', "\\"))
        } else {
            path.clone()
        }
    } else {
        path.clone()
    };
    let base = std::path::Path::new(&win_path);
    Ok(base.join(".continuity").is_dir() || base.join("active-planning-files").is_dir())
}

#[tauri::command]
pub async fn open_directory(path: String) -> Result<(), String> {
    open::that(&path).map_err(|e| format!("Failed to open directory: {}", e))
}

/// Resolve any supported path flavor to a Windows-side filesystem path usable
/// by `std::fs`. Mirrors the three-branch logic in `check_path_exists`.
fn resolve_to_windows_fs(path: &str) -> Option<String> {
    // Already Windows-style — backslashes, UNC, or drive letter form.
    if path.contains('\\') {
        return Some(path.to_string());
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return Some(path.to_string());
    }
    if let Some(win) = wsl_path_to_windows(path) {
        return Some(win);
    }
    if path.starts_with('/') {
        if let Some(info) = wsl::wsl_info() {
            return Some(format!(
                r"\\wsl.localhost\{}{}",
                info.distro,
                path.replace('/', "\\")
            ));
        }
    }
    None
}

/// Create a new project folder under `parent` named `name`. Returns the full
/// path in the same slash style the caller used for `parent`, so the UI can
/// pass it straight into the existing new-project flow.
#[tauri::command]
pub async fn create_project_folder(parent: String, name: String) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    if name == "." || name == ".." {
        return Err("Folder name cannot be '.' or '..'".to_string());
    }
    const INVALID: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];
    if name.chars().any(|c| INVALID.contains(&c)) {
        return Err(format!("Folder name contains invalid characters: {}", name));
    }

    let parent_raw = parent.trim();
    if parent_raw.is_empty() {
        return Err("Parent path cannot be empty".to_string());
    }
    let parent_clean_trim = parent_raw.trim_end_matches(|c| c == '/' || c == '\\');
    let parent_clean = if parent_clean_trim.is_empty() {
        parent_raw
    } else {
        parent_clean_trim
    };

    let parent_fs = resolve_to_windows_fs(parent_clean)
        .ok_or_else(|| format!("Could not resolve parent path: {}", parent_clean))?;
    let parent_path = std::path::Path::new(&parent_fs);
    if !parent_path.is_dir() {
        return Err(format!(
            "Parent is not an existing directory: {}",
            parent_clean
        ));
    }

    let new_fs = parent_path.join(name);
    if new_fs.exists() {
        return Err(format!(
            "A file or folder named \"{}\" already exists at that location",
            name
        ));
    }

    std::fs::create_dir_all(&new_fs).map_err(|e| format!("Failed to create folder: {}", e))?;

    let sep = if parent_clean.contains('/') && !parent_clean.contains('\\') {
        '/'
    } else {
        '\\'
    };
    Ok(format!("{}{}{}", parent_clean, sep, name))
}

/// Get the filesystem inode for a directory path.
/// Runs via WSL, so uses Unix inode. The actual path may be on NTFS
/// (via /mnt/c/) but WSL exposes a stable inode for NTFS files.
/// Returns None if the path doesn't exist.
#[tauri::command]
pub async fn get_inode(path: String) -> Result<Option<u64>, String> {
    // The Rust binary runs on Windows, but the paths we're checking are
    // WSL paths accessed via wsl.exe. Use a WSL stat call to get the inode.
    let script = format!("stat -c '%i' '{}' 2>/dev/null || echo 'NOTFOUND'", path.replace('\'', "'\\''"));

    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", &script]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let output = cmd.output()
        .map_err(|e| format!("Failed to run stat via WSL: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout == "NOTFOUND" || stdout.is_empty() {
        return Ok(None);
    }

    match stdout.parse::<u64>() {
        Ok(inode) => Ok(Some(inode)),
        Err(_) => Ok(None),
    }
}

/// Search for a directory with a specific inode within a root directory tree.
/// Uses WSL `find` + `stat` for efficiency. Bounded by max_depth.
/// Returns the WSL path if found, None if not.
#[tauri::command]
pub async fn find_inode_in_tree(
    root: String,
    target_inode: u64,
    max_depth: u32,
) -> Result<Option<String>, String> {
    // Use find to enumerate directories, stat each one, compare inodes
    let script = format!(
        "find '{}' -maxdepth {} -type d -exec stat -c '%i %n' {{}} \\; 2>/dev/null | grep '^{} ' | head -1 | cut -d' ' -f2-",
        root.replace('\'', "'\\''"),
        max_depth,
        target_inode
    );

    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", &script]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let output = cmd.output()
        .map_err(|e| format!("Failed to search via WSL: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stdout))
    }
}

