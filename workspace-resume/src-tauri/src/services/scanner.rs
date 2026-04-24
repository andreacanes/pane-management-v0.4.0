use std::path::Path;
use std::io::{BufRead, BufReader};
use std::fs::File;
use rev_lines::RevLines;

/// Parsed session metadata from a JSONL file.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    // Read from the JSONL header but not currently surfaced through the
    // `SessionInfo` DTO (that wire uses the filename-derived session_id).
    // Kept here so a future consumer that wants to verify the header/
    // filename agreement has the value already parsed.
    #[allow(dead_code)]
    pub session_id: Option<String>,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
    pub last_user_message: Option<String>,
    pub is_corrupted: bool,
}

const MAX_PARSE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
const WARN_SIZE: u64 = 50 * 1024 * 1024; // 50 MB
const MAX_MESSAGE_LEN: usize = 200;
/// How far into a JSONL file's header to scan for `sessionId` and
/// `timestamp`. Was 5; compacted sessions and sessions that open with
/// a large meta block blew past that and silently yielded
/// `first_timestamp: None` + `session_id: None` (surfaces as an
/// "unresolved" project or a session with no date). 100 is ample.
const HEADER_SCAN_CAP: usize = 100;

/// Check if a user record's content represents a real human message
/// (not a tool result, command output, or meta message).
fn is_real_user_message(val: &serde_json::Value) -> Option<String> {
    // Must be type "user"
    if val.get("type")?.as_str()? != "user" {
        return None;
    }
    // Skip isMeta records
    if val.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    // Get message content as string (not array)
    let content = val.pointer("/message/content")?.as_str()?;
    // Filter out tool results, command outputs
    if content.starts_with("[{")
        || content.starts_with("<command-")
        || content.starts_with("<local-command")
    {
        return None;
    }
    // Truncate to MAX_MESSAGE_LEN
    let truncated: String = content.chars().take(MAX_MESSAGE_LEN).collect();
    Some(truncated)
}

