//! The Swift face of bloom-core for the native iOS app (spike S1 of
//! specs/ios-native/plan.md).
//!
//! The rules of spec §5.1, in their first form:
//! - Core is blocking (ureq, threads). No call blocks Swift's thread pool:
//!   each request runs on a small pool of Rust threads, and Swift awaits a
//!   one-shot channel (`run`).
//! - Events come through a subscription object with `next()` and `close()`.
//! - Swift implements `SecretStore` (the Keychain); its methods never wait
//!   for Swift's main actor.
//! - A panic on a pool thread comes back as an error; a panic on any other
//!   thread is kept for `last_panic`.
//!
//! This crate only wraps core: it changes nothing the Mac app compiles.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, mpsc},
    thread,
    time::Duration,
};

use bloom_core::{
    connection::{Verdict, Why, classify},
    jellyfin::{Client, Item, ItemQuery, Progress},
};

uniffi::setup_scaffolding!();

mod account;
mod admin;
mod admin_extra;
mod browse;
mod detail;
mod enhanced;
mod kinds;
mod link;
mod lists;
mod prefs;
mod socket;
mod still_watching;
mod subtitles;
mod syncplay;
mod titles;

pub use kinds::*;

// ----- Errors ---------------------------------------------------------------

/// What went wrong, as a code: Swift writes the sentence.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum BloomError {
    #[error("offline: {why}")]
    Offline { why: OfflineWhy },
    #[error("sign in again")]
    SignInAgain,
    #[error("the server answered with an error")]
    Answered { detail: String },
    #[error("not signed in")]
    NotSignedIn,
    #[error("internal: {message}")]
    Internal { message: String },
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum OfflineWhy {
    Dns,
    Connect,
    Timeout,
    Tls,
    Gateway { status: u16 },
}

impl std::fmt::Display for OfflineWhy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl From<anyhow::Error> for BloomError {
    fn from(err: anyhow::Error) -> Self {
        match classify(&err) {
            Verdict::Unauthorized => Self::SignInAgain,
            Verdict::Unreachable(why) => Self::Offline {
                why: match why {
                    Why::Dns => OfflineWhy::Dns,
                    Why::Connect => OfflineWhy::Connect,
                    Why::Timeout => OfflineWhy::Timeout,
                    Why::Tls => OfflineWhy::Tls,
                    Why::Gateway(status) => OfflineWhy::Gateway { status },
                },
            },
            Verdict::Answered => Self::Answered { detail: format!("{err:#}") },
        }
    }
}

// ----- The pool -------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

/// Eight threads that run the blocking calls of core. Swift's cooperative
/// pool has about one thread per core and must never block; these may.
const POOL_THREADS: usize = 8;

fn pool() -> &'static Mutex<mpsc::Sender<Job>> {
    static POOL: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for n in 0..POOL_THREADS {
            let rx = rx.clone();
            thread::Builder::new()
                .name(format!("bloom-ffi-{n}"))
                .spawn(move || {
                    // The lock is held while waiting only (a `while let`
                    // would hold it through the job too).
                    loop {
                        let job = lock(&rx).recv();
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    }
                })
                .expect("a pool thread");
        }
        Mutex::new(tx)
    })
}

/// Runs `work` on the pool and awaits its answer. A panic in `work` comes
/// back as `Internal`, and the pool thread lives on.
async fn run<T: Send + 'static>(work: impl FnOnce() -> Result<T, BloomError> + Send + 'static) -> Result<T, BloomError> {
    let (tx, rx) = async_channel::bounded(1);
    let job: Job = Box::new(move || {
        let answer = catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
            Err(BloomError::Internal { message: panic_text(&*panic) })
        });
        let _ = tx.send_blocking(answer);
    });
    lock(pool())
        .send(job)
        .map_err(|_| BloomError::Internal { message: "the pool is gone".into() })?;
    rx.recv().await.unwrap_or_else(|_| Err(BloomError::Internal { message: "the call was dropped".into() }))
}

/// A lock, also after a panic on another thread that held it: the data of
/// this crate stays whole across a panic (each write is one assignment),
/// and a poisoned lock must not take every later call down with it.
pub(crate) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A read of an `RwLock`, also after a panic (as `lock`).
pub(crate) fn read<T>(rw: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rw.read().unwrap_or_else(PoisonError::into_inner)
}

/// A write of an `RwLock`, also after a panic (as `lock`).
pub(crate) fn write<T>(rw: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rw.write().unwrap_or_else(PoisonError::into_inner)
}

/// Runs a call that answers Swift directly (no `Result`): a panic in it
/// would end the app (UniFFI turns it into a fatal error), so it answers
/// `fallback` instead and the panic goes to the log.
pub(crate) fn guarded<T>(what: &str, fallback: T, work: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        log::error!("{what}: {}", panic_text(&*panic));
        fallback
    })
}

/// A switch of a server object; off when it is absent or not a boolean.
pub(crate) fn flag(v: &serde_json::Value, key: &str) -> bool {
    v.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false)
}

/// JSON the app sent (an edited document, an original).
pub(crate) fn parse_json(text: &str) -> Result<serde_json::Value, BloomError> {
    serde_json::from_str(text).map_err(|err| BloomError::Internal { message: format!("bad JSON from the app: {err}") })
}

/// As `run`, on a thread of its own: for a call that mostly waits (a poll
/// of many seconds), so it does not hold one of the pool's eight threads.
async fn run_apart<T: Send + 'static>(name: &str, work: impl FnOnce() -> Result<T, BloomError> + Send + 'static) -> Result<T, BloomError> {
    let (tx, rx) = async_channel::bounded(1);
    thread::Builder::new()
        .name(format!("bloom-ffi-{name}"))
        .spawn(move || {
            let answer = catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
                Err(BloomError::Internal { message: panic_text(&*panic) })
            });
            let _ = tx.send_blocking(answer);
        })
        .map_err(|err| BloomError::Internal { message: format!("no thread: {err}") })?;
    rx.recv().await.unwrap_or_else(|_| Err(BloomError::Internal { message: "the call was dropped".into() }))
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}

// ----- Languages ------------------------------------------------------------

/// The user's languages, most wanted first, as iOS lists them
/// (`Locale.preferredLanguages`: "en-US", "de-DE"): every request to the
/// server sends them as its `Accept-Language` (Jellyfin 12.0 localises by
/// it). Call once at start, before the first request.
#[uniffi::export]
pub fn set_preferred_languages(tags: Vec<String>) {
    bloom_core::jellyfin::set_preferred_languages(&tags);
}

/// The `Accept-Language` the requests send, for a test.
#[uniffi::export]
pub fn accept_language() -> Option<String> {
    bloom_core::jellyfin::accept_language()
}

// ----- Panics on other threads ---------------------------------------------

static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

/// Keeps the message of a panic on a thread of core, where no call is
/// there to carry it to Swift. Call once at start.
#[uniffi::export]
pub fn install_panic_hook() {
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = format!("{info}");
        *lock(&LAST_PANIC) = Some(text);
        before(info);
    }));
}

/// The last panic on a thread of core, if any.
#[uniffi::export]
pub fn last_panic() -> Option<String> {
    lock(&LAST_PANIC).clone()
}

/// For the spike: a panic on a thread of core (not a pool call).
#[uniffi::export]
pub fn panic_on_a_core_thread() {
    let _ = thread::spawn(|| panic!("a test panic on a core thread")).join();
}

// ----- Secrets --------------------------------------------------------------

/// The Keychain, as Swift gives it. Called on Rust threads: an
/// implementation must not wait for Swift's main actor.
#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    fn get(&self, key: String) -> Option<String>;
    fn set(&self, key: String, value: String);
    /// Takes the key out of the store (a sign-out): no empty item stays.
    fn remove(&self, key: String);
}

