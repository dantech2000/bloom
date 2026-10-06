// SPDX-License-Identifier: AGPL-3.0-or-later
//! The download engine: a queue of items, worker threads that fetch the
//! files, and an index on disk that survives a restart. Nothing of GPUI is
//! in here; the UI reads a snapshot and polls the version counter.
//!
//! One item lives in `<dir>/<server id>/<item id>/`: the media file, its
//! `meta.json` (the item as the server sent it, plus the segments and the
//! subtitle files), a poster, a backdrop and the external subtitles. The
//! media file is written as `<name>.part` and renamed when its size is the
//! one the server announced, so a complete file is never half written.
//!
//! One process owns the folder at a time (`lock`, held while the engine
//! lives). A second instance on the same folder reads the index and plays
//! the files, but downloads nothing and writes nothing.
//!
//! A worker gets a [`Job`] from the scheduler: the entry's identity and
//! generation, the client, the folder and the stop flag, all fixed under
//! the lock. It touches the entry and the folder only while the entry still
//! has that generation. A removed entry leaves a tombstone on its folder
//! until its worker is out and the files are gone; no job starts in that
//! folder before.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jellyfin::{Client, Item};

/// Free space that must stay on the disk after a download.
pub const MIN_FREE_BYTES: u64 = 5_000_000_000;
/// Bytes read from the server in one step.
const CHUNK: usize = 256 * 1024;
/// Tries of one download before it counts as failed.
const ATTEMPTS: u32 = 6;
const INDEX_FILE: &str = "index.json";
/// The lock of the folder: one process downloads at a time.
const LOCK_FILE: &str = "lock";
pub const META_FILE: &str = "meta.json";
pub const POSTER_FILE: &str = "poster.img";
pub const BACKDROP_FILE: &str = "backdrop.img";
/// Poster of the series of an episode.
pub const SERIES_FILE: &str = "series.img";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryState {
    /// Waits for a worker.
    Queued,
    Downloading,
    /// Stopped by the user; the part file stays. It waits for "Resume",
    /// also after a restart of the app.
    Paused,
    /// Gave up; `error` says why. A retry starts from the part file, and
    /// one runs at the next start and when the server is reachable again.
    Failed,
    Done,
}

/// One item of the index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub server_id: String,
    pub item_id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub series_name: Option<String>,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub season: Option<i32>,
    #[serde(default)]
    pub episode: Option<i32>,
    #[serde(default)]
    pub year: Option<i32>,
    /// Size of the media file, once the server said it.
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub done: u64,
    pub state: EntryState,
    #[serde(default)]
    pub error: Option<String>,
    /// Name of the media file in the folder of the item.
    #[serde(default)]
    pub file: Option<String>,
    /// Unix seconds when the item was added.
    #[serde(default)]
    pub added: i64,
    /// Bytes per second right now; 0 when not downloading.
    #[serde(skip)]
    pub speed: f64,
    /// The stop flag of the job under way; set by a cancel and by a remove.
    #[serde(skip)]
    cancel: Arc<AtomicBool>,
    /// The life of the entry a worker may act on; see [`Generation`].
    #[serde(skip)]
    generation: Generation,
    /// The space check let this download through: its whole size counts
    /// against the limit and the reserve for the downloads admitted after.
    #[serde(skip)]
    admitted: bool,
}

/// Counts the lives of the entries: a new one for every add and for every
/// job that starts. A worker holds the generation of its job and acts on an
/// entry, or writes in its folder, only while the entry still has it. The
/// entry of an item that was removed and added again has a new one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Generation(u64);

/// Another instance holds the download folder: this one downloads nothing.
#[derive(Debug)]
pub struct NotOwner;

impl std::fmt::Display for NotOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another instance of the app uses the download folder")
    }
}

impl std::error::Error for NotOwner {}

impl Entry {
    /// Refuses an entry whose ids or file name are not safe in a path. The
    /// ids and the file name come from the server and become folders.
    pub fn validate(&self) -> Result<()> {
        check_id("server id", &self.server_id)?;
        check_id("item id", &self.item_id)?;
        if let Some(file) = &self.file {
            check_file_name(file)?;
        }
        Ok(())
    }

    fn from_item(item: &Item, server_id: &str, generation: Generation) -> Self {
        Self {
            server_id: server_id.to_string(),
            item_id: item.id.clone(),
            name: item.name.clone(),
            kind: item.kind.clone(),
            series_name: item.series_name.clone(),
            series_id: item.series_id.clone(),
            season: item.parent_index_number,
            episode: item.index_number,
            year: item.production_year,
            total: None,
            done: 0,
            state: EntryState::Queued,
            error: None,
            file: None,
            added: unix_now(),
            speed: 0.,
            cancel: Arc::new(AtomicBool::new(false)),
            generation,
            admitted: false,
        }
    }

    /// Takes the names of the item as the server has them.
    fn fill_from(&mut self, item: &Value) {
        let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
        let number = |key: &str| item.get(key).and_then(Value::as_i64).map(|n| n as i32);
        if let Some(name) = text("Name") {
            self.name = name;
        }
        if let Some(kind) = text("Type") {
            self.kind = kind;
        }
        self.series_name = text("SeriesName").or(self.series_name.take());
        self.series_id = text("SeriesId").or(self.series_id.take());
        self.season = number("ParentIndexNumber").or(self.season);
        self.episode = number("IndexNumber").or(self.episode);
        self.year = number("ProductionYear").or(self.year);
    }

    /// Progress from 0 to 1, when the size is known.
    pub fn progress(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done as f64 / total as f64).clamp(0., 1.) as f32)
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, EntryState::Queued | EntryState::Downloading)
    }

    /// The heading the entry sits under on the Downloads page.
    pub fn group(&self) -> String {
        self.series_name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Movies".to_string())
    }

    /// "S1:E2 · Name" for an episode, "Name (2014)" for a movie.
    pub fn title(&self) -> String {
        match (self.season, self.episode) {
            (Some(s), Some(e)) if self.kind == "Episode" => format!("S{s}:E{e} · {}", self.name),
            (None, Some(e)) if self.kind == "Episode" => format!("E{e} · {}", self.name),
            _ => match self.year {
                Some(year) => format!("{} ({year})", self.name),
                None => self.name.clone(),
            },
        }
    }

    /// Words for the state: "Queued", "3.2 MB/s", "Paused at 40%", ...
    pub fn state_text(&self) -> String {
        let percent = || (self.progress().unwrap_or(0.) * 100.).round() as u32;
        match self.state {
            EntryState::Queued => "Queued".to_string(),
            EntryState::Downloading => {
                if self.speed > 0. {
                    format!("{}% · {}/s", percent(), format_bytes(self.speed as u64))
                } else {
                    format!("{}%", percent())
                }
            }
            EntryState::Paused => format!("Paused at {}%", percent()),
            EntryState::Failed => self.error.clone().unwrap_or_else(|| "Failed".to_string()),
            EntryState::Done => "Downloaded".to_string(),
        }
    }
}

