// SPDX-License-Identifier: AGPL-3.0-or-later
//! The download engine: a queue of items, worker threads that fetch the
//! files, and an index on disk that survives a restart. Nothing of GPUI is
//! in here; the UI reads a snapshot and waits for the word that it changed
//! (`Engine::changes`).
//!
//! This file has the entries, the state and the calls of the app. `job`
//! has the scheduler and one download from start to end, `fetch` what a
//! download asks of the server beside the media file, `index` the index
//! on the disk and the lock of the folder, `space` the room on the disk.
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
//!
//! The index is the list; the folders are the record. A folder with a
//! `meta.json` and no entry gets its entry back at the start, so an index
//! that was lost costs no download (`index::recover`).

use std::{
    collections::HashSet,
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

use crate::{
    fsutil,
    jellyfin::{Client, Item, ItemKind},
};

mod fetch;
mod index;
mod job;
mod space;
#[cfg(test)]
mod tests;

pub use space::free_space;
use {fetch::*, index::*, job::*, space::*};

/// Free space that must stay on the disk after a download.
pub const MIN_FREE_BYTES: u64 = 5_000_000_000;
/// Bytes read from the server in one step.
const CHUNK: usize = 256 * 1024;
/// Tries of one download before it counts as failed.
const ATTEMPTS: u32 = 6;
/// Failed tries of one download in all, with or without new bytes.
const TRIES_AT_MOST: u32 = 60;
const INDEX_FILE: &str = "index.json";
/// The lock of the folder: one process downloads at a time.
const LOCK_FILE: &str = "lock";
/// The end of the name of a folder that a removal took out of the way
/// and has not deleted yet: `<item id>.gone`. An id has no dot, so no
/// item has such a folder.
const GONE: &str = ".gone";
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
    pub kind: ItemKind,
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

    /// The entry of a folder that the index does not list, from its
    /// `meta.json`: done when the media file is there and whole, else
    /// paused at the length of its part file. `None` for a folder that
    /// has no `meta.json` with an item and a safe file name.
    fn from_folder(server_id: &str, item_id: &str, folder: &Path) -> Option<Self> {
        let meta: Value = serde_json::from_slice(&fs::read(folder.join(META_FILE)).ok()?).ok()?;
        let item = meta.get("Item")?;
        let file = meta["File"].as_str().filter(|file| check_file_name(file).is_ok())?;
        let total = size_of(&meta);
        let length = |name: &str| fs::metadata(folder.join(name)).ok().filter(|m| m.is_file()).map(|m| m.len());
        let (state, done) = match length(file) {
            Some(whole) if total.is_none_or(|total| total == whole) => (EntryState::Done, whole),
            _ => (EntryState::Paused, length(&format!("{file}.part")).unwrap_or(0)),
        };
        let mut entry = Self {
            server_id: server_id.to_string(),
            item_id: item_id.to_string(),
            name: String::new(),
            kind: ItemKind::default(),
            series_name: None,
            series_id: None,
            season: None,
            episode: None,
            year: None,
            total,
            done,
            state,
            error: None,
            file: Some(file.to_string()),
            added: unix_now(),
            speed: 0.,
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Generation::default(),
            admitted: false,
        };
        entry.fill_from(item);
        entry.validate().ok()?;
        Some(entry)
    }

    /// Back in the queue, for a worker to take.
    fn requeue(&mut self) {
        self.state = EntryState::Queued;
        self.error = None;
    }

    /// Takes the names of the item as the server has them.
    fn fill_from(&mut self, item: &Value) {
        let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
        let number = |key: &str| item.get(key).and_then(Value::as_i64).map(|n| n as i32);
        if let Some(name) = text("Name") {
            self.name = name;
        }
        if let Some(kind) = text("Type") {
            self.kind = kind.as_str().into();
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
            (Some(s), Some(e)) if self.kind == ItemKind::Episode => format!("S{s}:E{e} · {}", self.name),
            (None, Some(e)) if self.kind == ItemKind::Episode => format!("E{e} · {}", self.name),
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

/// What the engine asks of the image cache of the app. The engine has no
/// image decoder and knows no cache, so the app gives it these two; with
/// the defaults every answer counts as an image and nothing is seeded.
#[derive(Clone, Copy, Debug)]
pub struct Artwork {
    /// True when the bytes are a whole image that the app can show.
    pub is_image: fn(&[u8]) -> bool,
    /// Puts a file of this Mac in the image cache of the app, under a URL.
    pub seed: fn(&str, &Path),
}

impl Default for Artwork {
    fn default() -> Self {
        Self { is_image: |_| true, seed: |_, _| {} }
    }
}

/// Settings of an engine. The defaults are the ones of the app.
#[derive(Clone, Debug)]
pub struct Options {
    /// The image cache of the app, for the artwork of an item.
    pub artwork: Artwork,
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
            artwork: Artwork::default(),
            parallel: 1,
            limit_bytes: None,
            free_override: None,
            retry_wait: Duration::from_secs(1),
            byte_limit: crate::options::options().download_limit_bytes,
            throttle_bps: crate::options::options().download_throttle_bps,
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

/// What a removal leaves to do off the lock: the positions kept for the
/// item go, and its folder, unless a worker is in it (`running`; that
/// worker deletes the folder when it is out).
#[derive(Clone)]
struct Removed {
    server_id: String,
    item_id: String,
    generation: Generation,
    folder: PathBuf,
    running: bool,
}

/// Which entry a call means: an item of the session's server (`"id"`), or
/// an item of a server by name (`Key::of(Some("server"), "id")`). Two servers can
/// hold an item under the same id (the id comes from the path of the
/// file), and the Downloads page lists the entries of every server, so
/// its rows name theirs. With no session, as offline, `"id"` alone finds
/// the entry of any server.
#[derive(Clone, Copy, Debug)]
pub struct Key<'a> {
    server: Option<&'a str>,
    item: &'a str,
}

impl<'a> From<&'a str> for Key<'a> {
    fn from(item: &'a str) -> Self {
        Self { server: None, item }
    }
}

impl<'a> From<&'a String> for Key<'a> {
    fn from(item: &'a String) -> Self {
        Self { server: None, item }
    }
}

impl<'a> Key<'a> {
    /// The entry of a server by name; `None` is the server of the session.
    pub fn of(server: Option<&'a str>, item: &'a str) -> Self {
        Self { server, item }
    }
}

/// The downloads in a few numbers, for what draws at every frame (the
/// top bar, a button): one look at the state, and no copy of the entries.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub entries: usize,
    /// Queued or downloading.
    pub active: usize,
    pub downloading: usize,
    pub done: usize,
    /// Bytes of all downloads on the disk.
    pub used: u64,
    /// How far the active ones are, from 0 to 1, each file counting the
    /// same: one that waits (its size is not known until its worker
    /// starts) counts as not begun.
    pub progress: f32,
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
    /// Counts the copies of the entries taken for the index on the disk;
    /// see [`Inner::save`].
    revision: u64,
}