// ----- Data -----------------------------------------------------------------

#[derive(uniffi::Record)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
}

#[derive(uniffi::Record)]
pub struct SignedIn {
    pub user_id: String,
    pub user_name: String,
}

#[derive(uniffi::Record)]
pub struct LoginBranding {
    pub disclaimer: Option<String>,
    pub splash: Option<String>,
}

#[derive(uniffi::Record)]
pub struct PublicUser {
    pub id: String,
    pub name: String,
}

/// An item as the pages show it: what a card, a row or the detail page
/// needs, with its picture addresses made by core.
#[derive(uniffi::Record, Clone)]
pub struct MediaItem {
    pub id: String,
    pub name: String,
    pub kind: MediaKind,
    pub collection_type: Option<CollectionType>,
    pub year: Option<String>,
    pub overview: Option<String>,
    pub official_rating: Option<String>,
    pub community_rating: Option<f32>,
    pub runtime_secs: Option<i64>,
    pub resume_secs: i64,
    pub progress: Option<f32>,
    pub played: bool,
    pub favorite: bool,
    pub unplayed_count: Option<i32>,
    pub series_id: Option<String>,
    pub series_name: Option<String>,
    pub season_id: Option<String>,
    /// An episode's season ("Season 2"), for the link to it.
    #[uniffi(default = None)]
    pub season_name: Option<String>,
    pub episode_code: Option<String>,
    pub title: String,
    pub playable: bool,
    pub is_series: bool,
    pub genres: Vec<String>,
    pub poster: Option<String>,
    pub wide: Option<String>,
    /// The wide picture of the show an episode is from (its thumb, else
    /// its backdrop), for Continue Watching and Next Up: a row of shows,
    /// not of clips. The item's own wide picture for anything else.
    #[uniffi(default = None)]
    pub show_wide: Option<String>,
    /// The poster of the show an episode is from, for a row of posters
    /// (Recently Added): the Mac's poster card. The item's own poster for
    /// anything else.
    #[uniffi(default = None)]
    pub show_poster: Option<String>,
    /// An episode's own still, else the wide picture: the row of a
    /// season's episode list (the Mac's `still_url`).
    #[uniffi(default = None)]
    pub still: Option<String>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
    /// The trailer link the web plugin would pick (YouTube first).
    pub trailer: Option<String>,
    /// The item's page in Jellyfin Web.
    pub web_url: Option<String>,
    /// The critics' score in percent (Rotten Tomatoes), when the server has it.
    pub critic_rating: Option<f32>,
    /// An episode's season and number.
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    /// The Jellyfin Enhanced quality tags ("4K", "Dolby Vision", "ATMOS
    /// 7.1"...), in the user's order; none while the tags are off.
    #[uniffi(default = [])]
    pub quality_tags: Vec<String>,
    /// The day it came out or airs ("25 Nov 2026"), for an episode that
    /// has not aired yet.
    #[uniffi(default = None)]
    pub premiere: Option<String>,
}

fn media(client: &Client, item: &Item) -> MediaItem {
    MediaItem {
        id: item.id.clone(),
        name: item.name.clone(),
        kind: MediaKind::from(&item.kind),
        collection_type: item.collection_type.as_deref().map(CollectionType::parse),
        year: item.year_label(),
        overview: item.overview.clone(),
        official_rating: item.official_rating.clone(),
        community_rating: item.community_rating,
        runtime_secs: item.runtime_secs(),
        resume_secs: item.resume_secs(),
        progress: item.progress(),
        played: item.user_data.played,
        favorite: item.user_data.is_favorite,
        unplayed_count: item.user_data.unplayed_item_count,
        series_id: item.series_id.clone(),
        series_name: item.series_name.clone(),
        season_id: item.season_id.clone(),
        season_name: item.season_name.clone(),
        episode_code: item.episode_code(),
        title: title_once(item),
        playable: item.is_playable(),
        is_series: item.is_series(),
        genres: item.genres.clone(),
        poster: item.poster_url(client, 400),
        wide: item.wide_url(client, 640),
        show_wide: show_wide(client, item),
        show_poster: show_poster(client, item),
        still: still(client, item),
        backdrop: item.backdrop_url(client, 1280),
        logo: item.logo_url(client, 600),
        trailer: bloom_core::trailers::best_trailer(&item.remote_trailers),
        web_url: Some(client.web_url(&item.id)),
        critic_rating: item.critic_rating,
        season_number: item.parent_index_number,
        episode_number: item.index_number,
        quality_tags: enhanced::card_tags(item),
        premiere: item.premiere_label(),
    }
}

/// The creators of a show, once each, in the server's order (the Mac's
/// "Creator" row of the detail page).
fn creators(item: &Item) -> Vec<PersonName> {
    let mut out: Vec<PersonName> = Vec::new();
    for person in item.people.iter().filter(|p| p.kind == bloom_core::jellyfin::PersonKind::Creator) {
        if !out.iter().any(|known| known.name == person.name) {
            out.push(PersonName { id: person.id.clone(), name: person.name.clone() });
        }
    }
    out
}

/// An episode's show, wide: the series thumb, a parent's thumb, the
/// series backdrop; the episode's own picture only when the show has none.
fn show_wide(client: &Client, item: &Item) -> Option<String> {
    const WIDTH: u32 = 640;
    if item.kind != bloom_core::jellyfin::ItemKind::Episode {
        return item.wide_url(client, WIDTH);
    }
    if let (Some(series), Some(tag)) = (&item.series_id, &item.series_thumb_image_tag) {
        return Some(client.image_url(series, "Thumb", Some(tag), WIDTH));
    }
    if let (Some(parent), Some(tag)) = (&item.parent_thumb_item_id, &item.parent_thumb_image_tag) {
        return Some(client.image_url(parent, "Thumb", Some(tag), WIDTH));
    }
    if let (Some(parent), Some(tag)) = (&item.parent_backdrop_item_id, item.parent_backdrop_image_tags.first()) {
        return Some(client.image_url(parent, "Backdrop", Some(tag), WIDTH));
    }
    item.wide_url(client, WIDTH)
}

/// An episode's show, as a poster: the series poster, else the item's own.
fn show_poster(client: &Client, item: &Item) -> Option<String> {
    const WIDTH: u32 = 400;
    if item.kind == bloom_core::jellyfin::ItemKind::Episode
        && let (Some(series), Some(tag)) = (&item.series_id, &item.series_primary_image_tag)
    {
        return Some(client.image_url(series, "Primary", Some(tag), WIDTH));
    }
    item.poster_url(client, WIDTH)
}

/// An episode's own still, else the item's wide picture.
fn still(client: &Client, item: &Item) -> Option<String> {
    const WIDTH: u32 = 640;
    item.image_tags
        .primary
        .as_deref()
        .filter(|_| item.kind == bloom_core::jellyfin::ItemKind::Episode)
        .map(|tag| client.image_url(&item.id, "Primary", Some(tag), WIDTH))
        .or_else(|| item.wide_url(client, WIDTH))
}

/// The title with its year, unless the name holds the year already (a
/// film named "Film (2026)" read "Film (2026) (2026)").
fn title_once(item: &Item) -> String {
    match item.production_year {
        Some(year) if item.series_name.is_none() && item.name.contains(&format!("({year})")) => item.name.clone(),
        _ => item.display_title(),
    }
}

fn medias(client: &Client, items: &[Item]) -> Vec<MediaItem> {
    items.iter().map(|item| media(client, item)).collect()
}

#[derive(uniffi::Record)]
pub struct Row {
    /// The library's id: two libraries may share a name.
    pub id: String,
    pub title: String,
    pub items: Vec<MediaItem>,
}