/// Settings of an engine. The defaults are the ones of the app.
#[derive(Clone, Debug)]
pub struct Options {
    /// Downloads that run at the same time: 1 or 2.
    pub parallel: usize,
    /// Space all downloads may take; none for no limit.
    pub limit_bytes: Option<u64>,
    /// Free space to assume, for tests.
    pub free_override: Option<u64>,
    /// Wait before the first retry; it doubles each time.
    pub retry_wait: Duration,
    /// Stops a download after this many bytes of one run (a test switch:
    /// `BLOOM_DOWNLOAD_LIMIT_BYTES`).
    pub byte_limit: Option<u64>,
    /// Slows a download to this many bytes a second (a test switch:
    /// `BLOOM_DOWNLOAD_THROTTLE_BPS`).
    pub throttle_bps: Option<u64>,
    /// A read that gets no byte for this long ends the try; the retry goes
    /// on with `Range`.
    pub stall: Duration,
    /// A test holds every worker here before its first step, to arrange
    /// what happens before the worker runs.
    #[cfg(test)]
    pub start_gate: Option<Arc<std::sync::Barrier>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            parallel: 1,
            limit_bytes: None,
            free_override: None,
            retry_wait: Duration::from_secs(1),
            byte_limit: std::env::var("BLOOM_DOWNLOAD_LIMIT_BYTES")
                .ok()
                .and_then(|v| v.parse().ok()),
            throttle_bps: std::env::var("BLOOM_DOWNLOAD_THROTTLE_BPS")
                .ok()
                .and_then(|v| v.parse().ok()),
            stall: Duration::from_secs(30),
            #[cfg(test)]
            start_gate: None,
        }
    }
}

/// The folder of a removed entry while its files are still in use: a
/// worker is in it, or its deletion runs. No job starts in that folder.
struct Tombstone {
    server_id: String,
    item_id: String,
    /// The generation of the removed entry; the one who clears the stone
    /// finds it by this.
    generation: Generation,
}

struct State {
    dir: PathBuf,
    client: Option<Client>,
    server_id: String,
    options: Options,
    entries: Vec<Entry>,
    running: usize,
    /// The lock of the folder, held while the engine lives. `None` when
    /// another instance holds it: this one reads and writes nothing.
    owner: Option<fs::File>,
    /// The last generation given out.
    generations: u64,
    tombstones: Vec<Tombstone>,
}

impl State {
    fn entry_mut(&mut self, item_id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.item_id == item_id)
    }

    fn entry(&self, item_id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.item_id == item_id)
    }

    fn next_generation(&mut self) -> Generation {
        self.generations += 1;
        Generation(self.generations)
    }

    /// The entry a job works on: the one with its generation. `None` when
    /// the entry was removed, also when the item was added again since.
    fn owned_mut(&mut self, job: &Job) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| e.generation == job.generation && e.item_id == job.item_id)
    }

    fn owned(&self, job: &Job) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.generation == job.generation && e.item_id == job.item_id)
    }

    fn folder(&self, entry: &Entry) -> PathBuf {
        self.dir.join(&entry.server_id).join(&entry.item_id)
    }

    fn tombstoned(&self, server_id: &str, item_id: &str) -> bool {
        self.tombstones
            .iter()
            .any(|t| t.server_id == server_id && t.item_id == item_id)
    }

    fn clear_tombstone(&mut self, generation: Generation) {
        self.tombstones.retain(|t| t.generation != generation);
    }

    /// Bytes on the disk for all entries but one. A download under way
    /// counts with the size it will have: two at once share the limit.
    fn used_except(&self, item_id: &str) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.item_id != item_id)
            .map(|e| match e.admitted && e.state == EntryState::Downloading {
                true => e.total.map_or(e.done, |t| t.max(e.done)),
                false => e.done,
            })
            .sum()
    }

    /// Bytes the downloads under way still have to put on the disk, all
    /// entries but one.
    fn promised_except(&self, item_id: &str) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.item_id != item_id && e.admitted && e.state == EntryState::Downloading)
            .map(|e| e.total.map_or(0, |t| t.saturating_sub(e.done)))
            .sum()
    }
}

/// What a worker works on, fixed by the scheduler under the lock. The
/// worker uses nothing of the state that it did not get here, so a session
/// that changes after the start does not reach into a download under way.
#[derive(Clone)]
struct Job {
    server_id: String,
    item_id: String,
    generation: Generation,
    client: Client,
    folder: PathBuf,
    cancel: Arc<AtomicBool>,
    options: Options,
}

impl Job {
    /// Stops the job when its entry is gone or the user stopped it: a check
    /// before every write.
    fn check(&self, inner: &Inner) -> Result<(), Stop> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(Stop::Cancelled);
        }
        let s = inner.lock();
        match s.owned(self) {
            Some(_) => Ok(()),
            None => Err(Stop::Cancelled),
        }
    }

    fn live(&self, inner: &Inner) -> bool {
        self.check(inner).is_ok()
    }
}

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    /// Counts every change; the UI redraws when it moved.
    version: AtomicU64,
}

impl Inner {
    fn bump(&self) {
        self.version.fetch_add(1, Ordering::Release);
    }

