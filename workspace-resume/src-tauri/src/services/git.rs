//! Git worktree + branch detection for project cards.
//!
//! All git calls run inside WSL via wsl.exe. To keep discovery fast we
//! batch-probe every project in a single wsl.exe invocation: one shell
//! script loops over paths from stdin and prints a small fixed record
//! per project. Per-path overhead is ~5ms inside WSL vs ~500ms to spawn
//! a new wsl.exe, so for 15 projects we save ~7 seconds.

use std::collections::HashMap;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::io::Write;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitInfo {
    /// Short branch name, e.g. `main`, `feat/add-voice`.
    pub branch: Option<String>,
    /// Absolute path of the worktree root (POSIX).
    pub worktree_root: Option<String>,
    /// Whether this is a linked worktree (not the primary one).
    pub is_linked_worktree: bool,
    /// How many worktrees exist for the main repo.
    pub worktree_count: u32,
}

// Delimiters chosen to be unlikely in git output
const REC_BEGIN: &str = "<<GIT_REC_BEGIN>>";
const REC_END: &str = "<<GIT_REC_END>>";
const FIELD_SEP: &str = "|";

/// Batch-probe many paths at once. Returns a map keyed by the input paths.
/// Any path that isn't a git repo or doesn't exist returns the default.
pub fn probe_many(paths: &[&str]) -> HashMap<String, GitInfo> {
    let mut out: HashMap<String, GitInfo> = HashMap::new();
    if paths.is_empty() {
        return out;
    }

    // Seed defaults so callers can assume the key is always present
    for p in paths {
        out.insert((*p).to_string(), GitInfo::default());
    }

    // Bash script reads paths one per line on stdin.
    // For each path: cd && run the 4 git commands and emit a single record.
    let script = format!(
        r#"
while IFS= read -r p; do
  [ -z "$p" ] && continue
  echo "{begin}"
  echo "P={FIELD}$p"
  if cd "$p" 2>/dev/null; then
    B=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)
    R=$(git rev-parse --show-toplevel 2>/dev/null || true)
    G=$(git rev-parse --git-dir 2>/dev/null || true)
    C=$(git rev-parse --git-common-dir 2>/dev/null || true)
    W=$(git worktree list --porcelain 2>/dev/null | grep -c '^worktree ' || echo 0)
    echo "B={FIELD}$B"
    echo "R={FIELD}$R"
    echo "G={FIELD}$G"
    echo "C={FIELD}$C"
    echo "W={FIELD}$W"
  fi
  echo "{end}"
done
"#,
        begin = REC_BEGIN,
        end = REC_END,
        FIELD = FIELD_SEP,
    );

    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("git batch spawn failed: {}", e);
            return out;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        for p in paths {
            let _ = writeln!(stdin, "{}", p);
        }
        drop(stdin);
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return out,
    };
    if !output.status.success() {
        return out;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    parse_batch_output(&stdout, &mut out);
    out
}

fn parse_batch_output(stdout: &str, out: &mut HashMap<String, GitInfo>) {
    let mut current_path: Option<String> = None;
    let mut current = GitInfo::default();
    let mut git_dir: Option<String> = None;
    let mut common_dir: Option<String> = None;

    for line in stdout.lines() {
        if line == REC_BEGIN {
            current_path = None;
            current = GitInfo::default();
            git_dir = None;
            common_dir = None;
            continue;
        }
        if line == REC_END {
            if let Some(p) = current_path.take() {
                // Determine linked-worktree status from git_dir vs common_dir
                current.is_linked_worktree = match (git_dir.as_deref(), common_dir.as_deref()) {
                    (Some(g), Some(c)) => g != c,
                    _ => false,
                };
                out.insert(p, std::mem::take(&mut current));
            }
            git_dir = None;
            common_dir = None;
            continue;
        }
        let Some((key, value)) = line.split_once(FIELD_SEP) else {
            continue;
        };
        match key {
            "P" => current_path = Some(value.to_string()),
            "B" => {
                if !value.is_empty() {
                    current.branch = Some(value.to_string());
                }
            }
            "R" => {
                if !value.is_empty() {
                    current.worktree_root = Some(value.to_string());
                }
            }
            "G" => {
                if !value.is_empty() {
                    git_dir = Some(value.to_string());
                }
            }
            "C" => {
                if !value.is_empty() {
                    common_dir = Some(value.to_string());
                }
            }
            "W" => {
                current.worktree_count = value.parse().unwrap_or(1);
            }
            _ => {}
        }
    }
}