#[derive(uniffi::Record)]
pub struct Home {
    /// The home rows in the user's order (`homesection0..`).
    pub order: Vec<HomeSection>,
    pub hero: Vec<MediaItem>,
    pub resume: Vec<MediaItem>,
    pub next_up: Vec<MediaItem>,
    pub latest: Vec<Row>,
    /// Episodes that have not aired yet (the "upcoming" row).
    #[uniffi(default = [])]
    pub upcoming: Vec<MediaItem>,
}

#[derive(uniffi::Record)]
pub struct Page {
    pub items: Vec<MediaItem>,
    pub total: u32,
}

#[derive(uniffi::Record)]
pub struct CastMember {
    pub id: String,
    pub name: String,
    pub role: String,
    pub image: Option<String>,
}

#[derive(uniffi::Record)]
pub struct Detail {
    pub item: MediaItem,
    pub seasons: Vec<MediaItem>,
    pub episodes: Vec<MediaItem>,
    pub next_up: Option<MediaItem>,
    pub similar: Vec<MediaItem>,
    pub cast: Vec<CastMember>,
    /// The collections that hold the item ("Included in").
    #[uniffi(default = [])]
    pub collections: Vec<MediaItem>,
    /// A show's creators (people of kind Creator), for "Created by".
    #[uniffi(default = [])]
    pub creators: Vec<PersonName>,
}

/// A person's name with the id of their page.
#[derive(uniffi::Record, Clone)]
pub struct PersonName {
    pub id: String,
    pub name: String,
}

/// What the player needs to open an item. The server decides through
/// `PlaybackInfo` with mpv's device profile, as on the Mac: the file as it
/// is when it can ("Play the file as it is"), else the stream it makes (a
/// transcode, for instance over a link with a bitrate limit).
#[derive(uniffi::Record)]
pub struct PlayInfo {
    pub url: String,
    /// The header mpv sends: the token never goes in the URL.
    pub http_header: String,
    pub item_id: String,
    pub media_source_id: String,
    pub play_session_id: String,
    /// For the reports.
    pub play_method: PlayMethod,
    pub transcoding: bool,
    pub start_secs: f64,
    pub title: String,
    pub item: MediaItem,
    pub segments: Vec<Segment>,
    pub external_subtitles: Vec<ExternalSubtitle>,
    /// The episode after this one, for Up next.
    pub next: Option<MediaItem>,
    /// The quality steps under the file's own bitrate (the web client's
    /// ladder), for the Quality menu.
    pub rungs: Vec<QualityRung>,
    /// Bits per second of the file, when the server knows it.
    pub source_bitrate: Option<u64>,
    /// mpv's ids of the chosen tracks in the file (0: subtitles off); none
    /// leaves mpv's own choice (and in a transcode the server chose).
    pub audio_mpv_id: Option<i64>,
    pub subtitle_mpv_id: Option<i64>,
    /// The subtitle the user chose, by the server's index (a file of its
    /// own is fetched apart).
    pub subtitle_index: Option<i64>,
}

/// A track of an item the user can choose before playing.
#[derive(uniffi::Record)]
pub struct StreamOption {
    pub index: i64,
    pub kind: TrackKind,
    pub title: String,
    pub is_default: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct QualityRung {
    pub bitrate: u64,
    pub label: String,
    pub height: String,
}

/// The marked parts of an item; none when the server has none or fails.
fn segments(client: &Client, item_id: &str) -> Vec<Segment> {
    client
        .media_segments(item_id)
        .unwrap_or_default()
        .iter()
        .map(|s| Segment { kind: s.kind.into(), start_secs: s.start_secs(), end_secs: s.end_secs() })
        .collect()
}

fn rungs(list: &[bloom_core::bitrate::Rung]) -> Vec<QualityRung> {
    list.iter().map(|r| QualityRung { bitrate: r.bitrate, label: r.label.to_string(), height: r.height.to_string() }).collect()
}

/// The whole ladder, for the maximum bitrate in Settings.
#[uniffi::export]
pub fn quality_ladder() -> Vec<QualityRung> {
    rungs(&bloom_core::bitrate::LADDER)
}

/// "8 Mbps" for a bitrate.
#[uniffi::export]
pub fn bitrate_label(bitrate: u64) -> String {
    bloom_core::bitrate::bitrate_label(bitrate)
}

/// A part of an item the server marked: intro, credits, recap, preview.
#[derive(uniffi::Record)]
pub struct Segment {
    pub kind: SegmentKind,
    pub start_secs: f64,
    pub end_secs: f64,
}

/// A subtitle file the server serves beside the stream. The player fetches
/// it on its own task when it is chosen, never on mpv's thread (CLAUDE.md:
/// "Nothing on the mpv worker waits for the network").
#[derive(uniffi::Record)]
pub struct ExternalSubtitle {
    pub index: i64,
    pub title: String,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub url: String,
}

/// How to fetch the original file of an item for offline use (the Mac's
/// downloads take the same `/Items/{id}/Download`).
#[derive(uniffi::Record)]
pub struct DownloadInfo {
    pub url: String,
    /// The value of the `Authorization` header.
    pub authorization: String,
    pub item: MediaItem,
    /// The file's own extension ("mkv", "mp4"), for the name on disk.
    pub file_extension: String,
    pub segments: Vec<Segment>,
    /// About how big the file is (its bitrate over its length), for the
    /// room on the device; none when the server does not say.
    pub estimated_bytes: Option<u64>,
}

/// The timeline of an item (as the Mac reads it, `timeline_info`): its
/// chapters, its preview frames, and the tracks the user's settings pick,
/// counted as mpv counts them.
#[derive(uniffi::Record)]
pub struct Timeline {
    pub chapters: Vec<ChapterMark>,
    pub trickplay: Option<TrickplaySheets>,
    pub audio_track: Option<i64>,
    pub subtitle: SubtitleChoice,
}

#[derive(uniffi::Record)]
pub struct ChapterMark {
    pub name: String,
    pub start_secs: f64,
}

/// Preview frames: sheets of `columns` × `rows` frames, one every
/// `interval_ms`; `sheet_url` has `{sheet}` for the number of a sheet.
#[derive(uniffi::Record)]
pub struct TrickplaySheets {
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
    pub count: u32,
    pub interval_ms: u32,
    pub sheet_url: String,
}

#[derive(uniffi::Record)]
pub struct QuickConnectCode {
    pub code: String,
    pub secret: String,
}

/// Another Jellyfin app of this user that takes commands (the Play on
/// sheet; the Mac's `JellyfinRemote`).
#[derive(uniffi::Record)]
pub struct RemoteSession {
    pub id: String,
    pub device_name: String,
    pub client: String,
    pub now_playing: Option<String>,
    pub paused: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
}

#[derive(uniffi::Enum)]
pub enum ReportKind {
    Start,
    Progress,
    Stopped,
}

// ----- The server -----------------------------------------------------------

/// One Jellyfin server and, after `sign_in`, one session on it.
#[derive(uniffi::Object)]
pub struct Server {
    client: Mutex<Client>,
    secrets: Arc<dyn SecretStore>,
}

impl Server {
    fn client(&self) -> Result<Client, BloomError> {
        Ok(lock(&self.client).clone())
    }

    /// After a sign-in: the token goes to the secret store, and this
    /// server's calls run in the new session.
    fn open_session(&self, client: &Client, auth: &bloom_core::jellyfin::AuthResult) -> SignedIn {
        let token: &str = &auth.access_token;
        self.secrets.set(token_key(&auth.user.id), token.to_string());
        *lock(&self.client) = client.clone().with_session(token, &auth.user.id);
        SignedIn { user_id: auth.user.id.clone(), user_name: auth.user.name.clone() }
    }

