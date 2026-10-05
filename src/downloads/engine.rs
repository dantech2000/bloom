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

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
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
    #[serde(skip)]
    cancel: Arc<AtomicBool>,
}

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

    fn from_item(item: &Item, server_id: &str) -> Self {
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
        }
    }
}

struct State {
    dir: PathBuf,
    client: Option<Client>,
    server_id: String,
    options: Options,
    entries: Vec<Entry>,
    running: usize,
}

impl State {
    fn entry_mut(&mut self, item_id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.item_id == item_id)
    }

    fn entry(&self, item_id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.item_id == item_id)
    }

    fn folder(&self, entry: &Entry) -> PathBuf {
        self.dir.join(&entry.server_id).join(&entry.item_id)
    }

    /// Bytes on the disk for all entries but one.
    fn used_except(&self, item_id: &str) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.item_id != item_id)
            .map(|e| e.done)
            .sum()
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
        let mut entries = load_index(&dir);
        for entry in &mut entries {
            // What the run before left unfinished goes on: a download that
            // was under way, and one that gave up (the cause may be gone).
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
        }
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                dir,
                client: None,
                server_id: String::new(),
                options,
                entries,
                running: 0,
            }),
            wake: Condvar::new(),
            version: AtomicU64::new(1),
        });
        let scheduler = inner.clone();
        let _ = thread::Builder::new()
            .name("downloads".into())
            .spawn(move || schedule(scheduler));
        Self { inner }
    }

    /// The session the downloads use. Entries of another server wait.
    pub fn set_client(&self, client: Option<Client>, server_id: &str) {
        let mut s = self.inner.state.lock().unwrap();
        // The server answers again: the downloads that gave up try again.
        if client.is_some() {
            for entry in s.entries.iter_mut().filter(|e| e.server_id == server_id) {
                if entry.state == EntryState::Failed {
                    entry.state = EntryState::Queued;
                    entry.error = None;
                    entry.cancel.store(false, Ordering::Release);
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
        let mut s = self.inner.state.lock().unwrap();
        change(&mut s.options);
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    pub fn dir(&self) -> PathBuf {
        self.inner.state.lock().unwrap().dir.clone()
    }

    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Acquire)
    }

    /// Puts an item in the queue. One that is paused or failed starts again
    /// from its part file; one that is downloaded or under way stays as it is.
    /// An item whose ids are not safe for a path is refused with an error.
    pub fn add(&self, item: &Item) -> Result<()> {
        let mut s = self.inner.state.lock().unwrap();
        let server_id = s.server_id.clone();
        if s.entry(&item.id).is_none() {
            Entry::from_item(item, &server_id).validate()?;
        }
        match s.entry_mut(&item.id) {
            Some(entry) => {
                if matches!(entry.state, EntryState::Paused | EntryState::Failed) {
                    entry.state = EntryState::Queued;
                    entry.error = None;
                    entry.cancel.store(false, Ordering::Release);
                }
            }
            None => s.entries.push(Entry::from_item(item, &server_id)),
        }
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
        Ok(())
    }

    /// Stops a download; its part file stays for a resume.
    pub fn cancel(&self, item_id: &str) {
        let mut s = self.inner.state.lock().unwrap();
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
        let mut s = self.inner.state.lock().unwrap();
        if let Some(entry) = s.entry_mut(item_id)
            && matches!(entry.state, EntryState::Paused | EntryState::Failed)
        {
            entry.state = EntryState::Queued;
            entry.error = None;
            entry.cancel.store(false, Ordering::Release);
        }
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    /// Removes an item and its files.
    pub fn remove(&self, item_id: &str) {
        let mut s = self.inner.state.lock().unwrap();
        let Some(at) = s.entries.iter().position(|e| e.item_id == item_id) else {
            return;
        };
        let entry = s.entries.remove(at);
        entry.cancel.store(true, Ordering::Release);
        let folder = s.folder(&entry);
        let _ = fs::remove_dir_all(&folder);
        save_index(&s);
        self.inner.bump();
        self.inner.wake.notify_all();
    }

    pub fn remove_all(&self) {
        let ids: Vec<String> = self.entries().into_iter().map(|e| e.item_id).collect();
        for id in ids {
            self.remove(&id);
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.inner.state.lock().unwrap().entries.clone()
    }

    pub fn entry(&self, item_id: &str) -> Option<Entry> {
        self.inner.state.lock().unwrap().entry(item_id).cloned()
    }

    /// Bytes of all downloads on the disk.
    pub fn used_bytes(&self) -> u64 {
        self.inner.state.lock().unwrap().entries.iter().map(|e| e.done).sum()
    }

    /// Free space of the disk that holds the downloads.
    pub fn free_bytes(&self) -> Option<u64> {
        let s = self.inner.state.lock().unwrap();
        s.options.free_override.or_else(|| free_space(&s.dir))
    }

    /// The folder of an item's files.
    pub fn folder(&self, item_id: &str) -> Option<PathBuf> {
        let s = self.inner.state.lock().unwrap();
        s.entry(item_id).map(|e| s.folder(e))
    }

    /// The complete media file of an item, when it is there.
    pub fn local_path(&self, item_id: &str) -> Option<PathBuf> {
        let s = self.inner.state.lock().unwrap();
        let entry = s.entry(item_id)?;
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
        let s = self.inner.state.lock().unwrap();
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

}

// ----- scheduler and workers ---------------------------------------------------

/// Starts workers for queued entries while there is room.
fn schedule(inner: Arc<Inner>) {
    loop {
        let mut s = inner.state.lock().unwrap();
        while s.running < s.options.parallel.max(1) && s.client.is_some() {
            let server_id = s.server_id.clone();
            let next = s
                .entries
                .iter_mut()
                .find(|e| e.state == EntryState::Queued && e.server_id == server_id);
            let Some(entry) = next else { break };
            entry.state = EntryState::Downloading;
            entry.error = None;
            entry.cancel.store(false, Ordering::Release);
            let item_id = entry.item_id.clone();
            s.running += 1;
            inner.bump();
            let worker = inner.clone();
            let _ = thread::Builder::new()
                .name(format!("download-{}", &item_id[..item_id.len().min(8)]))
                .spawn(move || work(worker, item_id));
        }
        let _ = inner
            .wake
            .wait_timeout(s, Duration::from_millis(500))
            .unwrap();
    }
}

/// Why a download stopped before its end.
enum Stop {
    Cancelled,
    /// The test switch stopped it.
    Limit,
    Failed(String),
}

fn work(inner: Arc<Inner>, item_id: String) {
    let outcome = download(&inner, &item_id);
    let mut s = inner.state.lock().unwrap();
    s.running = s.running.saturating_sub(1);
    match s.entry_mut(&item_id) {
        Some(entry) => {
            entry.speed = 0.;
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
                Err(Stop::Failed(why)) => {
                    log::warn!("download failed: {}: {why}", entry.title());
                    entry.state = EntryState::Failed;
                    entry.error = Some(why);
                }
            }
        }
        // Removed while it ran; its folder must not come back.
        None => {
            let folder = s.dir.join(&s.server_id).join(&item_id);
            let _ = fs::remove_dir_all(folder);
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

fn download(inner: &Arc<Inner>, item_id: &str) -> Result<(), Stop> {
    let (client, folder, cancel, options, server_id) = {
        let s = inner.state.lock().unwrap();
        let entry = s.entry(item_id).ok_or(Stop::Cancelled)?;
        (
            s.client.clone().ok_or(Stop::Cancelled)?,
            s.folder(entry),
            entry.cancel.clone(),
            s.options.clone(),
            s.server_id.clone(),
        )
    };
    let failed = |err: anyhow::Error| Stop::Failed(format!("{err:#}"));
    fs::create_dir_all(&folder).map_err(|e| failed(e.into()))?;
    let agent = agent(options.stall, cancel.clone());

    // The item as the server has it. A folder from a run before keeps it.
    let meta_path = folder.join(META_FILE);
    let mut meta: Value = fs::read(&meta_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    if meta.get("Item").is_none() {
        meta = fetch_meta(&client, item_id, &server_id).map_err(failed)?;
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
        let mut s = inner.state.lock().unwrap();
        if let Some(entry) = s.entry_mut(item_id) {
            entry.fill_from(&item);
            entry.total = total;
            entry.file = Some(file.clone());
        }
        // A restart finds the part file by its name in the index.
        save_index(&s);
        inner.bump();
    }
    if cancel.load(Ordering::Acquire) {
        return Err(Stop::Cancelled);
    }

    // Artwork and subtitles are small and optional: the item plays
    // without them.
    fetch_images(&agent, &client, &item, &folder);
    if meta.get("Subtitles").is_none() {
        let subtitles = fetch_subtitles(&agent, &client, item_id, &source, &folder);
        meta["Subtitles"] = Value::Array(subtitles);
        let _ = write_json(&meta_path, &meta);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(Stop::Cancelled);
    }

    // The media file.
    let final_path = folder.join(&file);
    let part = folder.join(format!("{file}.part"));
    if let (Ok(meta), Some(total)) = (fs::metadata(&final_path), total)
        && meta.len() == total
    {
        set_done(inner, item_id, total, 0.);
        return Ok(());
    }
    let part_len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    check_space(inner, item_id, total, part_len, &options)?;

    let url = client.url(&format!("/Items/{item_id}/Download"), &[]);
    let mut attempt = 0;
    let mut wait = options.retry_wait;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(Stop::Cancelled);
        }
        let start = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        set_done(inner, item_id, start, 0.);
        match fetch_media(&agent, &client, &url, &part, start, inner, item_id, &cancel, &options) {
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
                    fs::rename(&part, &final_path).map_err(|e| failed(e.into()))?;
                    set_done(inner, item_id, got, 0.);
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
/// would go over the storage limit.
fn check_space(
    inner: &Arc<Inner>,
    item_id: &str,
    total: Option<u64>,
    part_len: u64,
    options: &Options,
) -> Result<(), Stop> {
    let Some(total) = total else { return Ok(()) };
    let need = total.saturating_sub(part_len);
    let (free, used) = {
        let s = inner.state.lock().unwrap();
        (
            options.free_override.or_else(|| free_space(&s.dir)),
            s.used_except(item_id) + part_len,
        )
    };
    if let Some(free) = free
        && free < need + MIN_FREE_BYTES
    {
        return Err(Stop::Failed(format!(
            "Not enough free space: {} needed, {} free, and {} must stay free",
            format_bytes(need),
            format_bytes(free),
            format_bytes(MIN_FREE_BYTES)
        )));
    }
    if let Some(limit) = options.limit_bytes
        && used + need > limit
    {
        return Err(Stop::Failed(format!(
            "Over the storage limit of {} ({} in use)",
            format_bytes(limit),
            format_bytes(used)
        )));
    }
    Ok(())
}

fn set_done(inner: &Arc<Inner>, item_id: &str, done: u64, speed: f64) {
    let mut s = inner.state.lock().unwrap();
    if let Some(entry) = s.entry_mut(item_id) {
        entry.done = done;
        entry.speed = speed;
    }
    inner.bump();
}

/// Gets the media file from `start` on, into the part file. Returns the
/// size of the part file at the end of the body.
#[allow(clippy::too_many_arguments)]
fn fetch_media(
    agent: &ureq::Agent,
    client: &Client,
    url: &str,
    part: &Path,
    start: u64,
    inner: &Arc<Inner>,
    item_id: &str,
    cancel: &AtomicBool,
    options: &Options,
) -> Result<u64, Stop> {
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
        set_done(inner, item_id, done, speed);
        if saved_at.elapsed() >= Duration::from_secs(2) {
            saved_at = Instant::now();
            save_index(&inner.state.lock().unwrap());
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
/// the app, so the pages show them without the server.
fn fetch_images(agent: &ureq::Agent, client: &Client, item: &Value, folder: &Path) {
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
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                let url = client.image_url(&owner, kind, Some(&tag), width);
                let Ok(bytes) = get_bytes(agent, &url, None) else { continue };
                let _ = fs::write(&path, &bytes);
                bytes
            }
        };
        for seed_width in seed_widths {
            let url = client.image_url(&owner, kind, Some(&tag), *seed_width);
            crate::images::seed(&url, &bytes);
        }
    }
}

/// External subtitle files of the item. Returns what `meta.json` keeps of
/// them: the stream index, language, title and file name.
fn fetch_subtitles(
    agent: &ureq::Agent,
    client: &Client,
    item_id: &str,
    source: &Value,
    folder: &Path,
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
        let url = if path.starts_with("http") { path } else { format!("{}{path}", client.base) };
        let name = format!("sub-{index}.{extension}");
        let file = folder.join(&name);
        if !file.exists() {
            // A route that answered with the web page gave no subtitle.
            let Ok(bytes) = get_bytes(agent, &url, Some(client))
                .and_then(|bytes| match bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<html") {
                    true => Err(anyhow!("not a subtitle file")),
                    false => Ok(bytes),
                })
            else {
                log::info!("subtitle {index} of {item_id} not fetched");
                continue;
            };
            if fs::write(&file, &bytes).is_err() {
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

/// Writes the index; the file is complete or not there, never half.
fn save_index(state: &State) {
    let path = state.dir.join(INDEX_FILE);
    if let Err(err) = write_json(&path, &state.entries) {
        log::warn!("downloads index not saved: {err:#}");
    }
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
    }

    struct Mock {
        port: u16,
        file: Arc<Vec<u8>>,
        mode: Arc<Mutex<Mode>>,
        /// Path and `Range` header of each request to the file.
        ranges: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl Mock {
        fn start(size: usize) -> Self {
            let file: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let file = Arc::new(file);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let mode = Arc::new(Mutex::new(Mode::Normal));
            let ranges = Arc::new(Mutex::new(Vec::new()));
            let (served, shared_mode, shared_ranges) = (file.clone(), mode.clone(), ranges.clone());
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (file, mode, ranges) = (served.clone(), shared_mode.clone(), shared_ranges.clone());
                    thread::spawn(move || serve(stream, file, mode, ranges));
                }
            });
            Self { port, file, mode, ranges }
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
        engine.set_client(Some(mock.client()), "server1");
        engine
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
        // A new engine on the same folder, as after a restart: the finished
        // one is still done, the one under way waits in the queue.
        let again = Engine::open(dir.clone(), options());
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
            let mut e = Entry::from_item(&item(id), "server1");
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
        let again = engine(&mock, dir.clone(), roomy);
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
        let again = Engine::open(dir.clone(), options);
        again.set_client(Some(mock.client()), "server1");
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
        assert!(!folder.exists());
        assert!(engine.entry("ep12").is_none());
        assert_eq!(engine.used_bytes(), 0);
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
