// SPDX-License-Identifier: AGPL-3.0-or-later
//! Watched positions of local plays. The player reports to the server as
//! always; when the server cannot be reached the position is kept here,
//! and the next start with a connection sends it.

use std::{collections::HashMap, fs, path::Path, sync::Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::jellyfin::{Client, Progress};

const FILE: &str = "positions.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Kept {
    ticks: i64,
    /// The server did not get this position yet.
    unsent: bool,
    /// Unix seconds of the last change.
    at: i64,
}

/// One writer at a time; the reports come from the thread of the player.
static LOCK: Mutex<()> = Mutex::new(());

fn load(dir: &Path) -> HashMap<String, Kept> {
    fs::read(dir.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, kept: &HashMap<String, Kept>) {
    let file = dir.join(FILE);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(kept) {
        let partial = file.with_extension("tmp");
        if fs::write(&partial, bytes).is_ok() && fs::rename(&partial, &file).is_err() {
            let _ = fs::remove_file(&partial);
        }
    }
}

/// The player reported the position of a local play. `failed` says the
/// server did not take it.
pub fn note_report(progress: &Progress, failed: bool) {
    note_report_in(&super::dir(), progress, failed)
}

fn note_report_in(dir: &Path, progress: &Progress, failed: bool) {
    let _guard = LOCK.lock().unwrap();
    let mut kept = load(dir);
    let entry = kept.entry(progress.item_id.clone()).or_default();
    entry.ticks = progress.position_ticks;
    // A position the server took is current; the earlier unsent one is
    // older than it.
    entry.unsent = failed;
    entry.at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    save(dir, &kept);
}

/// The last position of a local play, in ticks.
pub fn position(item_id: &str) -> Option<i64> {
    let _guard = LOCK.lock().unwrap();
    load(&super::dir()).get(item_id).map(|k| k.ticks).filter(|t| *t > 0)
}

/// Items with a position the server has not got.
pub fn pending() -> Vec<(String, i64)> {
    pending_in(&super::dir())
}

fn pending_in(dir: &Path) -> Vec<(String, i64)> {
    let _guard = LOCK.lock().unwrap();
    load(dir)
        .into_iter()
        .filter(|(_, k)| k.unsent)
        .map(|(id, k)| (id, k.ticks))
        .collect()
}

/// Sends the unsent positions to the server. Returns how many went.
/// Nothing goes while `BLOOM_NO_REPORT` is set.
pub fn flush(client: &Client) -> Result<usize> {
    flush_in(&super::dir(), client)
}

fn flush_in(dir: &Path, client: &Client) -> Result<usize> {
    let waiting = pending_in(dir);
    if waiting.is_empty() {
        return Ok(0);
    }
    if crate::jellyfin::no_report() {
        log::info!("offline positions not sent (BLOOM_NO_REPORT): {}", waiting.len());
        return Ok(0);
    }
    let user = client.user()?.to_string();
    let mut sent = 0;
    for (item_id, ticks) in waiting {
        let body = serde_json::json!({ "PlaybackPositionTicks": ticks });
        let result = client
            .post(&format!("/UserItems/{item_id}/UserData?userId={user}"), &body)
            .map(drop);
        match result {
            Ok(()) => {
                sent += 1;
                let _guard = LOCK.lock().unwrap();
                let mut kept = load(dir);
                if let Some(entry) = kept.get_mut(&item_id) {
                    entry.unsent = false;
                }
                save(dir, &kept);
            }
            Err(err) => log::warn!("offline position of {item_id} not sent: {err:#}"),
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(item_id: &str, secs: i64) -> Progress {
        Progress {
            item_id: item_id.into(),
            play_session_id: "s".into(),
            position_ticks: secs * crate::jellyfin::TICKS_PER_SECOND,
            paused: false,
            volume: 100,
            muted: false,
            play_method: String::new(),
            media_source_id: String::new(),
        }
    }

    #[test]
    fn keeps_the_position_and_what_is_unsent() {
        let dir = std::env::temp_dir().join(format!("bloom-positions-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // A report the server took, then one it did not.
        note_report_in(&dir, &progress("a", 10), false);
        note_report_in(&dir, &progress("b", 20), true);
        note_report_in(&dir, &progress("b", 25), true);
        assert_eq!(pending_in(&dir), vec![("b".to_string(), 25 * crate::jellyfin::TICKS_PER_SECOND)]);
        assert_eq!(load(&dir)["a"].ticks, 10 * crate::jellyfin::TICKS_PER_SECOND);
        // The server takes it later: it is sent no more.
        note_report_in(&dir, &progress("b", 30), false);
        assert!(pending_in(&dir).is_empty());
        // Nothing goes to a server that cannot be reached; the entry waits.
        note_report_in(&dir, &progress("c", 5), true);
        let dead = Client::new("http://127.0.0.1:9", "d").with_session("t", "u");
        if crate::jellyfin::no_report() {
            assert_eq!(flush_in(&dir, &dead).unwrap(), 0);
        } else {
            assert_eq!(flush_in(&dir, &dead).unwrap(), 0);
            assert_eq!(pending_in(&dir).len(), 1);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