    fn signed_in(&self) -> Result<Client, BloomError> {
        let client = self.client()?;
        if client.token.is_none() {
            return Err(BloomError::NotSignedIn);
        }
        Ok(client)
    }
}

/// The key of a user's token in the secret store.
fn token_key(user_id: &str) -> String {
    format!("token.{user_id}")
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[uniffi::export]
impl Server {
    #[uniffi::constructor]
    pub fn new(url: String, device_id: String, secrets: Arc<dyn SecretStore>) -> Arc<Self> {
        Arc::new(Self { client: Mutex::new(Client::new(&url, &device_id)), secrets })
    }

    pub async fn public_info(&self) -> Result<ServerInfo, BloomError> {
        let client = self.client()?;
        run(move || {
            let info = client.public_info()?;
            Ok(ServerInfo { id: info.id, name: info.server_name })
        })
        .await
    }

    /// Signs in; the token goes to the secret store, never to Swift.
    pub async fn sign_in(self: Arc<Self>, username: String, password: String) -> Result<SignedIn, BloomError> {
        let client = self.client()?;
        let me = self.clone();
        run(move || {
            let auth = client.authenticate(&username, &password)?;
            Ok(me.open_session(&client, &auth))
        })
        .await
    }

    /// A server's users that show on its sign-in page.
    pub async fn public_users(&self) -> Result<Vec<PublicUser>, BloomError> {
        let client = self.client()?;
        run(move || Ok(client.public_users()?.into_iter().map(|u| PublicUser { id: u.id, name: u.name }).collect())).await
    }

    /// What the server shows on its sign-in page: the disclaimer and the
    /// splash picture made from the library (when on). Needs no sign-in.
    pub async fn login_branding(&self) -> Result<LoginBranding, BloomError> {
        let client = self.client()?;
        run(move || {
            let branding = client.branding().unwrap_or_default();
            Ok(LoginBranding {
                disclaimer: branding.login_disclaimer.filter(|text| !text.trim().is_empty()),
                splash: branding.splashscreen_enabled.then(|| client.splashscreen_url()),
            })
        })
        .await
    }

    /// Opens the session of a profile signed in before: the token comes from
    /// the secret store. False when the store has none, or an empty one
    /// (a sign-out of an older build left it): sign in again.
    pub fn restore(&self, user_id: String) -> Result<bool, BloomError> {
        let Some(token) = self.secrets.get(token_key(&user_id)).filter(|token| !token.is_empty()) else {
            return Ok(false);
        };
        let mut client = lock(&self.client);
        *client = client.clone().with_session(&token, &user_id);
        Ok(true)
    }

    /// Ends the session on the server (best effort) and forgets its token;
    /// this server's calls then answer `NotSignedIn`.
    pub async fn sign_out(self: Arc<Self>, user_id: String) -> Result<(), BloomError> {
        let client = self.client()?;
        let me = self.clone();
        run(move || {
            let _ = client.logout();
            me.secrets.remove(token_key(&user_id));
            let mut held = lock(&me.client);
            if held.user_id.as_deref() == Some(user_id.as_str()) {
                *held = Client::new(&held.base, &held.device_id);
            }
            Ok(())
        })
        .await
    }

    pub async fn home(&self) -> Result<Home, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let (_views, home) = bloom_core::pages::load_home(&client, Vec::new(), true, true)?;
            Ok(Home {
                order: home.order.iter().map(|kind| HomeSection::parse(kind)).collect(),
                hero: medias(&client, &home.hero),
                resume: medias(&client, &home.resume),
                next_up: medias(&client, &home.next_up),
                latest: home.latest.iter().map(|(view, items)| Row { id: view.id.clone(), title: view.name.clone(), items: medias(&client, items) }).collect(),
                upcoming: medias(&client, &home.upcoming),
            })
        })
        .await
    }

    /// The libraries the app shows (video only, as on the Mac).
    pub async fn libraries(&self) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(medias(&client, &bloom_core::pages::library_views(client.views()?)))).await
    }

    /// One page of a library, by name; `favorites` for the favourite
    /// movies and shows of every library.
    /// No `sort_by` sorts by name.
    #[allow(clippy::too_many_arguments, reason = "an export: Swift calls it with labels, a struct adds a type for one call")]
    pub async fn library_page(&self, library_id: String, collection_type: Option<CollectionType>, start: u32, limit: u32, sort_by: Option<LibrarySort>, descending: bool, filter: Option<LibraryFilter>) -> Result<Page, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            // Besides a library or a folder: "favorites", "playlists", and
            // "person:<id>" for the titles of a person.
            let person = library_id.strip_prefix("person:").map(str::to_string);
            let types: Vec<String> = match collection_type {
                Some(CollectionType::Movies) => vec!["Movie".into()],
                Some(CollectionType::TvShows) => vec!["Series".into()],
                _ if library_id == "favorites" || person.is_some() => vec!["Movie".into(), "Series".into()],
                _ if library_id == "playlists" => vec!["Playlist".into()],
                _ => Vec::new(),
            };
            let favorites = library_id == "favorites";
            let no_parent = favorites || person.is_some() || library_id == "playlists";
            let query = ItemQuery {
                parent_id: (!no_parent).then(|| library_id.clone()),
                person_id: person,
                include_types: types,
                recursive: Some(true),
                sort_by: Some(sort_by.unwrap_or(LibrarySort::Name).field().into()),
                sort_order: Some(if descending { "Descending" } else { "Ascending" }.into()),
                filters: match (favorites, filter.map(LibraryFilter::field)) {
                    (true, Some(f)) => Some(format!("IsFavorite,{f}")),
                    (true, None) => Some("IsFavorite".into()),
                    (false, f) => f.map(str::to_string),
                },
                start: Some(start as usize),
                limit: Some(limit as usize),
                ..Default::default()
            };
            let page = client.items(&query)?;
            Ok(Page { items: medias(&client, &page.items), total: page.total_record_count as u32 })
        })
        .await
    }

    pub async fn detail(&self, item_id: String) -> Result<Detail, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let known = client.item(&item_id)?;
            let data = bloom_core::pages::load_detail(&client, &known)?;
            let cast = data
                .cast
                .iter()
                .map(|(person, role)| CastMember {
                    id: person.id.clone(),
                    name: person.name.clone(),
                    role: role.clone(),
                    image: person.primary_image_tag.as_deref().map(|tag| client.image_url(&person.id, "Primary", Some(tag), 240)),
                })
                .collect();
            Ok(Detail {
                item: media(&client, &data.item),
                seasons: medias(&client, &data.seasons),
                episodes: medias(&client, &data.episodes),
                next_up: data.next_up.as_ref().map(|item| media(&client, item)),
                similar: medias(&client, &data.similar),
                cast,
                collections: medias(&client, &data.collections),
                creators: creators(&data.item),
            })
        })
        .await
    }

    /// One item as a card shows it: the show or the season an episode
    /// page links to.
    pub async fn item(&self, item_id: String) -> Result<MediaItem, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(media(&client, &client.item(&item_id)?))).await
    }

    /// The episodes a show stands for in a queue (Play All on a library
    /// of shows): from the next one not watched on, or all of them when
    /// it was watched through. The Mac's `expand_series`.
    pub async fn series_queue(&self, series_id: String) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(medias(&client, &client.series_queue(&series_id)?))).await
    }

    pub async fn episodes(&self, series_id: String, season_id: String) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(medias(&client, &client.episodes(&series_id, &season_id)?))).await
    }

    pub async fn search(&self, text: String) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let (items, _seerr) = bloom_core::pages::load_search(&client, &text)?;
            Ok(medias(&client, &items))
        })
        .await
    }

    /// Takes an item out of Continue Watching or Next Up (Jellyfin Enhanced).
    pub async fn hide_from_row(&self, item_id: String, next_up: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || client.hide_from_row(next_up, &item_id).map_err(Into::into)).await
    }

    pub async fn set_played(&self, item_id: String, played: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || client.set_played(&item_id, played).map(drop).map_err(Into::into)).await
    }

    pub async fn set_favorite(&self, item_id: String, favorite: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || client.set_favorite(&item_id, favorite).map(drop).map_err(Into::into)).await
    }

    /// How to play an item, as the server decides it. `from_start` ignores
    /// the resume point; `start_secs` overrides both (a reload at a place).
    /// `max_bitrate` caps the stream (the Quality menu, the setting); none
    /// plays the file as it is when the server allows it.
    /// `audio_index` and `subtitle_index` are the server's indexes of the
    /// tracks the user chose (-1: no subtitles); none leaves it to the server.
    /// `burn_in` asks the server to draw the chosen subtitle into the
    /// video: after its file did not come (core's `Load::burn_in`).
    /// `media_source_id` is the version to play (`media_versions`); none
    /// plays the item's own file.
    #[uniffi::method(default(media_source_id = None))]
    #[allow(clippy::too_many_arguments, reason = "an export: Swift calls it with labels, a struct adds a type for one call")]
    pub async fn play_info(&self, item_id: String, from_start: bool, start_secs: Option<f64>, max_bitrate: Option<u64>,
                           audio_index: Option<i64>, subtitle_index: Option<i64>, burn_in: bool,
                           media_source_id: Option<String>) -> Result<PlayInfo, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            use bloom_core::{
                jellyfin::{StreamKind, SubtitleDelivery},
                playback::{Load, PlayMethod, PlaybackInfo, request_body, without_key},
            };
            let item = client.item(&item_id)?;
            let start = start_secs.unwrap_or(if from_start { 0. } else { item.resume_secs() as f64 });
            let load = Load {
                item_id: item.id.clone(),
                title: title_once(&item),
                start_secs: start,
                paused: false,
                token: 0,
                audio: audio_index,
                subtitle: subtitle_index,
                burn_in,
                media_source_id: media_source_id.filter(|id| !id.is_empty() && *id != item.id),
            };
            let info: PlaybackInfo = client.post_json(&format!("/Items/{}/PlaybackInfo", item.id), &request_body(&client, &load, max_bitrate))?;
            if let Some(code) = &info.error_code {
                return Err(BloomError::Answered { detail: code.clone() });
            }
            let source = info
                .source_for(&item.id, load.media_source_id.as_deref())
                .ok_or_else(|| BloomError::Answered { detail: "no media source".into() })?;
            let (url, method) = if source.supports_direct_play {
                (client.stream_url(&item.id, &source.id), PlayMethod::DirectPlay)
            } else if let Some(path) = source.transcoding_url.as_deref().filter(|_| source.supports_transcoding || source.supports_direct_stream) {
                (format!("{}{}", client.base, without_key(path)), PlayMethod::Transcode)
            } else {
                return Err(BloomError::Answered { detail: "the server offers no way to play this item".into() });
            };
            let remux = method == PlayMethod::Transcode && !source.supports_transcoding;
            let external_subtitles = source
                .media_streams
                .iter()
                .filter(|s| s.kind == StreamKind::Subtitle && s.delivery_method == Some(SubtitleDelivery::External))
                .filter_map(|s| {
                    let url = s.delivery_url.as_deref()?;
                    Some(ExternalSubtitle {
                        index: s.index,
                        title: s.display_title.clone().or_else(|| s.title.clone()).unwrap_or_else(|| format!("Subtitle {}", s.index)),
                        language: s.language.clone(),
                        codec: s.codec.clone(),
                        url: format!("{}{}", client.base, without_key(url)),
                    })
                })
                .collect();
            let segments = segments(&client, &item.id);
            let next = match &item.series_id {
                Some(series) => client.episodes_after(series, &item.id).ok().and_then(|e| e.first().map(|n| media(&client, n))),
                None => None,
            };
            let video_codec = source.media_streams.iter().find(|s| s.kind == StreamKind::Video).and_then(|s| s.codec.clone());
            let ladder = bloom_core::bitrate::ladder(source.bitrate, video_codec.as_deref());
            Ok(PlayInfo {
                rungs: rungs(&ladder),
                source_bitrate: source.bitrate,
                url,
                http_header: client.mpv_auth_header(),
                item_id: item.id.clone(),
                media_source_id: source.id.clone(),
                play_session_id: info.play_session_id.clone().unwrap_or_else(uuid_v4),
                play_method: match (method, remux) {
                    (PlayMethod::DirectPlay, _) => kinds::PlayMethod::DirectPlay,
                    (PlayMethod::Transcode, true) => kinds::PlayMethod::DirectStream,
                    (PlayMethod::Transcode, false) => kinds::PlayMethod::Transcode,
                },
                transcoding: method == PlayMethod::Transcode,
                start_secs: start,
                title: title_once(&item),
                item: media(&client, &item),
                segments,
                external_subtitles,
                next,
                // In a transcode the server made the choice; in the file
                // mpv picks by its own numbers (core's `mpv_id`).
                audio_mpv_id: audio_index
                    .filter(|_| method != PlayMethod::Transcode)
                    .and_then(|i| bloom_core::playback::mpv_id(&source.media_streams, StreamKind::Audio, i)),
                subtitle_mpv_id: subtitle_index.filter(|_| method != PlayMethod::Transcode).and_then(|i| {
                    if i < 0 { Some(0) } else { bloom_core::playback::mpv_id(&source.media_streams, StreamKind::Subtitle, i) }
                }),
                subtitle_index,
            })
        })
        .await
    }

    /// What a download of an item needs. `media_source_id` is the version
    /// to download (`media_versions`); none is the item's own file.
    #[uniffi::method(default(media_source_id = None))]
    pub async fn download_info(&self, item_id: String, media_source_id: Option<String>) -> Result<DownloadInfo, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let item = client.item(&item_id)?;
            // The version's own item serves its file (a source id is the id
            // of the item of its file).
            let source = bloom_core::versions::pick(&item.media_sources, &item.id, media_source_id.as_deref());
            let file_id = source.map_or(item.id.as_str(), |s| if s.id.is_empty() { item.id.as_str() } else { s.id.as_str() });
            let file_extension = source
                .and_then(|s| s.path.as_deref())
                .and_then(|p| std::path::Path::new(p).extension())
                .map(|e| e.to_string_lossy().to_lowercase())
                .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
                .unwrap_or_else(|| "mkv".into());
            let segments = segments(&client, &item.id);
            Ok(DownloadInfo {
                url: client.url(&format!("/Items/{file_id}/Download"), &[]),
                authorization: client.auth_header(),
                item: media(&client, &item),
                file_extension,
                segments,
                estimated_bytes: source
                    .and_then(|s| s.bitrate)
                    .zip(item.runtime_secs())
                    .map(|(bitrate, secs)| (bitrate / 8).saturating_mul(secs.max(0).unsigned_abs())),
            })
        })
        .await
    }

    pub async fn timeline(&self, item_id: String) -> Result<Timeline, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let info = client.timeline_info(&item_id)?;
            Ok(Timeline {
                chapters: info.chapters.iter().map(|c| ChapterMark { name: c.name.clone(), start_secs: c.start_secs() }).collect(),
                trickplay: info.trickplay.map(|t| TrickplaySheets {
                    width: t.width,
                    height: t.height,
                    columns: t.tile_width,
                    rows: t.tile_height,
                    count: t.thumbnail_count,
                    interval_ms: t.interval,
                    sheet_url: client.trickplay_url(&item_id, t.width, 0).replace("/0.jpg", "/{sheet}.jpg"),
                }),
                audio_track: info.default_audio,
                subtitle: info.default_subtitle.into(),
            })
        })
        .await
    }

    /// Whether the server lets a device sign in with a code.
    pub async fn quick_connect_enabled(&self) -> bool {
        let Ok(client) = self.client() else { return false };
        run(move || Ok(client.quick_connect_enabled().unwrap_or(false))).await.unwrap_or(false)
    }

    /// Starts a Quick Connect sign-in: the code to show, the secret to poll.
    pub async fn quick_connect_start(&self) -> Result<QuickConnectCode, BloomError> {
        let client = self.client()?;
        run(move || {
            let q = client.quick_connect_initiate()?;
            Ok(QuickConnectCode { code: q.code, secret: q.secret })
        })
        .await
    }

    /// True once another device approved the code.
    pub async fn quick_connect_approved(&self, secret: String) -> Result<bool, BloomError> {
        let client = self.client()?;
        run(move || Ok(client.quick_connect_state(&secret)?.authenticated)).await
    }

    /// The session of an approved code; the token goes to the secret store.
    pub async fn quick_connect_sign_in(self: Arc<Self>, secret: String) -> Result<SignedIn, BloomError> {
        let client = self.client()?;
        let me = self.clone();
        run(move || {
            let auth = client.authenticate_quick_connect(&secret)?;
            Ok(me.open_session(&client, &auth))
        })
        .await
    }

    /// Approves another device's code for this user.
    pub async fn quick_connect_authorize(&self, code: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || client.quick_connect_authorize(&code).map_err(Into::into)).await
    }

    /// The sessions of this user that this app may control (not itself).
    pub async fn remote_sessions(&self) -> Result<Vec<RemoteSession>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            use bloom_core::cast::protocol::{SessionInfo, controllable};
            let user = client.user_id.as_deref().unwrap_or_default().to_string();
            let all: Vec<SessionInfo> = client.sessions(&[("controllableByUserId", user)])?;
            Ok(controllable(all, &client.device_id)
                .into_iter()
                .map(|s| RemoteSession {
                    now_playing: s.now_playing_title(),
                    paused: s.play_state.is_paused,
                    position_secs: s.position_secs(),
                    duration_secs: s.duration_secs(),
                    id: s.id,
                    device_name: s.device_name,
                    client: s.client,
                })
                .collect())
        })
        .await
    }

    /// Items to a session.
    pub async fn remote_play(&self, session_id: String, item_ids: Vec<String>, command: PlayCommand, start_secs: Option<f64>) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let query = bloom_core::cast::protocol::play_query(&item_ids, command.into(), start_secs);
            client.session_play(&session_id, &query).map_err(Into::into)
        })
        .await
    }

    /// A playstate command for a session.
    pub async fn remote_playstate(&self, session_id: String, command: PlaystateCommand) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let (name, seek_secs) = command.request();
            let query: Vec<_> = seek_secs
                .map(|secs| ("seekPositionTicks", bloom_core::cast::protocol::secs_to_ticks(secs).to_string()))
                .into_iter()
                .collect();
            client.session_playstate(&session_id, name, &query).map_err(Into::into)
        })
        .await
    }

    /// Ends the server's transcode of a play session (on stop and on quit).
    pub async fn end_transcode(&self, play_session_id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let device = client.device_id.to_string();
            client
                .call(bloom_core::jellyfin::Method::Delete, "/Videos/ActiveEncodings", &[("deviceId", device), ("playSessionId", play_session_id)])
                .map_err(Into::into)
        })
        .await
    }

    /// A playback report. Off when BLOOM_NO_REPORT is set (core's switch).
    #[allow(clippy::too_many_arguments, reason = "an export: Swift calls it with labels, a struct adds a type for one call")]
    pub async fn report(&self, kind: ReportKind, info_item_id: String, media_source_id: String, play_session_id: String, play_method: PlayMethod, position_secs: f64, paused: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let progress = Progress {
                item_id: info_item_id,
                play_session_id,
                position_ticks: (position_secs * 10_000_000.) as i64,
                paused,
                volume: 100,
                muted: false,
                play_method: play_method.text().to_string(),
                media_source_id,
            };
            match kind {
                ReportKind::Start => client.report_start(&progress),
                ReportKind::Progress => client.report_progress(&progress),
                ReportKind::Stopped => client.report_stopped(&progress),
            }
            .map_err(Into::into)
        })
        .await
    }

    /// The episode that took a lost one's place: a file the server replaced
    /// (a new release) gets a new id, and the old one answers 404. Same
    /// series, same numbers (core's `queue::replacement`).
    pub async fn refind_episode(&self, lost_id: String, series_id: String, season: Option<i32>, episode: Option<i32>) -> Result<Option<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            if episode.is_none() {
                return Ok(None);
            }
            let episodes = client.season_episodes(&series_id, season)?;
            Ok(episodes
                .iter()
                .find(|e| e.id != lost_id && e.index_number == episode && e.parent_index_number == season)
                .map(|e| media(&client, e)))
        })
        .await
    }

    /// The audio and subtitle tracks of an item's first source.
    pub async fn streams(&self, item_id: String) -> Result<Vec<StreamOption>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            use bloom_core::jellyfin::StreamKind;
            let item = client.item(&item_id)?;
            let Some(source) = item.media_sources.first() else { return Ok(Vec::new()) };
            Ok(source
                .media_streams
                .iter()
                .filter(|s| matches!(s.kind, StreamKind::Audio | StreamKind::Subtitle))
                .map(|s| StreamOption {
                    index: s.index,
                    kind: if s.kind == StreamKind::Audio { TrackKind::Audio } else { TrackKind::Subtitle },
                    title: s.display_title.clone().or_else(|| s.title.clone()).unwrap_or_else(|| format!("Track {}", s.index)),
                    is_default: s.is_default,
                })
                .collect())
        })
        .await
    }

    /// For the stress test: holds a pool thread for `ms` like a slow
    /// request, then answers `ms`.
    pub async fn slow_echo(&self, ms: u64) -> Result<u64, BloomError> {
        run(move || {
            thread::sleep(Duration::from_millis(ms));
            Ok(ms)
        })
        .await
    }

    /// For the spike: a panic inside a pool call.
    pub async fn panic_in_a_call(&self) -> Result<(), BloomError> {
        run(|| panic!("a test panic in a call")).await
    }

    /// A stream of `count` numbers, one every `every_ms`, from a thread of
    /// its own: the shape of the socket, cast and download streams.
    pub fn ticker(&self, every_ms: u64, count: u32) -> Arc<Ticker> {
        let (tx, rx) = async_channel::unbounded();
        thread::spawn(move || {
            for n in 0..count {
                thread::sleep(Duration::from_millis(every_ms));
                if tx.send_blocking(n).is_err() {
                    return;
                }
            }
        });
        Arc::new(Ticker { rx })
    }
}