    /// The state. A thread that panicked under the lock leaves it poisoned;
    /// every change under it is a few assignments that leave the state
    /// whole, so the rest of the app goes on with it.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Handle of the engine; cheap to clone.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    /// Opens the index in `dir` and starts the scheduler. Downloads of the
    /// run before that were under way wait in the queue; they start once a
    /// client is set.
    pub fn open(dir: PathBuf, options: Options) -> Self {
        let _ = fs::create_dir_all(&dir);
        // The instance before this one may still be on its way out (a
        // restart): a few tries, then the scheduler keeps trying.
        let mut owner = None;
        for _ in 0..5 {
            owner = lock_folder(&dir);
            if owner.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if owner.is_none() {
            log::warn!("another instance uses {}: downloads are off here", dir.display());
        }
        let mut generations = 0;
        let entries = load_entries(&dir, &mut generations);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                dir,
                client: None,
                server_id: String::new(),
                options,
                entries,
                running: 0,
                owner,
                generations,
                tombstones: Vec::new(),
            }),
            wake: Condvar::new(),
            version: AtomicU64::new(1),
        });
        // The scheduler holds the engine only while it looks at it, so an
        // engine that is dropped goes, and its lock on the folder with it.
        let scheduler = Arc::downgrade(&inner);
        let _ = thread::Builder::new()
            .name("downloads".into())
            .spawn(move || schedule(scheduler));
        Self { inner }
    }

    /// True when this process holds the download folder. Another instance
    /// on the same folder reads it and downloads nothing.
    pub fn owns_storage(&self) -> bool {
        self.inner.lock().owner.is_some()
    }

    /// The session the downloads use. Entries of another server wait.
    pub fn set_client(&self, client: Option<Client>, server_id: &str) {
        let mut s = self.inner.lock();
        // The server answers again: the downloads that gave up try again.
        if client.is_some() && s.owner.is_some() {
            for entry in s.entries.iter_mut().filter(|e| e.server_id == server_id) {
                if entry.state == EntryState::Failed {
                    entry.state = EntryState::Queued;
                    entry.error = None;
                }
            }
            save_index(&s);
        }
        s.client = client;
        s.server_id = server_id.to_string();
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    pub fn set_options(&self, change: impl FnOnce(&mut Options)) {
        let mut s = self.inner.lock();
        change(&mut s.options);
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    pub fn dir(&self) -> PathBuf {
        self.inner.lock().dir.clone()
    }

    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Acquire)
    }

    /// Puts an item in the queue. One that is paused or failed starts again
    /// from its part file; one that is downloaded or under way stays as it is.
    /// An item whose ids are not safe for a path is refused with an error,
    /// and so is every item while another instance holds the folder
    /// ([`NotOwner`]).
    pub fn add(&self, item: &Item) -> Result<()> {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return Err(NotOwner.into());
        }
        let server_id = s.server_id.clone();
        match s.entry_mut(&item.id) {
            Some(entry) => {
                if matches!(entry.state, EntryState::Paused | EntryState::Failed) {
                    entry.state = EntryState::Queued;
                    entry.error = None;
                }
            }
            None => {
                let generation = s.next_generation();
                let entry = Entry::from_item(item, &server_id, generation);
                entry.validate()?;
                s.entries.push(entry);
            }
        }
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
        Ok(())
    }

    /// Stops a download; its part file stays for a resume.
    pub fn cancel(&self, item_id: &str) {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        if let Some(entry) = s.entry_mut(item_id) {
            match entry.state {
                EntryState::Queued => entry.state = EntryState::Paused,
                EntryState::Downloading => entry.cancel.store(true, Ordering::Release),
                _ => {}
            }
        }
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    /// Queues a paused or failed download again.
    pub fn resume(&self, item_id: &str) {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        if let Some(entry) = s.entry_mut(item_id)
            && matches!(entry.state, EntryState::Paused | EntryState::Failed)
        {
            entry.state = EntryState::Queued;
            entry.error = None;
        }
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    /// Removes an item and its files. The entry goes at once; the files go
    /// off the lock, after the worker of the entry (when one runs) is out
    /// of the folder. Until then the folder has a tombstone: a download of
    /// the same item added again waits for it.
    pub fn remove(&self, item_id: &str) {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        let Some(at) = s.entries.iter().position(|e| e.item_id == item_id) else {
            return;
        };
        let entry = s.entries.remove(at);
        entry.cancel.store(true, Ordering::Release);
        s.tombstones.push(Tombstone {
            server_id: entry.server_id.clone(),
            item_id: entry.item_id.clone(),
            generation: entry.generation,
        });
        let folder = s.folder(&entry);
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
        drop(s);
        // A worker in the folder deletes it when it is out (`work`). With
        // no worker the deletion goes on a thread of its own: unlinking a
        // large file takes tens of milliseconds, too long for the UI.
        if entry.state != EntryState::Downloading {
            let inner = self.inner.clone();
            let generation = entry.generation;
            let _ = thread::Builder::new().name("download-remove".into()).spawn(move || {
                let _ = fs::remove_dir_all(&folder);
                inner.lock().clear_tombstone(generation);
                inner.bump();
                inner.wake.notify_all();
            });
        }
    }

    pub fn remove_all(&self) {
        let ids: Vec<String> = self.entries().into_iter().map(|e| e.item_id).collect();
        for id in ids {
            self.remove(&id);
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.inner.lock().entries.clone()
    }

    pub fn entry(&self, item_id: &str) -> Option<Entry> {
        self.inner.lock().entry(item_id).cloned()
    }

    /// Bytes of all downloads on the disk.
    pub fn used_bytes(&self) -> u64 {
        self.inner.lock().entries.iter().map(|e| e.done).sum()
    }

    /// Free space of the disk that holds the downloads.
    pub fn free_bytes(&self) -> Option<u64> {
        let s = self.inner.lock();
        s.options.free_override.or_else(|| free_space(&s.dir))
    }

    /// The folder of an item's files.
    pub fn folder(&self, item_id: &str) -> Option<PathBuf> {
        let s = self.inner.lock();
        s.entry(item_id).map(|e| s.folder(e))
    }

    /// The complete media file of an item, when it is there.
    #[cfg(test)]
    pub fn local_path(&self, item_id: &str) -> Option<PathBuf> {
        self.local_path_of(None, item_id)
    }

    /// The complete media file of an item of the server `server_id`, when
    /// it is there. Two servers can hold an item under the same id (the
    /// id comes from the path of the file), so the file of one server is
    /// not the item of another: with a server named, only its own entry
    /// counts. `None` for the server is a caller with no session.
    pub fn local_path_of(&self, server_id: Option<&str>, item_id: &str) -> Option<PathBuf> {
        let s = self.inner.lock();
        let entry = s.entry(item_id)?;
        if server_id.is_some_and(|server| entry.server_id != server) {
            return None;
        }
        if entry.state != EntryState::Done {
            return None;
        }
        let path = s.folder(entry).join(entry.file.as_deref()?);
        path.is_file().then_some(path)
    }

    /// The file on this Mac that holds the image of a server URL such as
    /// `/Items/{id}/Images/Primary`: the poster or backdrop of a downloaded
    /// item, or of the series one belongs to. The pages then show the
    /// artwork of a download without the server.
    pub fn local_image(&self, url: &str) -> Option<PathBuf> {
        let rest = url.split_once("/Items/")?.1;
        let (id, rest) = rest.split_once("/Images/")?;
        let kind = rest.split(['?', '/']).next()?;
        let s = self.inner.lock();
        let own = s.entry(id);
        let name = match (kind, own) {
            ("Primary", Some(_)) => POSTER_FILE,
            ("Backdrop", Some(_)) => BACKDROP_FILE,
            ("Primary", None) => SERIES_FILE,
            ("Backdrop", None) => BACKDROP_FILE,
            _ => return None,
        };
        let entry = own.or_else(|| s.entries.iter().find(|e| e.series_id.as_deref() == Some(id)))?;
        let path = s.folder(entry).join(name);
        path.is_file().then_some(path)
    }

    /// The `meta.json` of an item, parsed.
    pub fn meta(&self, item_id: &str) -> Option<Value> {
        let folder = self.folder(item_id)?;
        let bytes = fs::read(folder.join(META_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The item of `meta.json`, as the app's `Item`.
    pub fn item(&self, item_id: &str) -> Option<Item> {
        let meta = self.meta(item_id)?;
        serde_json::from_value(meta.get("Item")?.clone()).ok()
    }

    /// Workers under way, and folders with a tombstone.
    #[cfg(test)]
    fn busy(&self) -> (usize, usize) {
        let s = self.inner.lock();
        (s.running, s.tombstones.len())
    }
}

// ----- scheduler and workers ---------------------------------------------------

/// The index, as the engine starts on it: what the run before left
/// unfinished goes on, and the part files say how far it got. Each entry
/// gets a generation after `generations`.
fn load_entries(dir: &Path, generations: &mut u64) -> Vec<Entry> {
    let mut entries = load_index(dir);
    for entry in &mut entries {
        // A download that was under way, and one that gave up (the cause
        // may be gone), go on.
        if matches!(entry.state, EntryState::Downloading | EntryState::Failed) {
            entry.state = EntryState::Queued;
            entry.error = None;
        }
        // The part file knows better than the index how far it got.
        if entry.state != EntryState::Done
            && let Some(file) = &entry.file
        {
            let part = dir.join(&entry.server_id).join(&entry.item_id).join(format!("{file}.part"));
            if let Ok(meta) = fs::metadata(part) {
                entry.done = meta.len();
            }
        }
        entry.speed = 0.;
        entry.cancel = Arc::new(AtomicBool::new(false));
        *generations += 1;
        entry.generation = Generation(*generations);
        entry.admitted = false;
    }
    entries
}

/// Starts workers for queued entries while there is room. Each worker gets
/// its job here, under the lock, so what it works on is fixed at the start.
fn schedule(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else { break };
        let mut s = inner.lock();
        // The folder was held by another instance at the start: once it
        // is free, this one takes it, with the index as that instance
        // left it.
        if s.owner.is_none()
            && let Some(lock) = lock_folder(&s.dir)
        {
            log::info!("the download folder is free now: downloads are on");
            s.owner = Some(lock);
            let mut generations = s.generations;
            s.entries = load_entries(&s.dir, &mut generations);
            s.generations = generations;
            inner.bump();
        }
        while s.owner.is_some() && s.running < s.options.parallel.max(1) {
            let Some(client) = s.client.clone() else { break };
            let server_id = s.server_id.clone();
            let next = s.entries.iter().position(|e| {
                e.state == EntryState::Queued
                    && e.server_id == server_id
                    && !s.tombstoned(&e.server_id, &e.item_id)
            });
            let Some(at) = next else { break };
            let generation = s.next_generation();
            let folder = s.dir.join(&server_id).join(&s.entries[at].item_id);
            let options = s.options.clone();
            let entry = &mut s.entries[at];
            let job = Job {
                server_id,
                item_id: entry.item_id.clone(),
                generation,
                client,
                folder,
                cancel: Arc::new(AtomicBool::new(false)),
                options,
            };
            entry.state = EntryState::Downloading;
            entry.error = None;
            entry.generation = generation;
            entry.cancel = job.cancel.clone();
            entry.admitted = false;
            s.running += 1;
            inner.bump();
            let worker = inner.clone();
            let _ = thread::Builder::new()
                .name(format!("download-{}", &job.item_id[..job.item_id.len().min(8)]))
                .spawn(move || work(worker, job));
        }
        let _ = inner
            .wake
            .wait_timeout(s, Duration::from_millis(500))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// Why a download stopped before its end.
enum Stop {
    Cancelled,
    /// The test switch stopped it.
    Limit,
    /// A try failed; the next try may go.
    Failed(String),
    /// The storage refused it: no retry, the word stays on the entry.
    Refused(String),
}

fn work(inner: Arc<Inner>, job: Job) {
    #[cfg(test)]
    if let Some(gate) = &job.options.start_gate {
        gate.wait();
    }
    let outcome = download(&inner, &job);
    let mut s = inner.lock();
    s.running = s.running.saturating_sub(1);
    match s.owned_mut(&job) {
        Some(entry) => {
            entry.speed = 0.;
            entry.admitted = false;
            match outcome {
                Ok(()) => {
                    entry.state = EntryState::Done;
                    entry.error = None;
                    log::info!("download done: {}", entry.title());
                }
                Err(Stop::Cancelled) => entry.state = EntryState::Paused,
                Err(Stop::Limit) => {
                    entry.state = EntryState::Paused;
                    entry.error = Some("Stopped by the byte limit of the test switch".into());
                }
                Err(Stop::Failed(why) | Stop::Refused(why)) => {
                    log::warn!("download failed: {}: {why}", entry.title());
                    entry.state = EntryState::Failed;
                    entry.error = Some(why);
                }
            }
        }
        // Removed while it ran. The worker is out of the folder now: the
        // files go, off the lock, and the tombstone with them. An entry
        // added again meanwhile starts after that, in a clean folder.
        None => {
            if s.tombstones.iter().any(|t| t.generation == job.generation) {
                drop(s);
                let _ = fs::remove_dir_all(&job.folder);
                s = inner.lock();
                s.clear_tombstone(job.generation);
            }
        }
    }
    save_index(&s);
    inner.bump();
    inner.wake.notify_all();
}

/// Agent for the files: no limit on the time a body may take, a limit on
/// the time without a byte (`stall`), and the name of this app as the user
/// agent. A cancel ends a wait for bytes within a second.
fn agent(stall: Duration, cancel: Arc<AtomicBool>) -> ureq::Agent {
    super::stall::agent(
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(6 * 3600)))
            .user_agent(format!("{}/{}", crate::brand::FOLDER, crate::config::APP_VERSION))
            .build(),
        stall,
        cancel,
    )
}

fn download(inner: &Arc<Inner>, job: &Job) -> Result<(), Stop> {
    let Job { client, folder, cancel, options, server_id, item_id, .. } = job;
    let failed = |err: anyhow::Error| Stop::Failed(format!("{err:#}"));
    // A job whose entry is gone writes nothing, not even its folder.
    job.check(inner)?;
    fs::create_dir_all(folder).map_err(|e| failed(e.into()))?;
    let agent = agent(options.stall, cancel.clone());
    let live = || job.live(inner);

    // The item as the server has it. A folder from a run before keeps it.
    let meta_path = folder.join(META_FILE);
    let mut meta: Value = fs::read(&meta_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    if meta.get("Item").is_none() {
        meta = fetch_meta(client, item_id, server_id).map_err(failed)?;
        job.check(inner)?;
        write_json(&meta_path, &meta).map_err(failed)?;
    }
    let item = meta["Item"].clone();
    let source = item["MediaSources"].get(0).cloned().unwrap_or(Value::Null);
    let total = source["Size"].as_u64().filter(|n| *n > 0);
    let file = meta["File"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| media_file_name(&source));
    check_file_name(&file).map_err(failed)?;
    {
        let mut s = inner.lock();
        let Some(entry) = s.owned_mut(job) else {
            return Err(Stop::Cancelled);
        };
        entry.fill_from(&item);
        entry.total = total;
        entry.file = Some(file.clone());
        // A restart finds the part file by its name in the index.
        save_index(&s);
        inner.bump();
    }
    job.check(inner)?;

    // Artwork and subtitles are small and optional: the item plays
    // without them.
    fetch_images(&agent, client, &item, folder, &live);
    if meta.get("Subtitles").is_none() {
        let subtitles = fetch_subtitles(&agent, client, item_id, &source, folder, &live);
        meta["Subtitles"] = Value::Array(subtitles);
        if live() {
            let _ = write_json(&meta_path, &meta);
        }
    }
    job.check(inner)?;

    // The media file.
    let final_path = folder.join(&file);
    let part = folder.join(format!("{file}.part"));
    if let (Ok(meta), Some(total)) = (fs::metadata(&final_path), total)
        && meta.len() == total
    {
        set_done(inner, job, total, 0.);
        return Ok(());
    }
    let part_len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    check_space(inner, job, total, part_len)?;

    let url = client.url(&format!("/Items/{item_id}/Download"), &[]);
    let mut attempt = 0;
    let mut wait = options.retry_wait;
    loop {
        job.check(inner)?;
        let start = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        set_done(inner, job, start, 0.);
        match fetch_media(&agent, job, &url, &part, start, inner, total.is_some()) {
            Ok(got) => {
                let want = total.unwrap_or(got);
                if got != want {
                    // The file is not what the server said; a retry gets
                    // the rest, or finds that there is none.
                    attempt += 1;
                    if attempt >= ATTEMPTS {
                        return Err(Stop::Failed(format!(
                            "The file is {} of {} bytes",
                            format_bytes(got),
                            format_bytes(want)
                        )));
                    }
                } else {
                    job.check(inner)?;
                    fs::rename(&part, &final_path).map_err(|e| failed(e.into()))?;
                    set_done(inner, job, got, 0.);
                    return Ok(());
                }
            }
            Err(Stop::Failed(why)) => {
                attempt += 1;
                log::info!("download of {item_id}: try {attempt} failed: {why}");
                if attempt >= ATTEMPTS {
                    return Err(Stop::Failed(why));
                }
            }
            Err(stop) => return Err(stop),
        }
        // Back off, and go at once when the user cancels meanwhile.
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if cancel.load(Ordering::Acquire) {
                return Err(Stop::Cancelled);
            }
            thread::sleep(Duration::from_millis(20));
        }
        wait = (wait * 2).min(Duration::from_secs(30));
    }
}

/// Refuses a download that would leave the disk under its reserve, or that
/// would go over the storage limit. The check and the admission are one
/// step under the lock: two downloads at once see each other's size. A
/// download whose size the server did not say is not admitted here; its
/// bytes are checked as they come (`check_budget`).
fn check_space(inner: &Arc<Inner>, job: &Job, total: Option<u64>, part_len: u64) -> Result<(), Stop> {
    let Some(total) = total else { return Ok(()) };
    let options = &job.options;
    let need = total.saturating_sub(part_len);
    let mut s = inner.lock();
    let free = options.free_override.or_else(|| free_space(&s.dir));
    let used = s.used_except(&job.item_id) + part_len;
    let promised = s.promised_except(&job.item_id);
    if let Some(free) = free
        && free < need + promised + MIN_FREE_BYTES
    {
        return Err(Stop::Refused(format!(
            "Not enough free space: {} needed, {} free, and {} must stay free",
            format_bytes(need + promised),
            format_bytes(free),
            format_bytes(MIN_FREE_BYTES)
        )));
    }
    if let Some(limit) = options.limit_bytes
        && used + need > limit
    {
        return Err(Stop::Refused(format!(
            "Over the storage limit of {} ({} in use)",
            format_bytes(limit),
            format_bytes(used)
        )));
    }
    if let Some(entry) = s.owned_mut(job) {
        entry.admitted = true;
    }
    Ok(())
}

/// The limit and the reserve for a download of unknown size, checked as
/// its bytes come, before `more` bytes go to the part file of `done`
/// bytes: it stops with a clear word when they would cross one of them.
fn check_budget(inner: &Arc<Inner>, job: &Job, done: u64, more: u64) -> Result<(), Stop> {
    let options = &job.options;
    let s = inner.lock();
    if let Some(limit) = options.limit_bytes {
        let used = s.used_except(&job.item_id);
        if used + done + more > limit {
            return Err(Stop::Refused(format!(
                "Over the storage limit of {} ({} in use)",
                format_bytes(limit),
                format_bytes(used)
            )));
        }
    }
    let free = options.free_override.or_else(|| free_space(&s.dir));
    let promised = s.promised_except(&job.item_id);
    if let Some(free) = free
        && free < more + promised + MIN_FREE_BYTES
    {
        return Err(Stop::Refused(format!(
            "Not enough free space: {} free, and {} must stay free",
            format_bytes(free),
            format_bytes(promised + MIN_FREE_BYTES)
        )));
    }
    Ok(())
}

fn set_done(inner: &Arc<Inner>, job: &Job, done: u64, speed: f64) {
    let mut s = inner.lock();
    if let Some(entry) = s.owned_mut(job) {
        entry.done = done;
        entry.speed = speed;
    }
    inner.bump();
}

/// Gets the media file from `start` on, into the part file. Returns the
/// size of the part file at the end of the body. `known_size` says the
/// download was admitted on its size; without one, the budget is checked
/// as the bytes come.
fn fetch_media(
    agent: &ureq::Agent,
    job: &Job,
    url: &str,
    part: &Path,
    start: u64,
    inner: &Arc<Inner>,
    known_size: bool,
) -> Result<u64, Stop> {
    let Job { client, cancel, options, item_id, .. } = job;
    let failed = |err: String| Stop::Failed(err);
    let mut request = agent.get(url).header("Authorization", client.auth_header());
    if start > 0 {
        request = request.header("Range", format!("bytes={start}-"));
    }
    let mut response = match request.call() {
        Ok(response) => response,
        // The part is longer than the file: it is not the file any more.
        Err(ureq::Error::StatusCode(416)) => {
            let _ = fs::remove_file(part);
            return Err(failed("The server has less than the part file (416)".into()));
        }
        Err(ureq::Error::StatusCode(code)) => return Err(failed(format!("HTTP {code}"))),
        Err(err) => return Err(failed(err.to_string())),
    };
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let mut start = start;
    log::info!(
        "download of {item_id}: from byte {start}: HTTP {}{}",
        response.status().as_u16(),
        header("content-range").map(|r| format!(" ({r})")).unwrap_or_default()
    );
    let mut file = match response.status().as_u16() {
        206 => {
            // "bytes <start>-<end>/<total>"
            let range = header("content-range").unwrap_or_default();
            let from: Option<u64> = range
                .trim_start_matches("bytes ")
                .split('-')
                .next()
                .and_then(|s| s.trim().parse().ok());
            if from != Some(start) {
                return Err(failed(format!("The server sent a wrong range: {range:?}")));
            }
            fs::File::options().append(true).create(true).open(part)
        }
        200 => {
            // The server ignored the range: the whole file comes again.
            if start > 0 {
                log::info!("download of {item_id}: the server ignored the range; starting over");
            }
            start = 0;
            fs::File::create(part)
        }
        code => return Err(failed(format!("HTTP {code}"))),
    }
    .map_err(|e| failed(e.to_string()))?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut done = start;
    let mut this_run = 0u64;
    let (mut mark_at, mut mark_done) = (Instant::now(), done);
    let mut saved_at = Instant::now();
    let mut speed = 0.;
    loop {
        // With the test switch on, one read never goes past its limit.
        let room = options
            .byte_limit
            .map_or(buffer.len(), |limit| (limit.saturating_sub(this_run) as usize).clamp(1, buffer.len()));
        let n = match reader.read(&mut buffer[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                let _ = file.flush();
                // A cancel ends the wait with an error as well.
                if cancel.load(Ordering::Acquire) {
                    return Err(Stop::Cancelled);
                }
                return Err(failed(format!("Read failed: {err}")));
            }
        };
        // A download of unknown size was not admitted on its size: each
        // read is checked before it is written.
        if !known_size
            && let Err(stop) = check_budget(inner, job, done, n as u64)
        {
            let _ = file.flush();
            return Err(stop);
        }
        file.write_all(&buffer[..n])
            .map_err(|e| failed(format!("Write failed: {e}")))?;
        done += n as u64;
        this_run += n as u64;
        // The speed over the last half second.
        if mark_at.elapsed() >= Duration::from_millis(500) {
            speed = (done - mark_done) as f64 / mark_at.elapsed().as_secs_f64();
            mark_at = Instant::now();
            mark_done = done;
        }
        set_done(inner, job, done, speed);
        if saved_at.elapsed() >= Duration::from_secs(2) {
            saved_at = Instant::now();
            save_index(&inner.lock());
        }
        if cancel.load(Ordering::Acquire) {
            let _ = file.flush();
            return Err(Stop::Cancelled);
        }
        if options.byte_limit.is_some_and(|limit| this_run >= limit) {
            let _ = file.flush();
            return Err(Stop::Limit);
        }
        // A test switch: slow the download to this many bytes a second, so
        // the progress can be watched on a fast link.
        if let Some(bps) = options.throttle_bps.filter(|b| *b > 0) {
            thread::sleep(Duration::from_secs_f64(n as f64 / bps as f64));
        }
    }
    file.flush().map_err(|e| failed(e.to_string()))?;
    Ok(done)
}

/// The item, its segments and the names of its files, for `meta.json`.
fn fetch_meta(client: &Client, item_id: &str, server_id: &str) -> Result<Value> {
    let user = client.user()?.to_string();
    let item: Value = client.get(&format!("/Items/{item_id}"), &[("userId", user)])?;
    // Intro and credits ranges; optional.
    let segments = client
        .get::<Value>(&format!("/MediaSegments/{item_id}"), &[])
        .ok()
        .and_then(|page| page.get("Items").cloned())
        .unwrap_or(Value::Array(Vec::new()));
    let source = item["MediaSources"].get(0).cloned().unwrap_or(Value::Null);
    Ok(serde_json::json!({
        "ServerId": server_id,
        "ItemId": item_id,
        "File": media_file_name(&source),
        "Item": item,
        "Segments": segments,
    }))
}

/// Jellyfin ids are hex GUIDs, with or without dashes: letters, digits, `-`.
fn check_id(what: &str, id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(anyhow!("the {what} {id:?} is not safe for a folder name"));
    }
    Ok(())
}

/// One path component: no `/`, no `..`, no NUL, not empty.
pub fn check_file_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(anyhow!("the file name {name:?} is not a single path component"));
    }
    Ok(())
}

/// "video.mkv": the container of the file, else the extension of its path.
fn media_file_name(source: &Value) -> String {
    let container = source["Container"]
        .as_str()
        .and_then(|c| c.split(',').next())
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .map(str::to_lowercase);
    let extension = container.or_else(|| {
        source["Path"]
            .as_str()
            .and_then(|p| p.rsplit_once('.'))
            .map(|(_, ext)| ext.to_lowercase())
            .filter(|ext| !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
    });
    format!("video.{}", extension.unwrap_or_else(|| "mkv".to_string()))
}

/// Poster and backdrop of the item, to its folder and to the image cache of
/// the app, so the pages show them without the server. `live` is asked
/// before each write: a job whose entry is gone writes nothing.
fn fetch_images(agent: &ureq::Agent, client: &Client, item: &Value, folder: &Path, live: &dyn Fn() -> bool) {
    let id = item["Id"].as_str().unwrap_or_default();
    let text = |value: &Value| value.as_str().map(str::to_string);
    let poster = text(&item["ImageTags"]["Primary"])
        .map(|tag| (id.to_string(), "Primary", tag))
        .or_else(|| {
            Some((
                text(&item["SeriesId"])?,
                "Primary",
                text(&item["SeriesPrimaryImageTag"])?,
            ))
        });
    let backdrop = item["BackdropImageTags"]
        .get(0)
        .and_then(text)
        .map(|tag| (id.to_string(), "Backdrop", tag))
        .or_else(|| {
            Some((
                text(&item["ParentBackdropItemId"])?,
                "Backdrop",
                item["ParentBackdropImageTags"].get(0).and_then(text)?,
            ))
        });
    let series = text(&item["SeriesId"])
        .zip(text(&item["SeriesPrimaryImageTag"]))
        .map(|(series, tag)| (series, "Primary", tag));
    let wanted = [
        (poster, POSTER_FILE, 640, &[240, 320, 400, 480, 640, 800][..]),
        (backdrop, BACKDROP_FILE, 1280, &[1000, 1280, 1600, 1920][..]),
        (series, SERIES_FILE, 640, &[240, 320, 400, 480, 640, 800][..]),
    ];
    for (image, name, width, seed_widths) in wanted {
        let Some((owner, kind, tag)) = image else { continue };
        let path = folder.join(name);
        if !path.is_file() {
            let url = client.image_url(&owner, kind, Some(&tag), width);
            // An answer that is no image (the page of a proxy) is not kept.
            let Ok(bytes) = get_bytes(agent, &url, None) else { continue };
            if !crate::images::looks_like_image(&bytes) || !live() {
                continue;
            }
            if fs::write(&path, &bytes).is_err() {
                continue;
            }
        }
        // The cache of the app gets the same image under each width the
        // pages ask for: one file on the disk, cloned (APFS), not copied.
        for seed_width in seed_widths {
            let url = client.image_url(&owner, kind, Some(&tag), *seed_width);
            crate::images::seed_file(&url, &path);
        }
    }
}

/// External subtitle files of the item. Returns what `meta.json` keeps of
/// them: the stream index, language, title and file name. `live` is asked
/// before each write.
fn fetch_subtitles(
    agent: &ureq::Agent,
    client: &Client,
    item_id: &str,
    source: &Value,
    folder: &Path,
    live: &dyn Fn() -> bool,
) -> Vec<Value> {
    let source_id = source["Id"].as_str().unwrap_or(item_id);
    let mut out = Vec::new();
    for stream in source["MediaStreams"].as_array().into_iter().flatten() {
        if stream["Type"].as_str() != Some("Subtitle") || stream["IsExternal"].as_bool() != Some(true) {
            continue;
        }
        let Some(index) = stream["Index"].as_i64() else { continue };
        let codec = stream["Codec"].as_str().unwrap_or_default().to_lowercase();
        let extension = match codec.as_str() {
            "subrip" | "srt" => "srt",
            "ass" => "ass",
            "ssa" => "ssa",
            "webvtt" | "vtt" => "vtt",
            // Image subtitles are not a file mpv takes on the side.
            _ => continue,
        };
        let path = stream["DeliveryUrl"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("/Videos/{item_id}/{source_id}/Subtitles/{index}/0/Stream.{extension}"));
        let Some(url) = resolve_url(&client.base, &path) else {
            log::info!("subtitle {index} of {item_id}: the address {path:?} is not one this app fetches");
            continue;
        };
        // The session goes to the own server only, whatever address the
        // server gave for the file.
        let auth = crate::jellyfin::same_origin(&url, &client.base).then_some(client);
        let name = format!("sub-{index}.{extension}");
        let file = folder.join(&name);
        if !file.exists() {
            // A route that answered with the web page gave no subtitle.
            let Ok(bytes) = get_bytes(agent, &url, auth)
                .and_then(|bytes| match bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<html") {
                    true => Err(anyhow!("not a subtitle file")),
                    false => Ok(bytes),
                })
            else {
                log::info!("subtitle {index} of {item_id} not fetched");
                continue;
            };
            if !live() || fs::write(&file, &bytes).is_err() {
                continue;
            }
        }
        out.push(serde_json::json!({
            "Index": index,
            "Language": stream["Language"],
            "Title": stream["DisplayTitle"],
            "File": name,
        }));
    }
    out
}

/// The address of a file the server named, as a URL: a path is on the
/// server, `//host/path` takes the server's scheme, an `http(s)` URL stands
/// as it is (also on another host, or plain `http` from an `https` server:
/// the session header does not go there, see `same_origin`). `None` for a
/// scheme this app does not fetch (`file:`, `ftp:`, `javascript:`).
fn resolve_url(base: &str, given: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    if let Some((scheme, _)) = given.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => Some(given.to_string()),
            _ => None,
        };
    }
    if let Some(rest) = given.strip_prefix("//") {
        let scheme = base.split_once("://")?.0;
        return Some(format!("{scheme}://{rest}"));
    }
    if given.starts_with('/') {
        return Some(format!("{base}{given}"));
    }
    // "mailto:x", "javascript:..." and the like: a scheme, not a path.
    let head = given.split(['/', '?', '#']).next().unwrap_or(given);
    if head.contains(':') {
        return None;
    }
    Some(format!("{base}/{given}"))
}

