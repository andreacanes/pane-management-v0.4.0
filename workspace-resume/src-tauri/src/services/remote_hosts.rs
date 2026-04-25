//! Single source of truth for "which remote SSH hosts does the app know about".
//!
//! Two callers need this answer:
//!   * `companion/http.rs::list_sessions` — to fan `/api/v1/sessions` out
//!     across every reachable remote tmux server.
//!   * `companion/tmux_poller.rs::poll_loop` — to fan the 1s pane poll
//!     out across every reachable remote tmux server.
//!
//! Both used to compute the union inline; the implementations had drifted
//! slightly (different fallback behaviour) before this lift. The HTTP
//! version is the one preserved here — its fallback ("only insert 'mac'
//! when no remote host is referenced anywhere") is the more conservative
//! and more correct of the two.
//!
//! The result is a sorted `Vec<String>` so iteration order is stable
//! across processes — clients that re-render on diffs don't get spurious
//! reorders.
//!
//! Sync (no `.await`) because both underlying reads are sync:
//!   * `get_pane_assignments_full_sync` — direct store read.
//!   * `load_store_or_default::<Vec<String>>(_, "remote_hosts")` — same.
//! `mac_sync::list_remote_hosts` is async only because Tauri commands
//! must be; its body is identical to `load_store_or_default`. We don't
//! need the indirection here.

use std::collections::BTreeSet;
use tauri::AppHandle;

/// Distinct non-local SSH aliases the app is currently aware of, sorted.
///
/// Union of:
///   1. Hosts referenced by any `pane_assignment` (live work).
///   2. Hosts in the persisted `remote_hosts` Tauri store key
///      (configured but not yet assigned).
///
/// Plus a first-run fallback of `["mac"]` when the user has neither
/// configured nor assigned any remote — matching `list_remote_hosts`'s
/// default so a brand-new install doesn't look local-only across the
/// app's sessions / panes / poller surfaces.
pub fn collect_remote_hosts(app: &AppHandle) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // Union 1: pane_assignments. Even if remote_hosts has been emptied,
    // a host with active work should still be reachable so its session
    // doesn't silently vanish from the wire.
    if let Ok(m) = crate::commands::project_meta::get_pane_assignments_full_sync(app) {
        for a in m.values() {
            if !a.host.is_empty() && a.host != "local" {
                seen.insert(a.host.clone());
            }
        }
    }

    // Union 2: configured store. Lets a brand-new Mac session created
    // by `/api/v1/launch-host-session` or `cc` surface in /sessions
    // before any pane_assignment exists.
    let store_hosts = crate::services::store::load_store_or_default::<Vec<String>>(
        app,
        "remote_hosts",
    )
    .unwrap_or_default();
    let had_configured = store_hosts.iter().any(|h| {
        let t = h.trim();
        !t.is_empty() && t != "local"
    });
    for h in store_hosts {
        let t = h.trim();
        if !t.is_empty() && t != "local" {
            seen.insert(t.to_string());
        }
    }

    // First-run baseline: a brand-new install with a running Mac tmux
    // would otherwise look local-only. Once the user configures any
    // remote host (or assigns one), the fallback disengages.
    if seen.is_empty() && !had_configured {
        seen.insert("mac".to_string());
    }
    seen.into_iter().collect()
}
