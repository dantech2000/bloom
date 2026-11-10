// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine against a server that plays a script: a local HTTP server
//! that serves one file, honours or ignores `Range`, cuts the connection
//! after some bytes, or announces a size it does not have.

use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

use super::*;
use crate::downloads::test_http::Request;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Serves the file with `Range`.
    Normal,
    /// Answers every request with the whole file (200).
    IgnoreRange,
    /// Closes the connection after this many bytes of the body, once.
    CutAfter(usize),
    /// Announces 100 bytes more than it sends.
    WrongSize,
    /// Sends 64 bytes every 20 ms.
    Slow,
    /// Sends this many bytes of the body, then sends nothing for this
    /// long, once; later requests are served in full.
    StallOnce(usize, Duration),
    /// The item says no size: the download is of unknown size.
    NoSize,
    /// The item says 1000 bytes less than the file the server sends,
    /// every time.
    Oversend,
}

struct Mock {
    port: u16,
    file: Arc<Vec<u8>>,
    mode: Arc<Mutex<Mode>>,
    /// Path and `Range` header of each request to the file.
    ranges: Arc<Mutex<Vec<Option<String>>>>,
    /// Requests of any kind.
    hits: Arc<std::sync::atomic::AtomicUsize>,
}

impl Mock {
    fn start(size: usize) -> Self {
        let file: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let file = Arc::new(file);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mode = Arc::new(Mutex::new(Mode::Normal));
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (served, shared_mode, shared_ranges, counted) = (file.clone(), mode.clone(), ranges.clone(), hits.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                let (file, mode, ranges) = (served.clone(), shared_mode.clone(), shared_ranges.clone());
                thread::spawn(move || serve(stream, file, mode, ranges));
            }
        });
        Self { port, file, mode, ranges, hits }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn client(&self) -> Client {
        Client::new(&format!("http://127.0.0.1:{}", self.port), "device-under-test")
            .with_session("token-under-test", "user-under-test")
    }

    fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn ranges(&self) -> Vec<Option<String>> {
        self.ranges.lock().unwrap().clone()
    }
}

