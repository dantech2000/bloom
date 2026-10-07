// SPDX-License-Identifier: AGPL-3.0-or-later
//! Watched positions of local plays. The player reports to the server as
//! always; when the server cannot be reached the position is kept here,
//! and the next start with a connection sends it.
//!
//! A position belongs to an [`Identity`]: the server and the user of the
//! play. A flush sends the positions of the identity of its client and no
//! other, so a play of one profile is never written to another account.
//!
//! The file before 0.1.3 (`positions.json` as a map by item id) kept no
//! owner. Its entries are not read: their positions were never sent, and
//! the local resume point of such a play is lost. The server's position
//! stands for those items.

use std::{fs, path::Path, sync::Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::jellyfin::{Client, Progress};

const FILE: &str = "positions.json";
/// The format of the file; a file without it is the old one.
const VERSION: u32 = 2;

/// Whose position: the server and the user of a play.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub server_id: String,
    pub user_id: String,
}

impl Identity {
    /// The identity of a client, when it has one: the client of a session
    /// has its server and user.
    pub fn of(client: &Client) -> Option<Self> {
        Some(Self {
            server_id: client.server_id.as_deref()?.to_string(),
            user_id: client.user_id.as_deref()?.to_string(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Kept {
    #[serde(flatten)]
    who: Identity,
    item_id: String,
    ticks: i64,
    /// The server did not get this position yet.
    unsent: bool,
    /// Unix seconds of the last change.
    at: i64,
    /// Counts the reports of this entry. A flush that sent an older
    /// revision does not mark the entry as sent.
    #[serde(default)]
    rev: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    version: u32,
    entries: Vec<Kept>,
}

/// One writer at a time; the reports come from the thread of the player.
static LOCK: Mutex<()> = Mutex::new(());
/// One flush at a time: two of the same identity would send the same
/// positions twice, in any order.
static FLUSHING: Mutex<()> = Mutex::new(());

fn load(dir: &Path) -> Vec<Kept> {
    fs::read(dir.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
        .filter(|file| file.version == VERSION)
        .map(|file| file.entries)
        .unwrap_or_default()
}

fn save(dir: &Path, entries: &[Kept]) {
    let file = dir.join(FILE);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let out = File { version: VERSION, entries: entries.to_vec() };
    if let Ok(bytes) = serde_json::to_vec_pretty(&out) {
        let partial = file.with_extension("tmp");
        if fs::write(&partial, bytes).is_ok() && fs::rename(&partial, &file).is_err() {
            let _ = fs::remove_file(&partial);
        }
    }
}

fn find<'a>(entries: &'a mut [Kept], who: &Identity, item_id: &str) -> Option<&'a mut Kept> {
    entries.iter_mut().find(|k| k.who == *who && k.item_id == item_id)
}

/// The player reported the position of a local play of `who`. `failed`
/// says the server did not take it.
pub fn note_report(who: &Identity, progress: &Progress, failed: bool) {
    note_report_in(&super::dir(), who, progress, failed)
}

fn note_report_in(dir: &Path, who: &Identity, progress: &Progress, failed: bool) {
    let _guard = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut entries = load(dir);
    let entry = match find(&mut entries, who, &progress.item_id) {
        Some(entry) => entry,
        None => {
            entries.push(Kept {
                who: who.clone(),
                item_id: progress.item_id.clone(),
                ticks: 0,
                unsent: false,
                at: 0,
                rev: 0,
            });
            entries.last_mut().expect("just pushed")
        }
    };
    entry.ticks = progress.position_ticks;
    // A position the server took is current; the earlier unsent one is
    // older than it.
    entry.unsent = failed;
    entry.rev += 1;
    entry.at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    save(dir, &entries);
}

/// The last position of a local play of `who`, in ticks.
pub fn position(who: &Identity, item_id: &str) -> Option<i64> {
    position_in(&super::dir(), who, item_id)
}

fn position_in(dir: &Path, who: &Identity, item_id: &str) -> Option<i64> {
    let _guard = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    load(dir)
        .into_iter()
        .find(|k| k.who == *who && k.item_id == item_id)
        .map(|k| k.ticks)
        .filter(|t| *t > 0)
}

/// The download of an item of a server was removed: the positions of it
/// that the server has go with it. Kept, a position would be the resume
/// point of the item when it is downloaded again, however far the user got
/// on another device since. A position the server has not got stays, for
/// the next flush.
pub fn forget(dir: &Path, server_id: &str, item_id: &str) {
    let _guard = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entries = load(dir);
    let kept: Vec<Kept> = entries
        .iter()
        .filter(|k| k.unsent || k.item_id != item_id || k.who.server_id != server_id)
        .cloned()
        .collect();
    if kept.len() != entries.len() {
        save(dir, &kept);
    }
}

/// Items of `who` with a position the server has not got: the item, the
/// position, and the revision of the entry.
fn pending_in(dir: &Path, who: &Identity) -> Vec<(String, i64, u64)> {
    let _guard = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    load(dir)
        .into_iter()
        .filter(|k| k.unsent && k.who == *who)
        .map(|k| (k.item_id, k.ticks, k.rev))
        .collect()
}

/// Sends the unsent positions of the client's identity to the server.
/// Returns how many went. Nothing goes while `BLOOM_NO_REPORT` is set, and
/// nothing for a client without a server id.
pub fn flush(client: &Client) -> Result<usize> {
    flush_in(&super::dir(), client, crate::jellyfin::no_report())
}

fn flush_in(dir: &Path, client: &Client, no_report: bool) -> Result<usize> {
    let Some(who) = Identity::of(client) else {
        log::info!("offline positions not sent: the client has no server id");
        return Ok(0);
    };
    let _one_at_a_time = FLUSHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let waiting = pending_in(dir, &who);
    if waiting.is_empty() {
        return Ok(0);
    }
    if no_report {
        log::info!("offline positions not sent (BLOOM_NO_REPORT): {}", waiting.len());
        return Ok(0);
    }
    let mut sent = 0;
    for (item_id, ticks, rev) in waiting {
        let body = serde_json::json!({ "PlaybackPositionTicks": ticks });
        let result = client
            .post(&format!("/UserItems/{item_id}/UserData?userId={}", who.user_id), &body)
            .map(drop);
        match result {
            Ok(()) => {
                sent += 1;
                let _guard = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut entries = load(dir);
                // A report that came while this one was on its way is
                // newer, also at the same position: it still waits.
                if let Some(entry) = find(&mut entries, &who, &item_id)
                    && entry.rev == rev
                {
                    entry.unsent = false;
                }
                save(dir, &entries);
            }
            Err(err) => log::warn!("offline position of {item_id} not sent: {err:#}"),
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering::SeqCst},
        },
        thread,
        time::Duration,
    };

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

    fn who(server: &str, user: &str) -> Identity {
        Identity { server_id: server.into(), user_id: user.into() }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bloom-positions-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn pending(dir: &Path, who: &Identity) -> Vec<(String, i64)> {
        pending_in(dir, who).into_iter().map(|(id, ticks, _)| (id, ticks)).collect()
    }

    /// A server that records the request line and the body of each request
    /// as it comes in, and answers 200. The answer to the first request
    /// waits for `delay`; the requests after it are served meanwhile, each
    /// on a thread of its own. It ends with the test.
    struct Recorder {
        port: u16,
        seen: Arc<Mutex<Vec<(String, String)>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    /// Reads one request, notes it, and answers after `delay`.
    fn serve(mut stream: TcpStream, kept: Arc<Mutex<Vec<(String, String)>>>, delay: Duration) {
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        let body_start = loop {
            let Ok(n) = stream.read(&mut buffer) else { return };
            if n == 0 {
                return;
            }
            data.extend_from_slice(&buffer[..n]);
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let head = String::from_utf8_lossy(&data[..body_start]).to_string();
        let length: usize = head
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")))
            .and_then(|(_, v)| v.trim().parse().ok())
            .unwrap_or(0);
        while data.len() < body_start + length {
            let Ok(n) = stream.read(&mut buffer) else { break };
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..n]);
        }
        let line = head.lines().next().unwrap_or_default().to_string();
        let body = String::from_utf8_lossy(&data[body_start..]).to_string();
        kept.lock().unwrap().push((line, body));
        thread::sleep(delay);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }

    impl Recorder {
        fn start(delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (kept, stopping) = (seen.clone(), stop.clone());
            let thread = thread::spawn(move || {
                let mut workers = Vec::new();
                for (n, stream) in listener.incoming().flatten().enumerate() {
                    if stopping.load(SeqCst) {
                        break;
                    }
                    let kept = kept.clone();
                    let delay = if n == 0 { delay } else { Duration::ZERO };
                    workers.push(thread::spawn(move || serve(stream, kept, delay)));
                }
                for worker in workers {
                    let _ = worker.join();
                }
            });
            Self { port, seen, stop, thread: Some(thread) }
        }

        fn client(&self, server: &str, user: &str) -> Client {
            Client::new(&format!("http://127.0.0.1:{}", self.port), "d")
                .with_session("t", user)
                .with_server(server)
        }

        fn lines(&self) -> Vec<String> {
            self.seen.lock().unwrap().iter().map(|(line, _)| line.clone()).collect()
        }
    }

    impl Drop for Recorder {
        fn drop(&mut self) {
            self.stop.store(true, SeqCst);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn keeps_the_position_and_what_is_unsent() {
        let dir = temp("keeps");
        let u = who("s", "u");
        // A report the server took, then one it did not.
        note_report_in(&dir, &u, &progress("a", 10), false);
        note_report_in(&dir, &u, &progress("b", 20), true);
        note_report_in(&dir, &u, &progress("b", 25), true);
        assert_eq!(pending(&dir, &u), vec![("b".to_string(), 25 * crate::jellyfin::TICKS_PER_SECOND)]);
        assert_eq!(position_in(&dir, &u, "a"), Some(10 * crate::jellyfin::TICKS_PER_SECOND));
        // The server takes it later: it is sent no more.
        note_report_in(&dir, &u, &progress("b", 30), false);
        assert!(pending(&dir, &u).is_empty());
        // Nothing goes to a server that cannot be reached; the entry waits.
        note_report_in(&dir, &u, &progress("c", 5), true);
        let dead = Client::new("http://127.0.0.1:9", "d").with_session("t", "u").with_server("s");
        assert_eq!(flush_in(&dir, &dead, false).unwrap(), 0);
        assert_eq!(pending(&dir, &u).len(), 1);
        // And nothing goes with the switch that keeps a test off the
        // server's records.
        assert_eq!(flush_in(&dir, &dead, true).unwrap(), 0);
        assert_eq!(pending(&dir, &u).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The file before this format kept no owner: it is not read, and the
    /// next save writes the new format.
    #[test]
    fn the_file_without_owners_is_dropped() {
        let dir = temp("old");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), br#"{"item1": {"ticks": 400000000, "unsent": true, "at": 1}}"#).unwrap();
        let u = who("s", "u");
        assert!(pending(&dir, &u).is_empty());
        assert_eq!(position_in(&dir, &u, "item1"), None);
        note_report_in(&dir, &u, &progress("item2", 3), true);
        let file: File = serde_json::from_slice(&fs::read(dir.join(FILE)).unwrap()).unwrap();
        assert_eq!(file.version, VERSION);
        assert_eq!(file.entries.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Finding 1 of the review: the position of a play of one profile must
    /// not be written to the account of another profile, nor to another
    /// server; the profile's own session sends it.
    #[test]
    fn a_position_of_one_identity_is_sent_as_that_identity_only() {
        let dir = temp("users");
        let server = Recorder::start(Duration::ZERO);
        // User A played a download on server 1 while the server was away.
        note_report_in(&dir, &who("server-1", "user-a"), &progress("item1", 40), true);
        // User B of the same server opens the app with a connection.
        assert_eq!(flush_in(&dir, &server.client("server-1", "user-b"), false).unwrap(), 0);
        // User A of another server too.
        assert_eq!(flush_in(&dir, &server.client("server-2", "user-a"), false).unwrap(), 0);
        assert!(server.lines().is_empty(), "a position went out as another identity: {:?}", server.lines());
        // A's own session on server 1 sends it, once.
        assert_eq!(flush_in(&dir, &server.client("server-1", "user-a"), false).unwrap(), 1);
        assert_eq!(server.lines(), vec!["POST /UserItems/item1/UserData?userId=user-a HTTP/1.1"]);
        assert!(pending(&dir, &who("server-1", "user-a")).is_empty());
        assert_eq!(flush_in(&dir, &server.client("server-1", "user-a"), false).unwrap(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The local resume point is per identity as well.
    #[test]
    fn the_local_resume_point_is_per_identity() {
        let dir = temp("resume");
        let (a, b, other) = (who("s", "a"), who("s", "b"), who("t", "a"));
        note_report_in(&dir, &a, &progress("item1", 40), true);
        assert_eq!(position_in(&dir, &a, "item1"), Some(40 * crate::jellyfin::TICKS_PER_SECOND));
        assert_eq!(position_in(&dir, &b, "item1"), None);
        assert_eq!(position_in(&dir, &other, "item1"), None);
        note_report_in(&dir, &b, &progress("item1", 5), false);
        assert_eq!(position_in(&dir, &a, "item1"), Some(40 * crate::jellyfin::TICKS_PER_SECOND));
        assert_eq!(position_in(&dir, &b, "item1"), Some(5 * crate::jellyfin::TICKS_PER_SECOND));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A removed download takes its sent positions with it, of every user
    /// of that server; what the server has not got stays for the flush.
    #[test]
    fn a_removed_download_leaves_no_resume_point_but_keeps_what_is_unsent() {
        let dir = temp("forget");
        let (a, b, other) = (who("s", "a"), who("s", "b"), who("t", "a"));
        note_report_in(&dir, &a, &progress("item1", 40), false);
        note_report_in(&dir, &b, &progress("item1", 50), true);
        note_report_in(&dir, &other, &progress("item1", 60), false);
        note_report_in(&dir, &a, &progress("item2", 70), false);
        forget(&dir, "s", "item1");
        assert_eq!(position_in(&dir, &a, "item1"), None, "a sent position outlived its download");
        assert_eq!(position_in(&dir, &b, "item1"), Some(50 * crate::jellyfin::TICKS_PER_SECOND), "an unsent position was dropped");
        assert_eq!(position_in(&dir, &other, "item1"), Some(60 * crate::jellyfin::TICKS_PER_SECOND), "another server's item was touched");
        assert_eq!(position_in(&dir, &a, "item2"), Some(70 * crate::jellyfin::TICKS_PER_SECOND), "another item was touched");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A client without a server id (not the client of a session) has no
    /// identity: nothing is noted for it, nothing sent.
    #[test]
    fn a_client_without_a_server_id_has_no_identity() {
        let client = Client::new("http://127.0.0.1:9", "d").with_session("t", "u");
        assert!(Identity::of(&client).is_none());
        assert_eq!(Identity::of(&client.with_server("s")), Some(who("s", "u")));
        let dir = temp("noid");
        assert_eq!(flush_in(&dir, &Client::new("http://127.0.0.1:9", "d"), false).unwrap(), 0);
    }

    /// Finding 5 of the review: a report that fails while a flush is on its
    /// way must stay pending, also when it has the same position: the flush
    /// sent an older report.
    #[test]
    fn a_report_during_the_flush_stays_pending_also_at_the_same_position() {
        let dir = temp("during");
        let server = Recorder::start(Duration::from_millis(400));
        let u = who("s", "u");
        note_report_in(&dir, &u, &progress("item2", 10), true);
        let client = server.client("s", "u");
        let flushed_dir = dir.clone();
        let flush = thread::spawn(move || flush_in(&flushed_dir, &client, false));
        // The flush has sent its request once the server has it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.lines().is_empty() {
            assert!(std::time::Instant::now() < deadline, "the flush sent nothing");
            thread::sleep(Duration::from_millis(5));
        }
        // The player seeks away and back: it reports 10 s again, and the
        // server does not take it.
        note_report_in(&dir, &u, &progress("item2", 10), true);
        assert_eq!(flush.join().unwrap().unwrap(), 1);
        assert_eq!(pending(&dir, &u), vec![("item2".to_string(), 10 * crate::jellyfin::TICKS_PER_SECOND)]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The reversed flush: a flush that waits for the server must not send
    /// its older position after a second flush has sent the newer one. The
    /// flushes go one after the other, so the server gets the newer
    /// position last and no entry is marked as sent on an older report.
    #[test]
    fn a_second_flush_does_not_overtake_the_first() {
        let dir = temp("reversed");
        // The answer to the first request waits; the server takes the
        // requests after it at once, as a real one does.
        let server = Recorder::start(Duration::from_millis(400));
        let u = who("s", "u");
        note_report_in(&dir, &u, &progress("item-a", 5), true);
        note_report_in(&dir, &u, &progress("item-b", 10), true);
        let first_client = server.client("s", "u");
        let first_dir = dir.clone();
        // The first flush reads both positions and waits on item-a.
        let first = thread::spawn(move || flush_in(&first_dir, &first_client, false));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.lines().is_empty() {
            assert!(std::time::Instant::now() < deadline, "the first flush sent nothing");
            thread::sleep(Duration::from_millis(5));
        }
        // A newer position of item-b, and a second flush while the first waits.
        note_report_in(&dir, &u, &progress("item-b", 20), true);
        let second_client = server.client("s", "u");
        let second_dir = dir.clone();
        let second = thread::spawn(move || flush_in(&second_dir, &second_client, false));
        let (first, second) = (first.join().unwrap().unwrap(), second.join().unwrap().unwrap());
        let ticks = |secs: i64| format!("{{\"PlaybackPositionTicks\":{}}}", secs * crate::jellyfin::TICKS_PER_SECOND);
        let of_b: Vec<String> = server
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(line, _)| line.contains("/UserItems/item-b/"))
            .map(|(_, body)| body.split_whitespace().collect())
            .collect();
        // The server has the newer position of item-b last.
        assert_eq!(of_b, vec![ticks(10), ticks(20)], "flushes sent {first} and {second}");
        assert!(pending(&dir, &u).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