/// A subscription: `next()` until it answers nothing, or `close()`.
#[derive(uniffi::Object)]
pub struct Ticker {
    rx: async_channel::Receiver<u32>,
}

#[uniffi::export]
impl Ticker {
    pub async fn next(&self) -> Option<u32> {
        self.rx.recv().await.ok()
    }

    pub fn close(&self) {
        self.rx.close();
    }
}

// ----- Chromecast ------------------------------------------------------------

use bloom_core::chromecast::{
    discovery::{Device, Discovery},
    session::{Event as CastSessionEvent, Session as CastSessionCore},
};

/// A Chromecast on the network (or the test's fake one).
#[derive(uniffi::Record, Clone)]
pub struct CastDevice {
    pub id: String,
    pub name: String,
    pub model: String,
    pub address: String,
    pub port: u16,
}

impl From<&Device> for CastDevice {
    fn from(d: &Device) -> Self {
        CastDevice { id: d.id.clone(), name: d.name.clone(), model: d.model.clone(), address: d.address.to_string(), port: d.port }
    }
}

impl CastDevice {
    fn device(&self) -> Result<Device, BloomError> {
        Ok(Device {
            id: self.id.clone(),
            name: self.name.clone(),
            model: self.model.clone(),
            address: self.address.parse().map_err(|_| BloomError::Internal { message: "device address".into() })?,
            port: self.port,
            instance: self.name.clone(),
        })
    }
}