fn serve(mut stream: TcpStream, file: Arc<Vec<u8>>, mode: Arc<Mutex<Mode>>, ranges: Arc<Mutex<Vec<Option<String>>>>) {
    let Some(request) = Request::read(&mut stream) else { return };
    let path = request.path().to_string();
    let header = |name: &str| request.header(name);
    let reply = |stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]| {
        let _ = write!(stream, "HTTP/1.1 {status}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n", body.len());
        let _ = stream.write_all(body);
    };
    if path.ends_with("/Download") {
        let range = header("range");
        ranges.lock().unwrap().push(range.clone());
        // The file has a validator; a range that names another one
        // gets the whole file, as a server answers `If-Range`.
        let tag = format!("\"file-{}\"", file.len());
        let same = header("if-range").is_none_or(|named| named == tag);
        let start: usize = range
            .as_deref()
            .filter(|_| same)
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.split('-').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let current = *mode.lock().unwrap();
        match current {
            Mode::IgnoreRange => reply(&mut stream, "200 OK", "", &file),
            Mode::WrongSize => {
                let claimed = file.len() + 100;
                if start >= file.len() {
                    reply(&mut stream, "416 Range Not Satisfiable", "", b"");
                    return;
                }
                let body = &file[start..];
                let status = match start {
                    0 => "200 OK".to_string(),
                    _ => format!("206 Partial Content\r\nContent-Range: bytes {start}-{}/{claimed}", claimed - 1),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nConnection: close\r\nETag: {tag}\r\nContent-Length: {}\r\n\r\n",
                    claimed - start
                );
                let _ = stream.write_all(body);
            }
            _ => {
                if start > file.len() {
                    reply(&mut stream, "416 Range Not Satisfiable", "", b"");
                    return;
                }
                let body = &file[start..];
                if start > 0 {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nETag: {tag}\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Length: {}\r\n\r\n",
                        file.len() - 1,
                        file.len(),
                        body.len()
                    );
                } else {
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nETag: {tag}\r\nContent-Length: {}\r\n\r\n", body.len());
                }
                match current {
                    Mode::CutAfter(n) => {
                        let _ = stream.write_all(&body[..n.min(body.len())]);
                        let _ = stream.flush();
                        *mode.lock().unwrap() = Mode::Normal;
                        // Dropped without the rest.
                    }
                    Mode::StallOnce(n, pause) => {
                        let _ = stream.write_all(&body[..n.min(body.len())]);
                        let _ = stream.flush();
                        *mode.lock().unwrap() = Mode::Normal;
                        // The connection stays open and silent.
                        thread::sleep(pause);
                    }
                    Mode::Slow => {
                        for chunk in body.chunks(64) {
                            if stream.write_all(chunk).is_err() {
                                return;
                            }
                            let _ = stream.flush();
                            thread::sleep(Duration::from_millis(20));
                        }
                    }
                    _ => {
                        let _ = stream.write_all(body);
                    }
                }
            }
        }
    } else if path.starts_with("/Items/") && !path.contains("/Images/") {
        let id = path.trim_start_matches("/Items/").to_string();
        // A server with a wrong idea of the size says so in the item too.
        let size = match *mode.lock().unwrap() {
            Mode::WrongSize => file.len() + 100,
            Mode::NoSize => 0,
            Mode::Oversend => file.len() - 1000,
            _ => file.len(),
        };
        let body = serde_json::json!({
            "Id": id, "Name": "Test episode", "Type": "Episode",
            "SeriesName": "Test show", "SeriesId": "series1",
            "ParentIndexNumber": 1, "IndexNumber": 2,
            "MediaSources": [{ "Id": id, "Size": size, "Container": "bin", "MediaStreams": [] }],
            // An item named "art..." has a poster (`POSTER`).
            "ImageTags": if id.starts_with("art") { serde_json::json!({"Primary": "t"}) } else { serde_json::json!({}) }
        });
        reply(&mut stream, "200 OK", "Content-Type: application/json\r\n", body.to_string().as_bytes());
    } else if path.starts_with("/Items/art") && path.contains("/Images/Primary") {
        reply(&mut stream, "200 OK", "Content-Type: image/png\r\n", POSTER);
    } else if path.starts_with("/MediaSegments/") {
        reply(&mut stream, "200 OK", "Content-Type: application/json\r\n", br#"{"Items":[]}"#);
    } else {
        reply(&mut stream, "404 Not Found", "", b"");
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bloom-downloads-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn options() -> Options {
    Options {
        artwork: Artwork::default(),
        parallel: 1,
        limit_bytes: None,
        free_override: Some(100 * MIN_FREE_BYTES),
        retry_wait: Duration::from_millis(20),
        byte_limit: None,
        throttle_bps: None,
        stall: Duration::from_secs(30),
        start_gate: None,
    }
}

fn item(id: &str) -> Item {
    Item {
        id: id.into(),
        name: "Hint name".into(),
        kind: "Episode".into(),
        ..Default::default()
    }
}

fn engine(mock: &Mock, dir: PathBuf, options: Options) -> Engine {
    let engine = Engine::open(dir, options);
    assert!(engine.owns_storage(), "the folder is held by another engine");
    engine.set_client(Some(mock.client()), "server1");
    engine
}

/// Opens the engine on a folder an engine of this process just left,
/// as after a restart. The lock goes with that engine, once its
/// scheduler let go of it (within its wait of half a second).
fn reopen(mock: &Mock, dir: PathBuf, options: Options) -> Engine {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let engine = Engine::open(dir.clone(), options.clone());
        if engine.owns_storage() {
            engine.set_client(Some(mock.client()), "server1");
            return engine;
        }
        assert!(Instant::now() < deadline, "the folder stays locked");
        drop(engine);
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_done_at_least(engine: &Engine, id: &str, bytes: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.entry(id).unwrap().done < bytes {
        assert!(Instant::now() < deadline, "{:?}", engine.entry(id));
        thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until no worker runs and no tombstone is left.
fn wait_idle(engine: &Engine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.busy() != (0, 0) {
        assert!(Instant::now() < deadline, "busy: {:?}", engine.busy());
        thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until the index on the disk is the one of the state: a
/// worker writes it off the lock, after the state shows its change.
fn wait_saved(engine: &Engine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let revision = engine.inner.lock().revision;
        if *engine.inner.saved.lock().unwrap() >= revision {
            return;
        }
        assert!(Instant::now() < deadline, "the index is not written");
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_gone(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < deadline, "{} is still there", path.display());
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_state(engine: &Engine, id: &str, state: EntryState) -> Entry {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // The entry of the session's server, or else (after a switch
        // of the session) the one of that id.
        let entry = engine
            .entry(id)
            .or_else(|| engine.entries().into_iter().find(|e| e.item_id == id))
            .expect("entry");
        if entry.state == state {
            return entry;
        }
        assert!(Instant::now() < deadline, "waited for {state:?}, got {entry:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn full_download_writes_file_meta_and_index() {
    let mock = Mock::start(100_000);
    let dir = temp_dir("full");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep1")).unwrap();
    let entry = wait_state(&engine, "ep1", EntryState::Done);
    assert_eq!(entry.total, Some(100_000));
    assert_eq!(entry.done, 100_000);
    // The names come from the server, not from the hint.
    assert_eq!(entry.name, "Test episode");
    assert_eq!(entry.title(), "S1:E2 · Test episode");
    let path = engine.local_path("ep1").expect("local file");
    assert_eq!(path.file_name().unwrap(), "video.bin");
    // The file is the item of its own server only: another server
    // with an item of the same id does not play it.
    assert_eq!(engine.local_path(Key::of(Some(&entry.server_id), "ep1")), Some(path.clone()));
    assert_eq!(engine.local_path(Key::of(Some("another-server"), "ep1")), None);
    assert_eq!(fs::read(&path).unwrap(), *mock.file);
    assert!(!path.with_extension("bin.part").exists());
    let meta = engine.meta("ep1").unwrap();
    assert_eq!(meta["Item"]["Name"], "Test episode");
    assert_eq!(meta["File"], "video.bin");
    wait_saved(&engine);
    let index: Vec<Entry> = serde_json::from_slice(&fs::read(dir.join(INDEX_FILE)).unwrap()).unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].state, EntryState::Done);
    assert_eq!(mock.ranges(), vec![None]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn resumes_with_range_after_a_cut_connection() {
    let mock = Mock::start(300_000);
    mock.set(Mode::CutAfter(120_000));
    let dir = temp_dir("cut");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep2")).unwrap();
    wait_state(&engine, "ep2", EntryState::Done);
    let ranges = mock.ranges();
    assert_eq!(ranges.len(), 2, "{ranges:?}");
    assert_eq!(ranges[0], None);
    assert_eq!(ranges[1].as_deref(), Some("bytes=120000-"));
    assert_eq!(fs::read(engine.local_path("ep2").unwrap()).unwrap(), *mock.file);
    let _ = fs::remove_dir_all(dir);
}

/// What the mock sends for the poster of an "art..." item.
const POSTER: &[u8] = b"POSTER bytes of the mock";

/// The image cache of the app is two functions the engine is given: it
/// keeps a poster they accept and seeds the cache with it, and keeps
/// none they refuse.
#[test]
fn artwork_goes_through_the_checks_of_the_app() {
    static SEEDED: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());
    fn seed(url: &str, path: &Path) {
        SEEDED.lock().unwrap().push((url.to_string(), path.to_path_buf()));
    }
    let mock = Mock::start(10_000);
    let seeded = |id: &str| SEEDED.lock().unwrap().iter().filter(|(url, _)| url.contains(id)).count();

    let dir = temp_dir("art-kept");
    let artwork = Artwork { is_image: |bytes| bytes == POSTER, seed };
    let kept = engine(&mock, dir.clone(), Options { artwork, ..options() });
    kept.add(&item("art1")).unwrap();
    wait_state(&kept, "art1", EntryState::Done);
    let poster = kept.folder("art1").unwrap().join(POSTER_FILE);
    assert_eq!(fs::read(&poster).unwrap(), POSTER);
    // Once for each width a page asks for, each time with the file.
    assert_eq!(seeded("art1"), 6);
    assert!(SEEDED.lock().unwrap().iter().all(|(_, path)| *path == poster));
    // The image loader asks for the file by the URL of the server.
    let url = |id: &str, kind: &str| format!("http://127.0.0.1:{}/Items/{id}/Images/{kind}?tag=t1&fillWidth=320", mock.port);
    assert_eq!(kept.local_image(&url("art1", "Primary")), Some(poster));
    assert_eq!(kept.local_image(&url("art1", "Logo")), None, "a kind the engine does not keep");
    assert_eq!(kept.local_image(&url("art9", "Primary")), None, "an item that is not downloaded");
    let _ = fs::remove_dir_all(dir);

    let dir = temp_dir("art-refused");
    let artwork = Artwork { is_image: |_| false, seed };
    let refused = engine(&mock, dir.clone(), Options { artwork, ..options() });
    refused.add(&item("art2")).unwrap();
    wait_state(&refused, "art2", EntryState::Done);
    assert!(!refused.folder("art2").unwrap().join(POSTER_FILE).exists(), "a refused answer was kept");
    assert_eq!(refused.local_image(&url("art2", "Primary")), None);
    assert_eq!(seeded("art2"), 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn starts_over_when_the_server_ignores_range() {
    let mock = Mock::start(50_000);
    let dir = temp_dir("norange");
    // A part file from an earlier run, with other bytes in it.
    let folder = dir.join("server1").join("ep3");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("video.bin.part"), vec![9u8; 20_000]).unwrap();
    fs::write(folder.join("video.bin.part.tag"), "\"file-50000\"").unwrap();
    mock.set(Mode::IgnoreRange);
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep3")).unwrap();
    wait_state(&engine, "ep3", EntryState::Done);
    assert_eq!(mock.ranges()[0].as_deref(), Some("bytes=20000-"));
    assert_eq!(fs::read(engine.local_path("ep3").unwrap()).unwrap(), *mock.file);
    let _ = fs::remove_dir_all(dir);
}

/// A part file with no validator beside it (the server gave none
/// when the file was started) cannot be tied to the file the server
/// has now: the download starts over. Before, the rest of the
/// server's file was put behind whatever the part file held.
#[test]
fn a_part_file_with_no_validator_starts_over() {
    let mock = Mock::start(50_000);
    let dir = temp_dir("novalidator");
    let folder = dir.join("server1").join("ep90");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("video.bin.part"), vec![9u8; 20_000]).unwrap();
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep90")).unwrap();
    wait_state(&engine, "ep90", EntryState::Done);
    assert_eq!(mock.ranges(), vec![None], "the part file was gone on with");
    assert_eq!(fs::read(engine.local_path("ep90").unwrap()).unwrap(), *mock.file);
    let _ = fs::remove_dir_all(dir);
}

/// A full disk ends the download with its word and no further try;
/// any other failed write is a failed try, as before.
#[test]
fn a_full_disk_is_no_failed_try() {
    use std::io::{Error, ErrorKind};
    assert!(matches!(write_failed(&Error::from(ErrorKind::StorageFull)), Stop::Refused(why) if why == "The disk is full"));
    assert!(matches!(write_failed(&Error::from(ErrorKind::QuotaExceeded)), Stop::Refused(_)));
    assert!(matches!(write_failed(&Error::from(ErrorKind::BrokenPipe)), Stop::Failed(_)));
}

fn wait_entries(engine: &Engine, ids: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ids.iter().all(|id| engine.entry(*id).is_some()) {
        assert!(Instant::now() < deadline, "no entry for one of {ids:?}: {:?}", engine.entries());
        thread::sleep(Duration::from_millis(10));
    }
}

/// The index is lost (it did not read, or was written over): the
/// folders of the downloads give their entries back at the next
/// start, the finished one playable and the unfinished one paused at
/// its part file. Before, the files stayed on the disk for good,
/// listed and counted nowhere. A download the user removed does not
/// come back, and a folder that a removal did not finish goes.
#[test]
fn downloads_the_index_lost_come_back_and_removed_ones_do_not() {
    let mock = Mock::start(40_000);
    let dir = temp_dir("orphans");
    let first = engine(&mock, dir.clone(), options());
    first.add(&item("ep91")).unwrap();
    wait_state(&first, "ep91", EntryState::Done);
    mock.set(Mode::Slow);
    first.add(&item("ep92")).unwrap();
    wait_done_at_least(&first, "ep92", 1_000);
    first.cancel("ep92");
    wait_state(&first, "ep92", EntryState::Paused);
    wait_idle(&first);
    wait_saved(&first);
    // Read after the worker let go: bytes written after the cancel count.
    let paused = first.entry("ep92").unwrap();
    drop(first);
    fs::remove_file(dir.join(INDEX_FILE)).unwrap();

    let second = reopen(&mock, dir.clone(), options());
    wait_entries(&second, &["ep91", "ep92"]);
    let done = second.entry("ep91").unwrap();
    assert_eq!((done.state, done.done, done.total), (EntryState::Done, 40_000, Some(40_000)));
    assert_eq!(done.title(), "S1:E2 · Test episode");
    assert_eq!(fs::read(second.local_path("ep91").expect("the file plays")).unwrap(), *mock.file);
    let part = second.entry("ep92").unwrap();
    assert_eq!((part.state, part.done), (EntryState::Paused, paused.done));
    // The entries are in the index again.
    wait_saved(&second);
    let index: Vec<Entry> = serde_json::from_slice(&fs::read(dir.join(INDEX_FILE)).unwrap()).unwrap();
    assert_eq!(index.len(), 2);

    // Removed by the user, and the index lost again: only the other
    // one comes back. A folder of a removal that did not end goes.
    let folder = second.folder("ep91").unwrap();
    second.remove("ep91");
    wait_gone(&folder);
    wait_idle(&second);
    wait_saved(&second);
    drop(second);
    fs::remove_file(dir.join(INDEX_FILE)).unwrap();
    let left = dir.join("server1").join(format!("ep93{GONE}"));
    fs::create_dir_all(&left).unwrap();
    fs::write(left.join("video.bin"), b"what a removal left").unwrap();
    let third = reopen(&mock, dir.clone(), options());
    wait_entries(&third, &["ep92"]);
    wait_gone(&left);
    assert!(third.entry("ep91").is_none(), "a removed download came back");
    let _ = fs::remove_dir_all(dir);
}

/// A call that changes no entry leaves the index on the disk alone
/// (the page calls from the UI thread), and several items go in or
/// out with one call.
#[test]
fn nothing_changed_writes_nothing_and_several_go_as_one() {
    use std::os::unix::fs::MetadataExt as _;
    let mock = Mock::start(5_000);
    let dir = temp_dir("many");
    let engine = engine(&mock, dir.clone(), options());
    let items = [item("ep94"), item("ep95"), item("../unsafe")];
    assert_eq!(engine.add_many(&items).unwrap(), 2);
    wait_state(&engine, "ep94", EntryState::Done);
    wait_state(&engine, "ep95", EntryState::Done);
    wait_idle(&engine);
    wait_saved(&engine);
    let all = engine.summary();
    assert_eq!((all.entries, all.done, all.active, all.used), (2, 2, 0, 10_000));
    let file = || fs::metadata(dir.join(INDEX_FILE)).unwrap().ino();
    let before = file();
    assert_eq!(engine.add_many(&items).unwrap(), 0);
    engine.add(&item("ep94")).unwrap();
    engine.cancel("ep94");
    engine.cancel("not-there");
    engine.resume("ep95");
    engine.remove("not-there");
    assert_eq!(file(), before, "the index was written for no change");
    let folders = [engine.folder("ep94").unwrap(), engine.folder("ep95").unwrap()];
    engine.remove_many(["ep94", "ep95"]);
    assert!(engine.entries().is_empty());
    assert_ne!(file(), before);
    folders.iter().for_each(|folder| wait_gone(folder));
    wait_idle(&engine);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn cancel_leaves_a_part_file_and_resume_goes_on() {
    let mock = Mock::start(40_000);
    mock.set(Mode::Slow);
    let dir = temp_dir("cancel");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep4")).unwrap();
    // Some bytes, then stop.
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.entry("ep4").unwrap().done < 1_000 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    engine.cancel("ep4");
    let entry = wait_state(&engine, "ep4", EntryState::Paused);
    assert!(entry.done >= 1_000 && entry.done < 40_000, "{entry:?}");
    let part = dir.join("server1").join("ep4").join("video.bin.part");
    assert_eq!(fs::metadata(&part).unwrap().len(), entry.done);
    assert!(engine.local_path("ep4").is_none());
    mock.set(Mode::Normal);
    engine.resume("ep4");
    wait_state(&engine, "ep4", EntryState::Done);
    let ranges = mock.ranges();
    assert_eq!(ranges.last().unwrap().as_deref(), Some(format!("bytes={}-", entry.done).as_str()));
    assert_eq!(fs::read(engine.local_path("ep4").unwrap()).unwrap(), *mock.file);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_stalled_connection_is_dropped_and_the_retry_goes_on_with_range() {
    let mock = Mock::start(40_000);
    mock.set(Mode::StallOnce(5_000, Duration::from_secs(30)));
    let dir = temp_dir("stall");
    let mut options = options();
    options.stall = Duration::from_millis(1_500);
    let engine = engine(&mock, dir.clone(), options);
    let began = Instant::now();
    engine.add(&item("ep9")).unwrap();
    wait_state(&engine, "ep9", EntryState::Done);
    // The stall of 1.5 s ended the first try; the server still sleeps.
    assert!(began.elapsed() < Duration::from_secs(10), "{:?}", began.elapsed());
    let ranges = mock.ranges();
    assert_eq!(ranges.len(), 2, "{ranges:?}");
    assert_eq!(ranges[1].as_deref(), Some("bytes=5000-"));
    assert_eq!(fs::read(engine.local_path("ep9").unwrap()).unwrap(), *mock.file);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_cancel_in_a_stall_takes_effect_within_about_a_second() {
    let mock = Mock::start(40_000);
    mock.set(Mode::StallOnce(3_000, Duration::from_secs(30)));
    let dir = temp_dir("stallcancel");
    // The stall limit is far away: only the cancel can end the wait.
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep10")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.entry("ep10").unwrap().done < 3_000 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    // Let the read block with no byte coming.
    thread::sleep(Duration::from_millis(300));
    let asked = Instant::now();
    engine.cancel("ep10");
    let entry = wait_state(&engine, "ep10", EntryState::Paused);
    assert!(asked.elapsed() < Duration::from_millis(2_500), "{:?}", asked.elapsed());
    assert_eq!(entry.done, 3_000);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_wrong_final_size_fails_and_keeps_the_part() {
    let mock = Mock::start(10_000);
    mock.set(Mode::WrongSize);
    let dir = temp_dir("wrongsize");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep5")).unwrap();
    let entry = wait_state(&engine, "ep5", EntryState::Failed);
    assert!(entry.error.as_deref().unwrap_or_default().contains("416") || entry.error.as_deref().unwrap_or_default().contains("bytes"), "{entry:?}");
    assert!(engine.local_path("ep5").is_none());
    assert!(!dir.join("server1").join("ep5").join("video.bin").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn index_survives_a_reload() {
    let mock = Mock::start(30_000);
    let dir = temp_dir("reload");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep6")).unwrap();
    wait_state(&engine, "ep6", EntryState::Done);
    mock.set(Mode::Slow);
    engine.add(&item("ep7")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.entry("ep7").unwrap().done < 500 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    // A second engine on the same folder while the first runs: it reads
    // the index (the finished one is done, the one under way waits in
    // the queue), but the first holds the folder.
    let again = Engine::open(dir.clone(), options());
    assert!(!again.owns_storage());
    let entries = again.entries();
    assert_eq!(entries.len(), 2);
    let done = entries.iter().find(|e| e.item_id == "ep6").unwrap();
    assert_eq!(done.state, EntryState::Done);
    assert!(again.local_path("ep6").is_some());
    let partial = entries.iter().find(|e| e.item_id == "ep7").unwrap();
    assert_eq!(partial.state, EntryState::Queued);
    assert!(partial.done > 0);
    engine.cancel("ep7");
    wait_state(&engine, "ep7", EntryState::Paused);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn refuses_a_download_that_leaves_too_little_free_space() {
    let mock = Mock::start(10_000);
    let dir = temp_dir("space");
    let mut options = options();
    options.free_override = Some(MIN_FREE_BYTES + 5_000);
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep8")).unwrap();
    let entry = wait_state(&engine, "ep8", EntryState::Failed);
    assert!(entry.error.as_deref().unwrap().contains("free space"), "{entry:?}");
    assert_eq!(mock.ranges().len(), 0, "no file request was made");
    // With room it goes.
    engine.set_options(|o| o.free_override = Some(MIN_FREE_BYTES + 50_000));
    engine.resume("ep8");
    wait_state(&engine, "ep8", EntryState::Done);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ids_and_file_names_are_checked() {
    for ok in ["0a1b2c3d4e5f60718293a4b5c6d7e8f9", "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9", "ep1"] {
        check_id("id", ok).unwrap();
    }
    for bad in ["", "..", "a/b", "a\\b", "../x", "a.b", "a b", "a\0b", "\u{e9}", "/abs"] {
        assert!(check_id("id", bad).is_err(), "{bad:?}");
    }
    for ok in ["video.mkv", "sub-1.srt", "a b.mp4", "..x", "x.."] {
        check_file_name(ok).unwrap();
    }
    for bad in ["", ".", "..", "a/b", "../video.mkv", "/etc/passwd", "a\\b", "a\0b"] {
        assert!(check_file_name(bad).is_err(), "{bad:?}");
    }
    // The name made from the server's data is always fine.
    for source in [
        serde_json::json!({ "Container": "mkv,webm" }),
        serde_json::json!({ "Container": "../../x", "Path": "/a/b/c.MP4" }),
        serde_json::json!({ "Container": "", "Path": "/a/b/../..\\x" }),
        Value::Null,
    ] {
        check_file_name(&media_file_name(&source)).unwrap();
    }
}

#[test]
fn an_item_with_an_unsafe_id_is_refused() {
    let mock = Mock::start(10_000);
    let dir = temp_dir("refuse");
    let engine = engine(&mock, dir.clone(), options());
    for bad in ["../../evil", "a/b", ""] {
        let err = engine.add(&item(bad)).unwrap_err();
        assert!(format!("{err:#}").contains("item id"), "{err:#}");
    }
    assert!(engine.entries().is_empty());
    // A server id of this kind is refused the same way.
    let other = Engine::open(temp_dir("refuse2"), options());
    other.set_client(Some(mock.client()), "../x");
    assert!(other.add(&item("ep1")).is_err());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_index_entry_with_an_unsafe_name_is_not_loaded() {
    let dir = temp_dir("index");
    let entry = |id: &str, file: Option<&str>| {
        let mut e = Entry::from_item(&item(id), "server1", Generation(0));
        e.file = file.map(str::to_string);
        e
    };
    let index = vec![
        entry("ok1", Some("video.mkv")),
        entry("../bad", None),
        entry("ok2", Some("../../escape")),
        entry("ok3", None),
    ];
    write_json(&dir.join(INDEX_FILE), &index).unwrap();
    let loaded = load_index(&dir);
    let ids: Vec<_> = loaded.iter().map(|e| e.item_id.as_str()).collect();
    assert_eq!(ids, ["ok1", "ok3"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_restart_retries_what_failed_and_leaves_a_pause_alone() {
    let mock = Mock::start(10_000);
    let dir = temp_dir("restart");
    let mut tight = options();
    tight.free_override = Some(MIN_FREE_BYTES + 5_000);
    let first = engine(&mock, dir.clone(), tight);
    first.add(&item("ep20")).unwrap();
    wait_state(&first, "ep20", EntryState::Failed);
    // The user pauses another one before it starts.
    mock.set(Mode::Slow);
    first.set_options(|o| o.free_override = Some(MIN_FREE_BYTES + 50_000));
    first.add(&item("ep21")).unwrap();
    first.cancel("ep21");
    wait_state(&first, "ep21", EntryState::Paused);
    drop(first);
    // The next start, with room on the disk: the failed one goes by
    // itself, the paused one waits for the user.
    mock.set(Mode::Normal);
    let mut roomy = options();
    roomy.free_override = Some(MIN_FREE_BYTES + 50_000);
    let again = reopen(&mock, dir.clone(), roomy);
    wait_state(&again, "ep20", EntryState::Done);
    assert_eq!(again.entry("ep21").unwrap().state, EntryState::Paused);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn refuses_a_download_over_the_storage_limit() {
    let mock = Mock::start(10_000);
    let dir = temp_dir("limit");
    let mut options = options();
    options.limit_bytes = Some(15_000);
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep9")).unwrap();
    wait_state(&engine, "ep9", EntryState::Done);
    engine.add(&item("ep10")).unwrap();
    let entry = wait_state(&engine, "ep10", EntryState::Failed);
    assert!(entry.error.as_deref().unwrap().contains("storage limit"), "{entry:?}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_byte_limit_switch_stops_and_a_restart_resumes() {
    let mock = Mock::start(90_000);
    let dir = temp_dir("bytelimit");
    let mut options = options();
    options.byte_limit = Some(30_000);
    let engine = engine(&mock, dir.clone(), options.clone());
    engine.add(&item("ep11")).unwrap();
    let entry = wait_state(&engine, "ep11", EntryState::Paused);
    assert!(entry.done >= 30_000 && entry.done < 90_000, "{entry:?}");
    options.byte_limit = None;
    drop(engine);
    let again = reopen(&mock, dir.clone(), options);
    again.resume("ep11");
    wait_state(&again, "ep11", EntryState::Done);
    assert_eq!(mock.ranges().last().unwrap().as_deref(), Some(format!("bytes={}-", entry.done).as_str()));
    assert_eq!(fs::read(again.local_path("ep11").unwrap()).unwrap(), *mock.file);
    let _ = fs::remove_dir_all(dir);
}

/// The file on the server was replaced by another one (a better
/// release) after `meta.json` was written. The download reads the item
/// again and ends with the file the server has now. Before, it held
/// the size of the old file, fetched the new one again and again
/// against it, and failed, at each start and each reconnect.
#[test]
fn a_file_replaced_on_the_server_downloads_with_its_new_size() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("replaced-file");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep85")).unwrap();
    wait_state(&engine, "ep85", EntryState::Done);
    wait_idle(&engine);
    // As after a download of the old file that did not end: the folder
    // has the item of the old file, which was larger, and no media.
    let folder = engine.folder("ep85").unwrap();
    let media = engine.local_path("ep85").unwrap();
    fs::remove_file(&media).unwrap();
    let mut meta = engine.meta("ep85").unwrap();
    meta["Item"]["MediaSources"][0]["Size"] = serde_json::json!(9_000);
    fs::write(folder.join(META_FILE), serde_json::to_vec(&meta).unwrap()).unwrap();
    let before = mock.ranges().len();
    engine.inner.lock().entry_mut("ep85").unwrap().state = EntryState::Failed;
    engine.set_client(Some(mock.client()), "server1");
    let entry = wait_state(&engine, "ep85", EntryState::Done);
    wait_idle(&engine);
    assert_eq!(fs::metadata(&media).unwrap().len(), 5_000);
    assert_eq!(entry.total, Some(5_000), "the entry keeps the size of the old file");
    assert_eq!(mock.ranges().len() - before, 1, "the file came more than once: {:?}", mock.ranges());
    let _ = fs::remove_dir_all(dir);
}

/// The same when the file of the server is larger than the one the
/// folder was made for: nothing is written past the old size, the item
/// is read again, and the download is the new file, whole.
#[test]
fn a_larger_file_on_the_server_downloads_with_its_new_size() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("larger-file");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep87")).unwrap();
    wait_state(&engine, "ep87", EntryState::Done);
    wait_idle(&engine);
    let folder = engine.folder("ep87").unwrap();
    let media = engine.local_path("ep87").unwrap();
    fs::remove_file(&media).unwrap();
    let mut meta = engine.meta("ep87").unwrap();
    meta["Item"]["MediaSources"][0]["Size"] = serde_json::json!(2_000);
    fs::write(folder.join(META_FILE), serde_json::to_vec(&meta).unwrap()).unwrap();
    engine.inner.lock().entry_mut("ep87").unwrap().state = EntryState::Failed;
    engine.set_client(Some(mock.client()), "server1");
    let entry = wait_state(&engine, "ep87", EntryState::Done);
    wait_idle(&engine);
    assert_eq!(entry.total, Some(5_000));
    assert_eq!(fs::read(&media).unwrap(), *mock.file, "the download is not the file of the server");
    let _ = fs::remove_dir_all(dir);
}

/// A server that sends more than the size it gives, every time: the
/// download fails. Nothing past the size is written, and no file of
/// the size, cut out of a longer one, is left to pass for the whole
/// at a later try, as it did.
#[test]
fn a_server_that_sends_more_than_it_says_leaves_no_cut_file() {
    let mock = Mock::start(5_000);
    mock.set(Mode::Oversend);
    let dir = temp_dir("oversend");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep89")).unwrap();
    let entry = wait_state(&engine, "ep89", EntryState::Failed);
    wait_idle(&engine);
    assert_eq!(entry.total, Some(4_000));
    let folder = engine.folder("ep89").unwrap();
    let files: Vec<String> = fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("video"))
        .collect();
    assert!(files.is_empty(), "a media or part file is left: {files:?}");
    // The same at the next start of the download: failed again, and
    // never done with the first 4000 bytes.
    engine.resume("ep89");
    wait_state(&engine, "ep89", EntryState::Downloading);
    let entry = wait_state(&engine, "ep89", EntryState::Failed);
    wait_idle(&engine);
    assert!(engine.local_path("ep89").is_none(), "{entry:?}");
    let _ = fs::remove_dir_all(dir);
}

/// A part file of another file than the server has now (same length
/// of the whole, so the sizes say nothing) does not go on: the range
/// names the file it was started from, the server sends its file
/// whole, and the download is that file, byte for byte. Before, the
/// rest of the new file was put behind the start of the old one.
#[test]
fn a_part_of_another_file_is_not_gone_on_with() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("part-of-another-file");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep86")).unwrap();
    wait_state(&engine, "ep86", EntryState::Done);
    wait_idle(&engine);
    let media = engine.local_path("ep86").unwrap();
    let part = PathBuf::from(format!("{}.part", media.display()));
    fs::remove_file(&media).unwrap();
    // 3,000 bytes of the file the server had before, with its validator.
    fs::write(&part, vec![0xEE; 3_000]).unwrap();
    fs::write(tag_file(&part), "\"file-of-before\"").unwrap();
    engine.inner.lock().entry_mut("ep86").unwrap().state = EntryState::Failed;
    engine.set_client(Some(mock.client()), "server1");
    wait_state(&engine, "ep86", EntryState::Done);
    wait_idle(&engine);
    assert_eq!(fs::read(&media).unwrap(), *mock.file, "the download is not the file of the server");
    assert!(!tag_file(&part).exists());
    let _ = fs::remove_dir_all(dir);
}

/// Two servers hold an item with one id. The page of the item on the
/// second server does not show the download of the first, a download
/// there is its own, and removing one leaves the other. Before, the
/// entry was found by the id alone: "Downloaded" showed for the file
/// of the other server, and a click removed that file.
#[test]
fn an_item_id_on_two_servers_is_two_downloads() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("two-servers");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep88")).unwrap();
    wait_state(&engine, "ep88", EntryState::Done);
    wait_idle(&engine);
    let first = engine.folder("ep88").unwrap();
    engine.set_client(Some(mock.client()), "server2");
    assert!(engine.entry("ep88").is_none(), "the download of another server shows for this one");
    engine.add(&item("ep88")).unwrap();
    wait_state(&engine, "ep88", EntryState::Done);
    wait_idle(&engine);
    let second = engine.folder("ep88").unwrap();
    assert_ne!(first, second);
    assert_eq!(engine.entries().len(), 2);
    engine.remove("ep88");
    wait_idle(&engine);
    let left = engine.entries();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].server_id, "server1", "the download of the other server was removed");
    assert_eq!(engine.folder(Key::of(Some("server1"), "ep88")), Some(first.clone()));
    assert!(first.join(META_FILE).is_file());
    let _ = fs::remove_dir_all(dir);
}

/// An index that does not read is kept as a copy, and one entry that
/// does not read costs that entry alone. Before, either made the list
/// empty, and the next save wrote the empty list over the file.
#[test]
fn a_broken_index_is_kept_and_a_broken_entry_costs_one_entry() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("broken-index");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep80")).unwrap();
    engine.add(&item("ep81")).unwrap();
    wait_state(&engine, "ep80", EntryState::Done);
    wait_state(&engine, "ep81", EntryState::Done);
    wait_idle(&engine);
    wait_saved(&engine);
    drop(engine);
    let path = dir.join(INDEX_FILE);
    let broken = dir.join(format!("{INDEX_FILE}.broken"));
    // One entry in a state this build does not know.
    let mut listed: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    listed[0]["state"] = serde_json::json!("FromALaterBuild");
    let odd = serde_json::to_vec(&listed).unwrap();
    fs::write(&path, &odd).unwrap();
    let read = load_index(&dir);
    assert_eq!(read.len(), 1, "the entry that reads was lost with the other");
    assert_eq!(fs::read(&broken).unwrap(), odd);
    // A file cut short.
    let cut = &odd[..odd.len() / 2];
    fs::write(&path, cut).unwrap();
    assert!(load_index(&dir).is_empty());
    assert_eq!(fs::read(&broken).unwrap(), cut);
    let _ = fs::remove_dir_all(dir);
}

/// A session opens on the UI thread. With no download to queue again it
/// leaves the index on the disk alone; with one it writes the index.
#[test]
fn a_session_that_changes_no_entry_does_not_write_the_index() {
    use std::os::unix::fs::MetadataExt as _;
    let mock = Mock::start(5_000);
    let dir = temp_dir("session-no-write");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep70")).unwrap();
    // The worker writes the index for the last time after it sets this.
    wait_state(&engine, "ep70", EntryState::Done);
    wait_saved(&engine);
    // A write puts a new file in the place of the index.
    let file = || fs::metadata(dir.join(INDEX_FILE)).unwrap().ino();
    let before = file();
    engine.set_client(Some(mock.client()), "server1");
    engine.set_client(None, "server1");
    assert_eq!(file(), before, "the index was written again");
    engine.inner.lock().entry_mut("ep70").unwrap().state = EntryState::Failed;
    engine.set_client(Some(mock.client()), "server1");
    assert_ne!(file(), before, "the entry queued again was not written");
    wait_state(&engine, "ep70", EntryState::Done);
    wait_idle(&engine);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn remove_deletes_the_folder() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("remove");
    let engine = engine(&mock, dir.clone(), options());
    engine.add(&item("ep12")).unwrap();
    wait_state(&engine, "ep12", EntryState::Done);
    let folder = dir.join("server1").join("ep12");
    assert!(folder.join("video.bin").exists());
    engine.remove("ep12");
    // The entry goes at once; the files go off the lock, right after.
    assert!(engine.entry("ep12").is_none());
    assert_eq!(engine.used_bytes(), 0);
    wait_gone(&folder);
    wait_idle(&engine);
    let _ = fs::remove_dir_all(dir);
}

/// Finding 3 of the review: the user removes a download and adds it
/// again within the second; the old worker must not touch the new
/// entry, and the new download reaches the end with the whole file.
fn removed_and_added_again_reaches_the_end(parallel: usize) {
    let mock = Mock::start(20_000);
    mock.set(Mode::Slow);
    let dir = temp_dir(&format!("readd{parallel}"));
    let mut options = options();
    options.parallel = parallel;
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep30")).unwrap();
    wait_done_at_least(&engine, "ep30", 1_000);
    engine.remove("ep30");
    // The removed download is still on its slow connection; the new one
    // needs no slow server, and on a slow machine 20 kB at 64 bytes a
    // turn does not end within the wait.
    mock.set(Mode::Normal);
    engine.add(&item("ep30")).unwrap();
    let entry = wait_state(&engine, "ep30", EntryState::Done);
    assert_eq!(entry.done, 20_000);
    assert_eq!(fs::read(engine.local_path("ep30").unwrap()).unwrap(), *mock.file);
    // The new download got the whole file from the start: it did not
    // go on from the part file of the removed one.
    assert_eq!(mock.ranges(), vec![None, None]);
    assert!(!dir.join("server1").join("ep30").join("video.bin.part").exists());
    wait_idle(&engine);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_download_removed_and_added_again_at_once_goes_to_the_end() {
    removed_and_added_again_reaches_the_end(1);
}

#[test]
fn a_download_removed_and_added_again_at_once_goes_to_the_end_with_two_workers() {
    removed_and_added_again_reaches_the_end(2);
}

/// Finding 2 of the review: a worker that starts after the session
/// changed keeps the client and the folder of its job, fixed when the
/// scheduler took the entry.
#[test]
fn a_worker_that_starts_after_a_session_switch_keeps_its_own_client_and_folder() {
    let (a, b) = (Mock::start(5_000), Mock::start(7_000));
    let dir = temp_dir("switch");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let mut options = options();
    options.start_gate = Some(gate.clone());
    let engine = Engine::open(dir.clone(), options);
    engine.set_client(Some(a.client()), "server-a");
    engine.add(&item("ep60")).unwrap();
    // The scheduler gave the worker its job; before its first step the
    // session changes to server B.
    wait_state(&engine, "ep60", EntryState::Downloading);
    engine.set_client(Some(b.client()), "server-b");
    gate.wait();
    let entry = wait_state(&engine, "ep60", EntryState::Done);
    assert_eq!(entry.server_id, "server-a");
    assert_eq!(entry.done, 5_000);
    assert_eq!(fs::read(dir.join("server-a").join("ep60").join("video.bin")).unwrap(), *a.file);
    assert!(!dir.join("server-b").exists());
    assert_eq!(a.ranges().len(), 1);
    assert_eq!(b.hits(), 0, "the worker asked server B for something");
    let _ = fs::remove_dir_all(dir);
}

/// A worker that gets to run only after its entry was removed, and the
/// item added again, writes nothing: not a request, not a file. The
/// new entry's worker starts after it, in a clean folder.
#[test]
fn a_late_worker_of_a_removed_entry_writes_nothing() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("late");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let mut options = options();
    options.start_gate = Some(gate.clone());
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep61")).unwrap();
    wait_state(&engine, "ep61", EntryState::Downloading);
    engine.remove("ep61");
    engine.add(&item("ep61")).unwrap();
    // The old worker goes now, with its entry gone; the new entry waits
    // for the tombstone, so the next one at the gate is its worker.
    gate.wait();
    gate.wait();
    let entry = wait_state(&engine, "ep61", EntryState::Done);
    assert_eq!(entry.done, 5_000);
    assert_eq!(fs::read(engine.local_path("ep61").unwrap()).unwrap(), *mock.file);
    // One item request, one segments request, one file request: the
    // old worker made none.
    assert_eq!(mock.ranges().len(), 1);
    assert_eq!(mock.hits(), 3);
    wait_idle(&engine);
    let _ = fs::remove_dir_all(dir);
}

/// One process holds the download folder; a second engine on it reads
/// the index and plays the files, but adds, changes and writes nothing.
#[test]
fn a_second_engine_on_the_same_folder_runs_with_downloads_off() {
    let mock = Mock::start(5_000);
    let dir = temp_dir("lock");
    let first = engine(&mock, dir.clone(), options());
    first.add(&item("ep70")).unwrap();
    wait_state(&first, "ep70", EntryState::Done);
    wait_saved(&first);
    let second = Engine::open(dir.clone(), options());
    second.set_client(Some(mock.client()), "server1");
    assert!(!second.owns_storage());
    let err = second.add(&item("ep71")).unwrap_err();
    assert!(err.downcast_ref::<NotOwner>().is_some(), "{err:#}");
    assert_eq!(second.entries().len(), 1, "it reads the index");
    assert!(second.local_path("ep70").is_some(), "it plays the files");
    let index_before = fs::read(dir.join(INDEX_FILE)).unwrap();
    second.remove("ep70");
    second.cancel("ep70");
    second.resume("ep70");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(fs::read(dir.join(INDEX_FILE)).unwrap(), index_before, "it wrote the index");
    assert!(first.local_path("ep70").is_some(), "it removed the files of the first");
    // Once the first is gone, a new engine holds the folder.
    drop(first);
    drop(second);
    let third = reopen(&mock, dir.clone(), options());
    assert!(third.owns_storage());
    third.add(&item("ep71")).unwrap();
    wait_state(&third, "ep71", EntryState::Done);
    let _ = fs::remove_dir_all(dir);
}

fn wait_settled(engine: &Engine, ids: &[&str]) -> Vec<Entry> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let entries: Vec<Entry> = ids.iter().map(|id| engine.entry(*id).expect("entry")).collect();
        if entries.iter().all(|e| !e.is_active()) {
            return entries;
        }
        assert!(Instant::now() < deadline, "{entries:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Finding 4 of the review: two downloads at once must share the
/// storage limit; each alone fits, together they do not.
#[test]
fn two_downloads_at_once_share_the_storage_limit() {
    let mock = Mock::start(10_000);
    mock.set(Mode::Slow);
    let dir = temp_dir("limit2");
    let mut options = options();
    options.parallel = 2;
    options.limit_bytes = Some(15_000);
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep40")).unwrap();
    engine.add(&item("ep41")).unwrap();
    let entries = wait_settled(&engine, &["ep40", "ep41"]);
    let done = entries.iter().filter(|e| e.state == EntryState::Done).count();
    assert_eq!(done, 1, "{entries:?}");
    assert!(engine.used_bytes() <= 15_000, "{}", engine.used_bytes());
    let _ = fs::remove_dir_all(dir);
}

/// The same for the reserve of free space.
#[test]
fn two_downloads_at_once_share_the_free_space() {
    let mock = Mock::start(10_000);
    mock.set(Mode::Slow);
    let dir = temp_dir("free2");
    let mut options = options();
    options.parallel = 2;
    options.free_override = Some(MIN_FREE_BYTES + 15_000);
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep42")).unwrap();
    engine.add(&item("ep43")).unwrap();
    let entries = wait_settled(&engine, &["ep42", "ep43"]);
    let done = entries.iter().filter(|e| e.state == EntryState::Done).count();
    assert_eq!(done, 1, "{entries:?}");
    let _ = fs::remove_dir_all(dir);
}

/// A download whose size the server did not say is not admitted on a
/// size; its bytes are checked as they come, and it stops with a clear
/// word at the limit.
#[test]
fn a_download_of_unknown_size_stops_at_the_storage_limit() {
    let mock = Mock::start(40_000);
    mock.set(Mode::NoSize);
    let dir = temp_dir("nosize");
    let mut options = options();
    options.limit_bytes = Some(15_000);
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep44")).unwrap();
    let entry = wait_state(&engine, "ep44", EntryState::Failed);
    assert_eq!(entry.total, None);
    assert!(entry.error.as_deref().unwrap().contains("storage limit"), "{entry:?}");
    // Nothing past the limit was written.
    assert!(entry.done <= 15_000, "{entry:?}");
    let part = dir.join("server1").join("ep44").join("video.bin.part");
    assert!(fs::metadata(&part).unwrap().len() <= 15_000);
    assert!(engine.local_path("ep44").is_none());
    let _ = fs::remove_dir_all(dir);
}

/// And at the reserve of free space.
#[test]
fn a_download_of_unknown_size_stops_when_the_reserve_would_be_crossed() {
    let mock = Mock::start(40_000);
    mock.set(Mode::NoSize);
    let dir = temp_dir("nosizefree");
    let mut options = options();
    options.free_override = Some(MIN_FREE_BYTES - 1);
    let engine = engine(&mock, dir.clone(), options);
    engine.add(&item("ep45")).unwrap();
    let entry = wait_state(&engine, "ep45", EntryState::Failed);
    assert!(entry.error.as_deref().unwrap().contains("free space"), "{entry:?}");
    assert_eq!(entry.done, 0, "{entry:?}");
    let _ = fs::remove_dir_all(dir);
}

/// A server that takes one request, keeps its head, and answers with a
/// subtitle file.
fn one_request_server() -> (u16, Arc<Mutex<Option<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(None));
    let kept = seen.clone();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let Some(request) = Request::read(&mut stream) else { return };
        *kept.lock().unwrap() = Some(request.head);
        let body = b"1\n00:00:00,000 --> 00:00:01,000\nHi\n";
        let _ = write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
        let _ = stream.write_all(body);
    });
    (port, seen)
}

/// Finding 6 of the review: the address the server gives for a subtitle
/// is resolved as a URL, and the session header goes to the own server
/// only.
#[test]
fn a_subtitle_address_is_resolved_and_the_session_goes_to_the_own_server_only() {
    let base = "https://jf.example.com:8920";
    let own = |url: &str| crate::jellyfin::same_origin(url, base);
    // A path, with or without the slash: the own server, with the session.
    let url = resolve_url(base, "/Videos/i/s/Subtitles/3/0/Stream.srt").unwrap();
    assert_eq!(url, "https://jf.example.com:8920/Videos/i/s/Subtitles/3/0/Stream.srt");
    assert!(own(&url));
    let url = resolve_url(base, "Videos/x.srt").unwrap();
    assert_eq!(url, "https://jf.example.com:8920/Videos/x.srt");
    assert!(own(&url));
    // An absolute address of the own origin: with the session.
    let url = resolve_url(base, "https://jf.example.com:8920/sub.srt").unwrap();
    assert_eq!(url, "https://jf.example.com:8920/sub.srt");
    assert!(own(&url));
    assert!(own(&resolve_url(base, "HTTPS://JF.example.com:8920/sub.srt").unwrap()));
    // Another origin: fetched, without the session. Another host,
    // another port, and plain http from an https server.
    for other in [
        "https://cdn.example.com/sub.srt",
        "https://jf.example.com:8096/sub.srt",
        "http://jf.example.com:8920/sub.srt",
        "http://jf.example.com/sub.srt",
    ] {
        let url = resolve_url(base, other).unwrap();
        assert_eq!(url, other);
        assert!(!own(&url), "{other} got the session");
    }
    // The scheme of the server for an address without one.
    let url = resolve_url(base, "//cdn.example.com/sub.srt").unwrap();
    assert_eq!(url, "https://cdn.example.com/sub.srt");
    assert!(!own(&url));
    // Schemes this app does not fetch.
    for bad in ["file:///etc/passwd", "ftp://x/y.srt", "javascript:alert(1)", "mailto:a@b", "data:text/plain,hi"] {
        assert_eq!(resolve_url(base, bad), None, "{bad}");
    }
}

/// The same, end to end: the header of the request as the host sees it.
#[test]
fn the_session_header_goes_to_a_subtitle_of_the_own_server_only() {
    let dir = temp_dir("suborigin");
    let agent = agent(Duration::from_secs(5), Arc::new(AtomicBool::new(false)));
    let stream = |url: String| {
        serde_json::json!({ "Id": "src", "MediaStreams": [
            { "Type": "Subtitle", "IsExternal": true, "Index": 3, "Codec": "subrip", "DeliveryUrl": url }
        ]})
    };
    let live = || true;
    // A foreign host.
    let (foreign, seen) = one_request_server();
    let client = Client::new("http://127.0.0.1:1", "d").with_session("token-under-test", "u");
    let out = fetch_subtitles(&agent, &client, "item", &stream(format!("http://127.0.0.1:{foreign}/sub.srt")), &dir, &live);
    let head = seen.lock().unwrap().clone().expect("the foreign host got the request");
    assert!(!head.to_lowercase().contains("authorization:"), "the token went to a foreign host:\n{head}");
    assert_eq!(out.len(), 1);
    // The own server, with an absolute address.
    let _ = fs::remove_dir_all(&dir);
    let (own, seen) = one_request_server();
    let client = Client::new(&format!("http://127.0.0.1:{own}"), "d").with_session("token-under-test", "u");
    fetch_subtitles(&agent, &client, "item", &stream(format!("http://127.0.0.1:{own}/Videos/item/src/Subtitles/3/0/Stream.srt")), &dir, &live);
    let head = seen.lock().unwrap().clone().expect("the own server got the request");
    assert!(head.contains("token-under-test"), "no session header for the own server:\n{head}");
    // An address with a scheme this app does not fetch is left out.
    let _ = fs::remove_dir_all(&dir);
    let out = fetch_subtitles(&agent, &client, "item", &stream("file:///etc/passwd".into()), &dir, &live);
    assert!(out.is_empty());
    let _ = fs::remove_dir_all(dir);
}

/// An image sidecar: a PGS is kept as the `.sup` the server serves as
/// it is; a VobSub pair gets no file from the server and is left out.
#[test]
fn a_pgs_sidecar_is_kept_and_a_vobsub_pair_is_not() {
    let dir = temp_dir("subpgs");
    let agent = agent(Duration::from_secs(5), Arc::new(AtomicBool::new(false)));
    let (own, seen) = one_request_server();
    let client = Client::new(&format!("http://127.0.0.1:{own}"), "d").with_session("t", "u");
    let source = serde_json::json!({ "Id": "src", "MediaStreams": [
        { "Type": "Subtitle", "IsExternal": true, "Index": 0, "Codec": "DVDSUB", "DisplayTitle": "English - DVDSUB - External" },
        { "Type": "Subtitle", "IsExternal": true, "Index": 1, "Codec": "PGSSUB", "DisplayTitle": "English - PGSSUB - External" },
        { "Type": "Subtitle", "IsExternal": false, "Index": 5, "Codec": "PGSSUB" }
    ]});
    let out = fetch_subtitles(&agent, &client, "item", &source, &dir, &|| true);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0]["File"], "sub-1.sup");
    assert_eq!(out[0]["Index"], 1);
    assert!(dir.join("sub-1.sup").is_file());
    let head = seen.lock().unwrap().clone().expect("the server got the request");
    assert!(head.starts_with("GET /Videos/item/src/Subtitles/1/0/Stream.pgssub "), "{head}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn free_space_agrees_with_the_disk() {
    let free = free_space(&std::env::temp_dir()).expect("free space");
    // A few kilobytes at least, and under a petabyte.
    assert!(free > 4096 && free < 1_000_000_000_000_000, "{free}");
}

#[test]
fn formats_bytes() {
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(347_800_000), "348 MB");
    assert_eq!(format_bytes(5_000_000_000), "5.0 GB");
}