/// Single-path convenience wrapper.
pub fn probe(path: &str) -> GitInfo {
    if path.is_empty() || path.starts_with("[unresolved]") {
        return GitInfo::default();
    }
    probe_many(&[path])
        .remove(path)
        .unwrap_or_default()
}

/// Create a new linked worktree at `<parent>-<slug>` with branch `<slug>`.
/// Returns the new worktree's absolute POSIX path on success.
pub fn create_worktree(project_path: &str, slug: &str) -> Result<String, String> {
    if slug.is_empty()
        || !slug
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/')
    {
        return Err("Invalid slug: use alphanumerics, dashes, underscores, slashes".into());
    }

    // Derive the sibling path
    let parent = std::path::Path::new(project_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or("Cannot derive parent directory")?;
    let basename = std::path::Path::new(project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Cannot derive basename")?;
    let new_name = format!("{}-{}", basename, slug.replace('/', "-"));
    let new_path = format!("{}/{}", parent.trim_end_matches('/'), new_name);

    let script = format!(
        "cd '{}' && git worktree add '{}' -b '{}' 2>&1",
        project_path.replace('\'', "'\\''"),
        new_path.replace('\'', "'\\''"),
        slug.replace('\'', "'\\''"),
    );
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", &script]);

    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run git worktree: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(new_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_path_returns_default() {
        let info = probe("");
        assert!(info.branch.is_none());
    }

    #[test]
    fn test_unresolved_path_returns_default() {
        let info = probe("[unresolved] foo");
        assert!(info.branch.is_none());
    }

    #[test]
    fn test_slug_validation() {
        assert!(create_worktree("/tmp", "").is_err());
        assert!(create_worktree("/tmp", "bad slug with space").is_err());
        assert!(create_worktree("/tmp", "bad;injection").is_err());
    }

    #[test]
    fn test_parse_batch_output_single_record() {
        let stdout = "<<GIT_REC_BEGIN>>\nP=|/home/andrea/proj\nB=|main\nR=|/home/andrea/proj\nG=|/home/andrea/proj/.git\nC=|/home/andrea/proj/.git\nW=|1\n<<GIT_REC_END>>\n";
        let mut out = HashMap::new();
        out.insert("/home/andrea/proj".to_string(), GitInfo::default());
        parse_batch_output(stdout, &mut out);
        let info = out.get("/home/andrea/proj").unwrap();
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.worktree_count, 1);
        assert!(!info.is_linked_worktree);
    }

    #[test]
    fn test_parse_batch_output_linked_worktree() {
        // Linked worktree: git_dir != common_dir
        let stdout = "<<GIT_REC_BEGIN>>\nP=|/home/andrea/proj-feat\nB=|feat\nR=|/home/andrea/proj-feat\nG=|/home/andrea/proj/.git/worktrees/feat\nC=|/home/andrea/proj/.git\nW=|2\n<<GIT_REC_END>>\n";
        let mut out = HashMap::new();
        out.insert("/home/andrea/proj-feat".to_string(), GitInfo::default());
        parse_batch_output(stdout, &mut out);
        let info = out.get("/home/andrea/proj-feat").unwrap();
        assert!(info.is_linked_worktree);
        assert_eq!(info.worktree_count, 2);
    }
}