/// Looks for Chromecasts while it lives (Bonjour, `_googlecast._tcp`);
/// the app keeps it only while the Play on sheet is open.
#[derive(uniffi::Object)]
pub struct CastDiscovery(Discovery);

#[uniffi::export]
impl CastDiscovery {
    #[uniffi::constructor]
    pub fn start() -> Arc<Self> {
        Arc::new(Self(bloom_core::chromecast::discovery::discover()))
    }

    pub fn devices(&self) -> Vec<CastDevice> {
        self.0.devices().iter().map(CastDevice::from).collect()
    }
}

/// The fake Chromecast of core's tests, on this device; never a real one.
/// Only with the `cast-mock` feature (a default one: the app's tests and
/// `castMockDropConnections` need it).
#[cfg(feature = "cast-mock")]
#[derive(uniffi::Object)]
pub struct CastMock(#[allow(dead_code, reason = "held for its threads; nothing reads it")] bloom_core::chromecast::mock::Mock, CastDevice);

#[cfg(feature = "cast-mock")]
#[uniffi::export]
impl CastMock {
    #[uniffi::constructor]
    pub fn start() -> Arc<Self> {
        let mock = bloom_core::chromecast::mock::Mock::start();
        lock(&CAST_MOCKS).push(mock.state.clone());
        let device = CastDevice::from(&mock.device());
        Arc::new(Self(mock, device))
    }

    /// The fake device without the Jellyfin receiver app: a load goes to
    /// the Default Media Receiver, for the tests of that path.
    #[uniffi::constructor]
    pub fn media_receiver() -> Arc<Self> {
        let mock = bloom_core::chromecast::mock::Mock::start();
        mock.set(|state| state.available.retain(|app| !bloom_core::chromecast::jellyfin::is_jellyfin_app(app)));
        lock(&CAST_MOCKS).push(mock.state.clone());
        let device = CastDevice::from(&mock.device());
        Arc::new(Self(mock, device))
    }

    pub fn device(&self) -> CastDevice {
        self.1.clone()
    }
}

/// The fake devices this process started: a mock outlives its object (its
/// threads run on), and the connection to it outlives the Play on sheet
/// that made it.
#[cfg(feature = "cast-mock")]
static CAST_MOCKS: Mutex<Vec<Arc<Mutex<bloom_core::chromecast::mock::MockState>>>> = Mutex::new(Vec::new());

/// For a test of casting with the phone asleep (spec §8.4): every fake
/// device closes the socket it serves, as a socket that died while the app
/// was suspended. The session connects again by itself. Never a real
/// device: only the mocks of this process are in the list.
#[cfg(feature = "cast-mock")]
#[uniffi::export]
pub fn cast_mock_drop_connections() {
    for state in lock(&CAST_MOCKS).iter() {
        let mut state = lock(state);
        // Only a mock with a connection: the flag waits for the next one
        // otherwise, and would drop it.
        if state.connections > 0 {
            state.drop_now = true;
        }
    }
}

/// What the device does now.
#[derive(uniffi::Record)]
pub struct CastStatus {
    pub connected: bool,
    pub loaded: bool,
    pub paused: bool,
    pub buffering: bool,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub volume: f64,
    pub muted: bool,
    pub app_name: Option<String>,
    pub error: Option<String>,
    /// The item this app loaded, while the device holds it.
    pub item_id: Option<String>,
    pub title: Option<String>,
    /// The Jellyfin receiver app runs (else the Default Media Receiver or
    /// another sender's app).
    pub jellyfin_app: bool,
    /// The streams that play, by the server's index (-1: subtitles off);
    /// none when nobody said. The Jellyfin app reports them; for the
    /// media receiver they are the ones this app asked for.
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
    /// An audio or subtitle choice works now: an item of this app plays.
    pub can_tracks: bool,
}

impl CastStatus {
    /// What a status says when it could not be read: nothing connected.
    fn unknown() -> Self {
        CastStatus {
            connected: false,
            loaded: false,
            paused: false,
            buffering: false,
            position_secs: 0.,
            duration_secs: None,
            volume: 0.,
            muted: false,
            app_name: None,
            error: None,
            item_id: None,
            title: None,
            jellyfin_app: false,
            audio_index: None,
            subtitle_index: None,
            can_tracks: false,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum CastEvent {
    /// The status changed: read `status()`.
    Changed,
    /// The certificate to keep for this device (trust on first use).
    Pinned { certificate: Vec<u8> },
    /// The item of a load is gone from the device.
    Ended,
    Error { message: String },
    Closed,
}

/// A connection to one Chromecast. Dropping it (or `close`) ends it.
#[derive(uniffi::Object)]
pub struct Chromecast {
    session: CastSessionCore,
    events: async_channel::Receiver<CastSessionEvent>,
    client: Client,
    server_id: String,
    loaded: Mutex<CastLoaded>,
}

/// What this app loaded on the device, as the Mac's `CastLoad` keeps it.
#[derive(Default)]
struct CastLoaded {
    item: Option<Item>,
    /// The number the session gave the load; its end comes with it.
    load: u64,
    audio_index: Option<i64>,
    subtitle_index: Option<i64>,
}

#[uniffi::export]
impl Server {
    /// Connects to a Chromecast; `pin` is the certificate kept from an
    /// earlier connection to it.
    pub fn cast_connect(&self, device: CastDevice, pin: Option<Vec<u8>>, server_id: String) -> Result<Arc<Chromecast>, BloomError> {
        let client = self.signed_in()?;
        let (session, events) = CastSessionCore::connect(device.device()?, pin);
        Ok(Arc::new(Chromecast { session, events, client, server_id, loaded: Mutex::default() }))
    }
}

impl Chromecast {
    fn loaded(&self) -> std::sync::MutexGuard<'_, CastLoaded> {
        lock(&self.loaded)
    }

    /// A report of the Jellyfin receiver app names the streams it plays,
    /// and the end of the load of this app's item takes the item. The end
    /// of an older load does not count: a new item makes the device change
    /// its app, and the end of the app before comes with the older number.
    fn note(&self, event: &CastSessionEvent) {
        use bloom_core::chromecast::jellyfin;
        let mut loaded = self.loaded();
        match event {
            CastSessionEvent::Message { namespace, payload } if namespace == jellyfin::NAMESPACE => {
                if let Some(report) = jellyfin::parse_report(payload) {
                    if report.audio_index.is_some() {
                        loaded.audio_index = report.audio_index;
                    }
                    if report.subtitle_index.is_some() {
                        loaded.subtitle_index = report.subtitle_index;
                    }
                }
            }
            CastSessionEvent::Ended { load } if *load == loaded.load => loaded.item = None,
            _ => {}
        }
    }

    fn status_now(&self) -> CastStatus {
        use bloom_core::chromecast::messages::PlayerState;
        let s = self.session.status();
        let loaded = self.loaded();
        let holds = s.loaded() && loaded.item.is_some();
        CastStatus {
            connected: s.connected,
            loaded: s.loaded(),
            paused: s.player_state == Some(PlayerState::Paused),
            buffering: s.player_state == Some(PlayerState::Buffering),
            position_secs: s.position_now(),
            duration_secs: s.duration,
            volume: s.volume,
            muted: s.muted,
            app_name: s.app.as_ref().map(|a| a.display_name.clone()),
            error: s.error.clone(),
            item_id: loaded.item.as_ref().filter(|_| holds).map(|item| item.id.clone()),
            title: loaded.item.as_ref().filter(|_| holds).map(Item::display_title),
            jellyfin_app: s.playing_jellyfin_app(),
            audio_index: loaded.audio_index,
            subtitle_index: loaded.subtitle_index,
            can_tracks: holds,
        }
    }

    /// The Default Media Receiver plays one stream, made for one audio
    /// stream and with the subtitle burned in: another choice sends the
    /// item again with the new streams, from the place it plays now.
    fn reload_stream(&self, loaded: &mut CastLoaded) {
        let Some(item) = loaded.item.clone() else { return };
        let start = self.session.status().position_now();
        let mut request = bloom_core::chromecast::jellyfin::load_request(&self.client, &self.server_id, &[item], start);
        request.jellyfin = None;
        let Some(media) = request.media.as_mut() else { return };
        if let Some(index) = loaded.audio_index {
            media.url.push_str(&format!("&AudioStreamIndex={index}"));
        }
        if let Some(index) = loaded.subtitle_index.filter(|index| *index >= 0) {
            media.url.push_str(&format!("&SubtitleStreamIndex={index}"));
        }
        loaded.load = self.session.load(request);
    }
}

#[uniffi::export]
impl Chromecast {
    pub fn status(&self) -> CastStatus {
        guarded("cast status", CastStatus::unknown(), || self.status_now())
    }

    /// The next event of the connection; nothing once it is closed.
    pub async fn next_event(&self) -> Option<CastEvent> {
        let event = self.events.recv().await.ok()?;
        self.note(&event);
        Some(match event {
            CastSessionEvent::Pinned(certificate) => CastEvent::Pinned { certificate },
            CastSessionEvent::Ended { .. } => CastEvent::Ended,
            CastSessionEvent::Error(message) => CastEvent::Error { message },
            CastSessionEvent::Closed => CastEvent::Closed,
            _ => CastEvent::Changed,
        })
    }

    /// Plays an item on the device: the Jellyfin receiver app when it
    /// launches, else the Default Media Receiver with the server's stream.
    pub async fn load(self: Arc<Self>, item_id: String, start_secs: f64) -> Result<(), BloomError> {
        let me = self.clone();
        run(move || {
            let item = me.client.item(&item_id)?;
            let request = bloom_core::chromecast::jellyfin::load_request(&me.client, &me.server_id, std::slice::from_ref(&item), start_secs);
            let mut loaded = me.loaded();
            *loaded = CastLoaded { item: Some(item), ..Default::default() };
            loaded.load = me.session.load(request);
            Ok(())
        })
        .await
    }

    pub fn play(&self) {
        self.session.play();
    }

    pub fn pause(&self) {
        self.session.pause();
    }

    pub fn seek(&self, secs: f64) {
        self.session.seek(secs);
    }

    pub fn stop(&self) {
        self.session.stop_media();
    }

    /// `level` from 0 to 1.
    pub fn set_volume(&self, level: f64) {
        self.session.set_volume(level);
    }

    pub fn set_muted(&self, muted: bool) {
        self.session.set_muted(muted);
    }

    /// An audio stream of the item, by the server's index. The Jellyfin
    /// app changes it in place; the media receiver gets the stream again.
    pub fn set_audio(&self, index: i64) {
        guarded("cast audio", (), || {
            let mut loaded = self.loaded();
            loaded.audio_index = Some(index);
            if self.session.status().playing_jellyfin_app() {
                self.session.set_audio(index);
            } else {
                self.reload_stream(&mut loaded);
            }
        });
    }

    /// A subtitle of the item, by the server's index; -1 turns them off.
    pub fn set_subtitle(&self, index: i64) {
        guarded("cast subtitle", (), || {
            let mut loaded = self.loaded();
            loaded.subtitle_index = Some(index);
            if self.session.status().playing_jellyfin_app() {
                self.session.set_subtitle((index >= 0).then_some(index));
            } else {
                self.reload_stream(&mut loaded);
            }
        });
    }

    /// Asks the device for its state again, after the app was away (spec
    /// §8.4): a socket that died meanwhile fails on this and the session
    /// connects again.
    pub fn refresh(&self) {
        drop(self.session.get_status());
        self.session.media_status();
    }

    /// Before another target: the device keeps what another sender put
    /// on it, but the item this app loaded stops, as on the Mac.
    pub fn leave(&self) {
        if self.loaded().item.is_some() && self.session.status().loaded() {
            self.session.stop_media();
        }
    }

    pub fn close(&self) {
        self.events.close();
    }
}

#[cfg(test)]
mod tests {

    /// Continue Watching shows the show, not the clip: an episode with its
    /// own still and a series thumb gets the thumb; with no thumb, the
    /// series backdrop; with neither, its still.
    #[test]
    fn an_episode_card_shows_its_show() {
        let client = Client::new("http://127.0.0.1:1", "show-test").with_session("token", "u1");
        let item = |extra: serde_json::Value| -> Item {
            let mut base = serde_json::json!({"Id": "e1", "Name": "Pilot", "Type": "Episode", "SeriesId": "s1",
                "ImageTags": {"Primary": "still", "Thumb": "clip"}});
            base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value(base).unwrap()
        };
        let thumb = show_wide(&client, &item(serde_json::json!({"SeriesThumbImageTag": "st"}))).unwrap();
        assert!(thumb.contains("/Items/s1/Images/Thumb"), "{thumb}");
        let backdrop = show_wide(&client, &item(serde_json::json!({"ParentBackdropItemId": "s1", "ParentBackdropImageTags": ["bd"]}))).unwrap();
        assert!(backdrop.contains("/Items/s1/Images/Backdrop"), "{backdrop}");
        let own = show_wide(&client, &item(serde_json::json!({}))).unwrap();
        assert!(own.contains("/Items/e1/"), "{own}");
        let poster = show_poster(&client, &item(serde_json::json!({"SeriesPrimaryImageTag": "sp"}))).unwrap();
        assert!(poster.contains("/Items/s1/Images/Primary"), "{poster}");
        let still_poster = show_poster(&client, &item(serde_json::json!({}))).unwrap();
        assert!(still_poster.contains("/Items/e1/Images/Primary"), "{still_poster}");
        // The episode list shows the episode's own still, not the show.
        let own = still(&client, &item(serde_json::json!({"SeriesThumbImageTag": "st"}))).unwrap();
        assert!(own.contains("/Items/e1/Images/Primary"), "{own}");
    }

    use std::{
        collections::HashMap,
        future::Future,
        pin::pin,
        task::{Context, Poll, Wake, Waker},
    };

    use super::*;

    /// The Keychain as a map.
    #[derive(Default)]
    struct Store(Mutex<HashMap<String, String>>);

    impl SecretStore for Store {
        fn get(&self, key: String) -> Option<String> {
            lock(&self.0).get(&key).cloned()
        }
        fn set(&self, key: String, value: String) {
            lock(&self.0).insert(key, value);
        }
        fn remove(&self, key: String) {
            lock(&self.0).remove(&key);
        }
    }

    struct Unpark(thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    /// Waits for a future on this thread (the crate has no executor).
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
            thread::park();
        }
    }

    #[test]
    fn a_sign_out_forgets_the_token_and_ends_the_session() {
        let store = Arc::new(Store::default());
        // Port 1: the logout fails at once, as a server that is away.
        let server = Server::new("http://127.0.0.1:1".into(), "ffi-test".into(), store.clone());
        store.set(token_key("u1"), "secret".into());
        assert!(server.restore("u1".into()).expect("restore"));
        assert!(server.signed_in().is_ok());

        block_on(server.clone().sign_out("u1".into())).expect("sign out");
        assert_eq!(store.get(token_key("u1")), None, "the store kept the token");
        assert!(matches!(server.signed_in(), Err(BloomError::NotSignedIn)), "the session lived on");
        assert!(!server.restore("u1".into()).expect("restore"), "a profile without a token opened");

        // An empty token, as a sign-out of an older build left it.
        store.set(token_key("u1"), String::new());
        assert!(!server.restore("u1".into()).expect("restore"), "an empty token opened a session");
    }
}