/// Parse session metadata from a JSONL file.
/// Uses reverse-read strategy for efficiency on large files.
pub fn parse_session_metadata(path: &Path) -> Result<SessionMeta, String> {
    let file_meta = std::fs::metadata(path).map_err(|e| format!("Cannot read file metadata: {}", e))?;
    let file_size = file_meta.len();

    // Empty file
    if file_size == 0 {
        return Ok(SessionMeta {
            session_id: None,
            first_timestamp: None,
            last_timestamp: None,
            last_user_message: None,
            is_corrupted: true,
        });
    }

    // Very large file -- return stub without parsing. Leave
    // last_user_message as None rather than the old bracketed literal
    // that rendered verbatim in session lists; the empty slot is more
    // honest than "[large file - skipped parsing]" masquerading as
    // user content.
    if file_size > MAX_PARSE_SIZE {
        eprintln!(
            "[scanner] session log exceeds {} MB, skipping parse: {}",
            MAX_PARSE_SIZE / 1024 / 1024,
            path.display()
        );
        return Ok(SessionMeta {
            session_id: None,
            first_timestamp: None,
            last_timestamp: None,
            last_user_message: None,
            is_corrupted: false,
        });
    }

    if file_size > WARN_SIZE {
        eprintln!("Warning: large session file ({} MB): {}", file_size / 1024 / 1024, path.display());
    }

    // Step 1: Read forward to get session_id and first_timestamp
    let mut session_id: Option<String> = None;
    let mut first_timestamp: Option<String> = None;
    let mut any_valid_record = false;

    {
        let file = File::open(path).map_err(|e| format!("Cannot open file: {}", e))?;
        let reader = BufReader::new(file);
        let mut scanned = 0usize;
        for line in reader.lines() {
            scanned += 1;
            if scanned > HEADER_SCAN_CAP {
                eprintln!(
                    "[scanner] gave up after {} header lines without sessionId+timestamp in {}",
                    HEADER_SCAN_CAP,
                    path.display()
                );
                break;
            }
            if let Ok(line) = line {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    any_valid_record = true;
                    if session_id.is_none() {
                        session_id = val.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string());
                    }
                    if first_timestamp.is_none() {
                        first_timestamp = val.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string());
                    }
                    if session_id.is_some() && first_timestamp.is_some() {
                        break;
                    }
                }
            }
        }
    }

    // Step 2: Read backward to find last user message and last timestamp
    let mut last_user_message: Option<String> = None;
    let mut last_timestamp: Option<String> = None;

    {
        let file = File::open(path).map_err(|e| format!("Cannot open file for reverse read: {}", e))?;
        let rev = RevLines::new(BufReader::new(file));

        for line_result in rev {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => continue,
            };

            if line.trim().is_empty() {
                continue;
            }

            let val = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(v) => {
                    any_valid_record = true;
                    v
                }
                Err(_) => continue,
            };

            // Fast path: last-prompt record
            if val.get("type").and_then(|v| v.as_str()) == Some("last-prompt") {
                if last_user_message.is_none() {
                    last_user_message = val.get("lastPrompt")
                        .and_then(|v| v.as_str())
                        .map(|s| s.chars().take(MAX_MESSAGE_LEN).collect());
                }
                // last-prompt records don't have timestamps, so continue looking
                // for a timestamped record for last_timestamp
                if last_timestamp.is_some() {
                    break;
                }
                continue;
            }

            // Track last timestamp from any record type
            if last_timestamp.is_none() {
                last_timestamp = val.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string());
            }

            // Slow path: check if this is a real user message
            if last_user_message.is_none() {
                last_user_message = is_real_user_message(&val);
            }

            // If we have both, stop
            if last_user_message.is_some() && last_timestamp.is_some() {
                break;
            }
        }
    }

    // If we couldn't parse any valid records at all, mark as corrupted
    if !any_valid_record {
        return Ok(SessionMeta {
            session_id: None,
            first_timestamp: None,
            last_timestamp: None,
            last_user_message: None,
            is_corrupted: true,
        });
    }

    Ok(SessionMeta {
        session_id,
        first_timestamp,
        last_timestamp,
        last_user_message,
        is_corrupted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn test_normal_session_parses_all_fields() {
        let meta = parse_session_metadata(&fixture("normal_session.jsonl")).unwrap();
        assert_eq!(meta.session_id, Some("abc-123".to_string()));
        assert_eq!(meta.first_timestamp, Some("2026-03-20T10:00:00Z".to_string()));
        assert_eq!(meta.last_timestamp, Some("2026-03-20T10:10:00Z".to_string()));
        assert_eq!(meta.last_user_message, Some("Now refactor the auth module".to_string()));
        assert!(!meta.is_corrupted);
    }

    #[test]
    fn test_last_prompt_fast_path() {
        let meta = parse_session_metadata(&fixture("last_prompt_session.jsonl")).unwrap();
        assert_eq!(meta.last_user_message, Some("Now refactor the auth module".to_string()));
        assert!(!meta.is_corrupted);
    }

    #[test]
    fn test_corrupted_session_marked() {
        let meta = parse_session_metadata(&fixture("corrupted_session.jsonl")).unwrap();
        assert!(meta.is_corrupted);
    }

    #[test]
    fn test_empty_session_marked_corrupted() {
        let meta = parse_session_metadata(&fixture("empty_session.jsonl")).unwrap();
        assert!(meta.is_corrupted);
    }

    #[test]
    fn test_meta_only_session_no_user_message() {
        let meta = parse_session_metadata(&fixture("meta_only_session.jsonl")).unwrap();
        assert_eq!(meta.last_user_message, None);
        assert!(!meta.is_corrupted);
    }

    #[test]
    fn test_filters_tool_results_and_meta() {
        let meta = parse_session_metadata(&fixture("normal_session.jsonl")).unwrap();
        assert_eq!(meta.last_user_message, Some("Now refactor the auth module".to_string()));
        assert!(!meta.last_user_message.as_ref().unwrap().starts_with("[{"));
    }
}