impl State {
    /// The place of an entry. Two servers can hold one item id, so the
    /// server counts: the one named, or else the server of the session
    /// (an item of a page is an item of that server). With no session, as
    /// offline, the id alone decides.
    fn at(&self, server_id: Option<&str>, item_id: &str) -> Option<usize> {
        let server = server_id.or(Some(self.server_id.as_str())).filter(|server| !server.is_empty());
        self.entries
            .iter()
            .position(|e| e.item_id == item_id && server.is_none_or(|server| e.server_id == server))
    }

    fn of(&self, key: Key) -> Option<&Entry> {
        self.at(key.server, key.item).map(|at| &self.entries[at])
    }

    #[cfg(test)]
    fn entry_mut(&mut self, item_id: &str) -> Option<&mut Entry> {
        self.at(None, item_id).map(|at| &mut self.entries[at])
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

    /// Puts an item in the queue; see [`Engine::add`]. True when the
    /// entries changed.
    fn add(&mut self, item: &Item) -> Result<bool> {
        if self.owner.is_none() {
            return Err(NotOwner.into());
        }
        match self.at(None, &item.id) {
            Some(at) => {
                let entry = &mut self.entries[at];
                let waits = matches!(entry.state, EntryState::Paused | EntryState::Failed);
                if waits {
                    entry.requeue();
                }
                Ok(waits)
            }
            None => {
                let generation = self.next_generation();
                let entry = Entry::from_item(item, &self.server_id, generation);
                entry.validate()?;
                self.entries.push(entry);
                Ok(true)
            }
        }
    }

    /// An entry that was taken out of the list: its job stops, and its
    /// folder gets a tombstone until the files are gone.
    fn retire(&mut self, entry: Entry) -> Removed {
        entry.cancel.store(true, Ordering::Release);
        self.tombstones.push(Tombstone {
            server_id: entry.server_id.clone(),
            item_id: entry.item_id.clone(),
            generation: entry.generation,
        });
        Removed {
            folder: self.folder(&entry),
            running: entry.state == EntryState::Downloading,
            generation: entry.generation,
            server_id: entry.server_id,
            item_id: entry.item_id,
        }
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

type Guard<'a> = std::sync::MutexGuard<'a, State>;

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    /// Counts every change; the UI redraws when it moved.
    version: AtomicU64,
    /// The revision of the entries that the index on the disk has.
    saved: Mutex<u64>,
    /// Holds one message while a change waits to be seen; see
    /// [`Engine::changes`].
    changes: (async_channel::Sender<()>, async_channel::Receiver<()>),
}

impl Inner {
    fn bump(&self) {
        self.version.fetch_add(1, Ordering::Release);
        let _ = self.changes.0.try_send(());
    }

    /// The state. A thread that panicked under the lock leaves it poisoned;
    /// every change under it is a few assignments that leave the state
    /// whole, so the rest of the app goes on with it.
    fn lock(&self) -> Guard<'_> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Writes the index; the file is complete or not there, never half.
    /// An instance that does not hold the folder writes nothing. The
    /// entries are copied under the lock and written off it, so a slow
    /// disk holds up no one who reads the state (the UI at each frame).
    /// Two writers at once write in the order of their copies: the one
    /// with the older copy finds the newer one on the disk and leaves it.
    fn save(&self, mut s: Guard<'_>) {
        if s.owner.is_none() {
            return;
        }
        s.revision += 1;
        let revision = s.revision;
        let path = s.dir.join(INDEX_FILE);
        let bytes = serde_json::to_vec_pretty(&s.entries);
        drop(s);
        let mut saved = self.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *saved > revision {
            return;
        }
        let written = bytes
            .map_err(anyhow::Error::from)
            .and_then(|bytes| fsutil::write_atomic(&path, &bytes).with_context(|| format!("write {}", path.display())));
        match written {
            Ok(()) => *saved = revision,
            Err(err) => log::warn!("downloads index not saved: {err:#}"),
        }
    }

    /// The end of a change made under the lock: the index is written when
    /// the entries changed, the UI and the scheduler hear of it.
    fn commit(&self, s: Guard<'_>, changed: bool) {
        match changed {
            true => self.save(s),
            false => drop(s),
        }
        self.bump();
        self.wake.notify_all();
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
        // One try: the app opens the engine on its UI thread. When the
        // instance before this one is still on its way out (a restart),
        // the scheduler takes the folder within its next rounds.
        let owner = lock_folder(&dir);
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
                revision: 0,
            }),
            wake: Condvar::new(),
            version: AtomicU64::new(1),
            saved: Mutex::new(0),
            changes: async_channel::bounded(1),
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
        // A session opens on the UI thread, at the start before the first
        // frame: no write of an index that is as it was.
        let mut changed = false;
        if client.is_some() && s.owner.is_some() {
            for entry in s.entries.iter_mut().filter(|e| e.server_id == server_id && e.state == EntryState::Failed) {
                entry.requeue();
                changed = true;
            }
        }
        s.client = client;
        s.server_id = server_id.to_string();
        self.inner.commit(s, changed);
    }