/// GET of a small file. `auth` sends the session header with it.
fn get_bytes(agent: &ureq::Agent, url: &str, auth: Option<&Client>) -> Result<Vec<u8>> {
    let mut request = agent.get(url);
    if let Some(client) = auth {
        request = request.header("Authorization", client.auth_header());
    }
    let mut response = request.call().context("get")?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .context("read")?;
    Ok(bytes)
}

// ----- index on disk -------------------------------------------------------------

fn load_index(dir: &Path) -> Vec<Entry> {
    let entries: Vec<Entry> = fs::read(dir.join(INDEX_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    // An entry with an unsafe id or file name is refused, not repaired. Its
    // folder is left alone.
    entries
        .into_iter()
        .filter(|entry| match entry.validate() {
            Ok(()) => true,
            Err(err) => {
                log::warn!("download entry refused: {err:#}");
                false
            }
        })
        .collect()
}

/// Writes the index; the file is complete or not there, never half. An
/// instance that does not hold the folder writes nothing.
fn save_index(state: &State) {
    if state.owner.is_none() {
        return;
    }
    let path = state.dir.join(INDEX_FILE);
    if let Err(err) = write_json(&path, &state.entries) {
        log::warn!("downloads index not saved: {err:#}");
    }
}

/// Takes the lock of the folder, so one process downloads into it at a
/// time. The lock is the open file: it goes when the file is dropped, and
/// with the process. `None` when another process holds it.
fn lock_folder(dir: &Path) -> Option<fs::File> {
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        unsafe extern "C" {
            fn flock(fd: std::ffi::c_int, operation: std::ffi::c_int) -> std::ffi::c_int;
        }
        const LOCK_EX: std::ffi::c_int = 2;
        const LOCK_NB: std::ffi::c_int = 4;
        // SAFETY: `fd` is the descriptor of `file`, open for the whole
        // call; flock reads and writes no memory of ours.
        let code = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if code != 0 {
            return None;
        }
    }
    Some(file)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let partial = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&partial, bytes).with_context(|| format!("write {}", partial.display()))?;
    fs::rename(&partial, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "1.2 GB", "348 MB", "12 KB": decimal units, as the Finder shows them.
pub fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

// ----- free space -------------------------------------------------------------------

/// Bytes a user may still write on the disk that holds `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        // The layout of `struct statvfs` on Darwin: the block counts are
        // 32 bits there (`__darwin_fsblkcnt_t` is an unsigned int).
        #[cfg(target_os = "macos")]
        #[repr(C)]
        struct Statvfs {
            f_bsize: std::ffi::c_ulong,
            f_frsize: std::ffi::c_ulong,
            f_blocks: u32,
            f_bfree: u32,
            f_bavail: u32,
            f_files: u32,
            f_ffree: u32,
            f_favail: u32,
            f_fsid: std::ffi::c_ulong,
            f_flag: std::ffi::c_ulong,
            f_namemax: std::ffi::c_ulong,
        }
        #[cfg(not(target_os = "macos"))]
        #[repr(C)]
        struct Statvfs {
            f_bsize: std::ffi::c_ulong,
            f_frsize: std::ffi::c_ulong,
            f_blocks: u64,
            f_bfree: u64,
            f_bavail: u64,
            f_files: u64,
            f_ffree: u64,
            f_favail: u64,
            f_fsid: std::ffi::c_ulong,
            f_flag: std::ffi::c_ulong,
            f_namemax: std::ffi::c_ulong,
            f_spare: [std::ffi::c_int; 6],
        }
        unsafe extern "C" {
            fn statvfs(path: *const std::ffi::c_char, buf: *mut Statvfs) -> std::ffi::c_int;
        }
        // The folder may not exist yet; its nearest parent is on the same disk.
        let mut probe = path.to_path_buf();
        while !probe.exists() {
            probe = probe.parent()?.to_path_buf();
        }
        let c_path = std::ffi::CString::new(probe.as_os_str().as_bytes()).ok()?;
        let mut buf = std::mem::MaybeUninit::<Statvfs>::uninit();
        let code = unsafe { statvfs(c_path.as_ptr(), buf.as_mut_ptr()) };
        if code != 0 {
            return None;
        }
        let buf = unsafe { buf.assume_init() };
        Some(buf.f_bavail as u64 * buf.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    //! The engine against a server that plays a script: a local HTTP server
    //! that serves one file, honours or ignores `Range`, cuts the connection
    //! after some bytes, or announces a size it does not have.

    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Mutex,
    };

    use super::*;

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
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        let head = loop {
            let Ok(n) = stream.read(&mut buffer) else { return };
            if n == 0 {
                return;
            }
            data.extend_from_slice(&buffer[..n]);
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break String::from_utf8_lossy(&data[..end]).to_string();
            }
        };
        let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let path = target.split('?').next().unwrap_or("/").to_string();
        let header = |name: &str| {
            head.lines()
                .find_map(|line| line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)))
                .map(|(_, v)| v.trim().to_string())
        };
        let reply = |stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]| {
            let _ = write!(stream, "HTTP/1.1 {status}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n", body.len());
            let _ = stream.write_all(body);
        };
        if path.ends_with("/Download") {
            let range = header("range");
            ranges.lock().unwrap().push(range.clone());
            let start: usize = range
                .as_deref()
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
                    let _ = write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Range: bytes {start}-{}/{claimed}\r\nContent-Length: {}\r\n\r\n",
                        claimed - 1,
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
                            "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Length: {}\r\n\r\n",
                            file.len() - 1,
                            file.len(),
                            body.len()
                        );
                    } else {
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
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
                _ => file.len(),
            };
            let body = serde_json::json!({
                "Id": id, "Name": "Test episode", "Type": "Episode",
                "SeriesName": "Test show", "SeriesId": "series1",
                "ParentIndexNumber": 1, "IndexNumber": 2,
                "MediaSources": [{ "Id": id, "Size": size, "Container": "bin", "MediaStreams": [] }],
                "ImageTags": {}
            });
            reply(&mut stream, "200 OK", "Content-Type: application/json\r\n", body.to_string().as_bytes());
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
            let entry = engine.entry(id).expect("entry");
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
        assert_eq!(engine.local_path_of(Some(&entry.server_id), "ep1"), Some(path.clone()));
        assert_eq!(engine.local_path_of(Some("another-server"), "ep1"), None);
        assert_eq!(fs::read(&path).unwrap(), *mock.file);
        assert!(!path.with_extension("bin.part").exists());
        let meta = engine.meta("ep1").unwrap();
        assert_eq!(meta["Item"]["Name"], "Test episode");
        assert_eq!(meta["File"], "video.bin");
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

    #[test]
    fn starts_over_when_the_server_ignores_range() {
        let mock = Mock::start(50_000);
        let dir = temp_dir("norange");
        // A part file from an earlier run, with other bytes in it.
        let folder = dir.join("server1").join("ep3");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("video.bin.part"), vec![9u8; 20_000]).unwrap();
        mock.set(Mode::IgnoreRange);
        let engine = engine(&mock, dir.clone(), options());
        engine.add(&item("ep3")).unwrap();
        wait_state(&engine, "ep3", EntryState::Done);
        assert_eq!(mock.ranges()[0].as_deref(), Some("bytes=20000-"));
        assert_eq!(fs::read(engine.local_path("ep3").unwrap()).unwrap(), *mock.file);
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
            let entries: Vec<Entry> = ids.iter().map(|id| engine.entry(id).expect("entry")).collect();
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
            let mut data = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..n]);
                if data.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            *kept.lock().unwrap() = Some(String::from_utf8_lossy(&data).to_string());
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
}