    pub fn set_options(&self, change: impl FnOnce(&mut Options)) {
        let mut s = self.inner.lock();
        change(&mut s.options);
        self.inner.commit(s, false);
    }

    pub fn dir(&self) -> PathBuf {
        self.inner.lock().dir.clone()
    }

    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Acquire)
    }

    /// Says when the state changed: one message for all the changes since
    /// the last message was taken. The UI waits here, so it sleeps while
    /// nothing downloads.
    pub fn changes(&self) -> async_channel::Receiver<()> {
        self.inner.changes.1.clone()
    }

    /// Puts an item in the queue. One that is paused or failed starts again
    /// from its part file; one that is downloaded or under way stays as it is.
    /// An item whose ids are not safe for a path is refused with an error,
    /// and so is every item while another instance holds the folder
    /// ([`NotOwner`]).
    pub fn add(&self, item: &Item) -> Result<()> {
        let mut s = self.inner.lock();
        let changed = s.add(item)?;
        self.inner.commit(s, changed);
        Ok(())
    }

    /// Puts several items in the queue, with one write of the index (the
    /// episodes of a season). Returns how many went in or started again;
    /// an item that is refused is left out, with a line in the log.
    pub fn add_many<'a>(&self, items: impl IntoIterator<Item = &'a Item>) -> Result<usize> {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return Err(NotOwner.into());
        }
        let mut added = 0;
        for item in items {
            match s.add(item) {
                Ok(changed) => added += changed as usize,
                Err(err) => log::warn!("download refused: {err:#}"),
            }
        }
        self.inner.commit(s, added > 0);
        Ok(added)
    }

    /// Stops a download; its part file stays for a resume.
    pub fn cancel<'a>(&self, key: impl Into<Key<'a>>) {
        self.cancel_many([key.into()])
    }

    /// The same for several downloads, with one write of the index.
    pub fn cancel_many<'a, K: Into<Key<'a>>>(&self, keys: impl IntoIterator<Item = K>) {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        let mut changed = false;
        for key in keys {
            let key = key.into();
            let Some(at) = s.at(key.server, key.item) else { continue };
            let entry = &mut s.entries[at];
            match entry.state {
                EntryState::Queued => {
                    entry.state = EntryState::Paused;
                    changed = true;
                }
                // The worker writes the pause when it is out.
                EntryState::Downloading => entry.cancel.store(true, Ordering::Release),
                _ => {}
            }
        }
        self.inner.commit(s, changed);
    }

    /// Queues a paused or failed download again.
    pub fn resume<'a>(&self, key: impl Into<Key<'a>>) {
        let key = key.into();
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        let mut changed = false;
        if let Some(at) = s.at(key.server, key.item)
            && matches!(s.entries[at].state, EntryState::Paused | EntryState::Failed)
        {
            s.entries[at].requeue();
            changed = true;
        }
        self.inner.commit(s, changed);
    }

    /// Removes an item and its files. The entry goes at once; the files go
    /// off the lock, on a thread of their own (unlinking a large file takes
    /// tens of milliseconds, too long for the UI), after the worker of the
    /// entry (when one runs) is out of the folder. Until then the folder
    /// has a tombstone: a download of the same item added again waits for
    /// it.
    pub fn remove<'a>(&self, key: impl Into<Key<'a>>) {
        self.remove_many([key.into()])
    }

    /// The same for several items, as one step: one write of the index
    /// and one thread for the files.
    pub fn remove_many<'a, K: Into<Key<'a>>>(&self, keys: impl IntoIterator<Item = K>) {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        let mut gone = Vec::new();
        for key in keys {
            let key = key.into();
            if let Some(at) = s.at(key.server, key.item) {
                let entry = s.entries.remove(at);
                gone.push(s.retire(entry));
            }
        }
        self.removed(s, gone);
    }

    pub fn remove_all(&self) {
        let mut s = self.inner.lock();
        if s.owner.is_none() {
            return;
        }
        let entries = std::mem::take(&mut s.entries);
        let gone: Vec<Removed> = entries.into_iter().map(|entry| s.retire(entry)).collect();
        self.removed(s, gone);
    }

    /// The rest of a removal: the index without the entries, then, on a
    /// thread, their positions and their files.
    fn removed(&self, s: Guard<'_>, gone: Vec<Removed>) {
        if gone.is_empty() {
            return;
        }
        let dir = s.dir.clone();
        self.inner.commit(s, true);
        let (inner, theirs, again) = (self.inner.clone(), dir.clone(), gone.clone());
        let spawned = thread::Builder::new()
            .name("download-remove".into())
            .spawn(move || clear_removed(&inner, &theirs, &gone));
        // No thread: here, then. Left alone, the folders would come back
        // as downloads at the next start.
        if spawned.is_err() {
            clear_removed(&self.inner, &dir, &again);
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.inner.lock().entries.clone()
    }

    pub fn entry<'a>(&self, key: impl Into<Key<'a>>) -> Option<Entry> {
        self.inner.lock().of(key.into()).cloned()
    }

    /// The state of an entry, when the item has one: what a button or a
    /// menu asks, without a copy of the entry.
    pub fn state_of<'a>(&self, key: impl Into<Key<'a>>) -> Option<EntryState> {
        self.inner.lock().of(key.into()).map(|e| e.state)
    }

    pub fn summary(&self) -> Summary {
        let s = self.inner.lock();
        let mut all = Summary { entries: s.entries.len(), ..Default::default() };
        let mut progress = 0.;
        for entry in &s.entries {
            all.used += entry.done;
            match entry.state {
                EntryState::Done => all.done += 1,
                EntryState::Queued | EntryState::Downloading => {
                    all.active += 1;
                    all.downloading += (entry.state == EntryState::Downloading) as usize;
                    progress += entry.progress().unwrap_or(0.);
                }
                EntryState::Paused | EntryState::Failed => {}
            }
        }
        if all.active > 0 {
            all.progress = progress / all.active as f32;
        }
        all
    }

    /// Bytes of all downloads on the disk.
    pub fn used_bytes(&self) -> u64 {
        self.inner.lock().entries.iter().map(|e| e.done).sum()
    }

    /// Free space of the disk that holds the downloads. The disk is asked
    /// off the lock.
    pub fn free_bytes(&self) -> Option<u64> {
        let (assumed, dir) = {
            let s = self.inner.lock();
            (s.options.free_override, s.dir.clone())
        };
        assumed.or_else(|| free_space(&dir))
    }

    /// The folder of an item's files.
    pub fn folder<'a>(&self, key: impl Into<Key<'a>>) -> Option<PathBuf> {
        let s = self.inner.lock();
        s.of(key.into()).map(|entry| s.folder(entry))
    }

    /// The complete media file of an item, when it is there. The file of
    /// one server is not the item of another: with a server named, only
    /// its own entry counts (see [`Key`]).
    pub fn local_path<'a>(&self, key: impl Into<Key<'a>>) -> Option<PathBuf> {
        let path = {
            let s = self.inner.lock();
            let entry = s.of(key.into()).filter(|e| e.state == EntryState::Done)?;
            s.folder(entry).join(entry.file.as_deref()?)
        };
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
        let path = {
            let s = self.inner.lock();
            let own = s.of(id.into());
            let name = match (kind, own) {
                ("Primary", Some(_)) => POSTER_FILE,
                ("Backdrop", Some(_)) => BACKDROP_FILE,
                ("Primary", None) => SERIES_FILE,
                ("Backdrop", None) => BACKDROP_FILE,
                _ => return None,
            };
            // The series of a download of this server: another server can
            // have a series under the same id, with other artwork.
            let entry = own.or_else(|| {
                s.entries
                    .iter()
                    .find(|e| e.series_id.as_deref() == Some(id) && (s.server_id.is_empty() || e.server_id == s.server_id))
            })?;
            s.folder(entry).join(name)
        };
        path.is_file().then_some(path)
    }

    /// The `meta.json` of an item, parsed.
    pub fn meta<'a>(&self, key: impl Into<Key<'a>>) -> Option<Value> {
        let folder = self.folder(key)?;
        let bytes = fs::read(folder.join(META_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The item of `meta.json`, as the app's `Item`.
    pub fn item<'a>(&self, key: impl Into<Key<'a>>) -> Option<Item> {
        let meta = self.meta(key)?;
        serde_json::from_value(meta.get("Item")?.clone()).ok()
    }

    /// Workers under way, and folders with a tombstone.
    #[cfg(test)]
    fn busy(&self) -> (usize, usize) {
        let s = self.inner.lock();
        (s.running, s.tombstones.len())
    }
}

/// The work of a removal that needs the disk: the resume points kept for
/// the items, then the folders no worker is in.
fn clear_removed(inner: &Inner, dir: &Path, gone: &[Removed]) {
    // The resume point kept on this Mac belongs to the file that goes.
    super::offline::forget(dir, gone.iter().map(|r| (r.server_id.as_str(), r.item_id.as_str())));
    for removed in gone.iter().filter(|r| !r.running) {
        delete_folder(inner, &removed.folder, removed.generation);
    }
}

/// Deletes the folder of a removed entry and clears its tombstone. The
/// folder first gets the name `<item id>.gone`, which is one quick step:
/// the place is free for a download of the item added again, and a
/// deletion that the end of the app cuts short is finished at the next
/// start (`recover`) instead of coming back as a download.
fn delete_folder(inner: &Inner, folder: &Path, generation: Generation) {
    let mut name = folder.file_name().unwrap_or_default().to_os_string();
    name.push(GONE);
    let gone = folder.with_file_name(name);
    // One from a removal before that did not end.
    let _ = fs::remove_dir_all(&gone);
    let moved = fs::rename(folder, &gone).is_ok();
    if !moved {
        let _ = fs::remove_dir_all(folder);
    }
    let mut s = inner.lock();
    s.clear_tombstone(generation);
    inner.commit(s, false);
    if moved {
        let _ = fs::remove_dir_all(&gone);
    }
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

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    fsutil::write_atomic(path, &bytes).with_context(|| format!("write {}", path.display()))
}

pub(super) fn unix_now() -> i64 {
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
