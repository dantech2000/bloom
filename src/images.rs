// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Remote image cache. The native GPUI platform ships without an HTTP client,
//! so artwork is fetched and decoded here and handed to GPUI as ready pixels.
//! Decoded pixels are the real cost (a 300x450 poster is 540 KB, plus the same
//! again in the GPU atlas), so the cache is bounded by decoded bytes.

use std::{
    collections::HashMap,
    io::Cursor,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{
    App, Bounds, Canvas, Corners, Global, ImageFormat, ObjectFit, Pixels, RenderImage, Window,
    canvas, px,
};
use image::{DynamicImage, Frame, ImageDecoder as _};
use smallvec::SmallVec;

/// Decoded bytes kept before the least recently shown images are dropped.
const MAX_BYTES: usize = 64 * 1024 * 1024;
/// Images shown this recently are never dropped. A window that shows more
/// than the budget then overshoots it instead of refetching in a loop.
const KEEP_RECENT: Duration = Duration::from_secs(5);

/// Byte-bounded map that evicts the least recently used entries.
struct Lru<T> {
    entries: HashMap<String, Entry<T>>,
    bytes: usize,
    max_bytes: usize,
}

struct Entry<T> {
    value: T,
    bytes: usize,
    used: Instant,
}

impl<T> Lru<T> {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            max_bytes,
        }
    }

    /// Looks up `key` and marks it as used at `now`.
    fn get(&mut self, key: &str, now: Instant) -> Option<&T> {
        let entry = self.entries.get_mut(key)?;
        entry.used = now;
        Some(&entry.value)
    }

    /// Stores `value` and returns the values that were dropped to make room.
    /// The new entry and entries used within `KEEP_RECENT` are never dropped.
    fn insert(&mut self, key: String, value: T, bytes: usize, now: Instant) -> Vec<T> {
        let mut evicted = Vec::new();
        let entry = Entry {
            value,
            bytes,
            used: now,
        };
        if let Some(old) = self.entries.insert(key.clone(), entry) {
            self.bytes -= old.bytes;
            evicted.push(old.value);
        }
        self.bytes += bytes;
        while self.bytes > self.max_bytes {
            let oldest = self
                .entries
                .iter()
                .filter(|(k, e)| **k != key && now.duration_since(e.used) >= KEEP_RECENT)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone());
            let Some(oldest) = oldest else { break };
            let old = self.entries.remove(&oldest).unwrap();
            self.bytes -= old.bytes;
            evicted.push(old.value);
        }
        evicted
    }
}

enum Slot {
    Loading,
    Failed {
        failures: u32,
        at: Instant,
        /// [`crate::jellyfin::session_count`] when it failed.
        session: u64,
        /// A 404 or a file that is no image will not get better.
        retry: bool,
    },
}

/// Waits before the first, second and third retry; later retries wait as
/// long as the last.
const RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(30), Duration::from_secs(120)];

/// May a download that failed `failures` times, `since` ago, be tried again?
/// A new session (the app connected again) clears the wait.
fn may_retry(failures: u32, since: Duration, retry: bool, new_session: bool) -> bool {
    retry && (new_session || since >= retry_delay(failures))
}

fn retry_delay(failures: u32) -> Duration {
    RETRY_DELAYS[(failures.max(1) as usize - 1).min(RETRY_DELAYS.len() - 1)]
}

/// Is a failed fetch worth another try: a timeout, a network error or a
/// server error is, a 404 or 410 and a bad image are not.
fn retryable(err: &anyhow::Error) -> bool {
    match err.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(code)) => !matches!(code, 404 | 410),
        Some(_) => true,
        None => false,
    }
}

/// How many images are fetched at once. A page asks for dozens; each fetch
/// holds a socket and a background thread while it blocks.
const MAX_FETCHES: usize = 6;

/// Limits the fetches in flight. A waiter does not block a thread: it awaits
/// a channel. The last to ask is served first, so the images on screen now do
/// not wait behind those of a page that was scrolled away.
struct Gate {
    limit: usize,
    state: std::sync::Mutex<GateState>,
}

struct GateState {
    active: usize,
    waiting: Vec<async_channel::Sender<()>>,
}

/// A slot of the [`Gate`]; it is given back when dropped.
struct Permit(Arc<Gate>);

impl Gate {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            state: std::sync::Mutex::new(GateState { active: 0, waiting: Vec::new() }),
        })
    }

    async fn acquire(self: &Arc<Self>) -> Permit {
        let wait = {
            let mut state = self.state.lock().unwrap();
            if state.active < self.limit {
                state.active += 1;
                None
            } else {
                let (tx, rx) = async_channel::bounded(1);
                state.waiting.push(tx);
                Some(rx)
            }
        };
        if let Some(rx) = wait {
            // The slot is handed over by the permit that was dropped.
            let _ = rx.recv().await;
        }
        Permit(self.clone())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        // Pass the slot on to the newest waiter that is still there.
        while let Some(tx) = state.waiting.pop() {
            if tx.try_send(()).is_ok() {
                return;
            }
        }
        state.active -= 1;
    }
}

fn gate() -> &'static Arc<Gate> {
    static GATE: std::sync::LazyLock<Arc<Gate>> = std::sync::LazyLock::new(|| Gate::new(MAX_FETCHES));
    &GATE
}

struct ImageStore {
    /// Downloads in flight and downloads that failed.
    slots: HashMap<String, Slot>,
    ready: Lru<Arc<RenderImage>>,
    agent: Option<ureq::Agent>,
    /// A redraw for arrived images is on its way.
    refresh_pending: bool,
    /// Encoded bytes of the images [`preload`] fetched ahead of use. Such an
    /// image only needs its decode when it is asked for.
    raw: HashMap<String, Arc<Vec<u8>>>,
    /// Counts the calls of [`preload`]; a download of an older call is dropped.
    raw_epoch: u64,
    /// The sheet of tiles decoded last, for [`tile`]. It is never painted,
    /// so the GPU holds no copy of it.
    sheet: Option<(String, Arc<RenderImage>)>,
}
impl Global for ImageStore {}

impl ImageStore {
    fn new() -> Self {
        // Once in each run, off the main thread.
        std::thread::spawn(trim_disk_cache);
        Self {
            slots: HashMap::new(),
            ready: Lru::new(MAX_BYTES),
            agent: None,
            refresh_pending: false,
            raw: HashMap::new(),
            raw_epoch: 0,
            sheet: None,
        }
    }

    fn agent(&mut self) -> ureq::Agent {
        self.agent
            .get_or_insert_with(|| {
                ureq::Agent::new_with_config(
                    ureq::Agent::config_builder()
                        .timeout_global(Some(Duration::from_secs(20)))
                        // A page asks for dozens of images at once, and a scroll
                        // asks for more a while later. Keeping more connections
                        // open, and for longer, saves a TLS handshake on each.
                        .max_idle_connections(16)
                        .max_idle_connections_per_host(8)
                        .max_idle_age(Duration::from_secs(90))
                        .build(),
                )
            })
            .clone()
    }
}

/// Fetches images ahead of use and keeps their encoded bytes, which are
/// small; [`image`] then only has to decode one. The player does this for
/// the preview sheets of its timeline: a decoded sheet is too large to keep
/// them all, and a download for each takes too long during a drag. A new
/// call replaces the set of the call before it; an empty list frees it.
pub fn preload(urls: Vec<String>, cx: &mut App) {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    let store = cx.global_mut::<ImageStore>();
    store.raw_epoch += 1;
    let epoch = store.raw_epoch;
    store.raw.retain(|url, _| urls.contains(url));
    if store.sheet.as_ref().is_some_and(|(url, _)| !urls.contains(url)) {
        store.sheet = None;
    }
    let agent = store.agent();
    cx.spawn(async move |cx| {
        for url in urls {
            let known = cx.update(|cx| {
                let store = cx.global::<ImageStore>();
                (store.raw_epoch != epoch).then_some(()).ok_or(store.raw.contains_key(&url))
            });
            match known {
                // A newer call took over.
                Ok(()) => return,
                Err(true) => continue,
                Err(false) => {}
            }
            let (agent, target) = (agent.clone(), url.clone());
            let permit = gate().acquire().await;
            let fetched = cx
                .background_executor()
                .spawn(async move { fetch_bytes(&agent, &target) })
                .await;
            drop(permit);
            cx.update(|cx| {
                let store = cx.global_mut::<ImageStore>();
                if store.raw_epoch != epoch {
                    return;
                }
                match fetched {
                    Ok(bytes) => {
                        store.raw.insert(url, Arc::new(bytes));
                    }
                    Err(err) => {
                        let shown = url.split('?').next().unwrap_or_default();
                        log::warn!("preload of {shown} failed: {err:#}");
                    }
                }
            });
        }
    })
    .detach();
}

/// Returns the cached image for `url`, starting a download the first time.
/// Windows are refreshed when the image arrives.
pub fn image(url: &str, cx: &mut App) -> Option<Arc<RenderImage>> {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    let store = cx.global_mut::<ImageStore>();
    if let Some(image) = store.ready.get(url, Instant::now()) {
        return Some(image.clone());
    }
    let failures = match store.slots.get(url) {
        None => 0,
        Some(Slot::Loading) => return None,
        Some(Slot::Failed { failures, at, session, retry }) => {
            let new_session = *session != crate::jellyfin::session_count();
            if !may_retry(*failures, at.elapsed(), *retry, new_session) {
                return None;
            }
            *failures
        }
    };
    store.slots.insert(url.to_string(), Slot::Loading);
    let agent = store.agent();
    let raw = store.raw.get(url).cloned();
    let key = url.to_string();
    let target = url.to_string();
    cx.spawn(async move |cx| {
        let permit = match raw {
            Some(_) => None,
            None => Some(gate().acquire().await),
        };
        let fetched = cx
            .background_executor()
            .spawn(async move {
                match raw {
                    Some(bytes) => decode_fetched("", &bytes),
                    None => download(&agent, &target),
                }
            })
            .await;
        drop(permit);
        cx.update(|cx| {
            let store = cx.global_mut::<ImageStore>();
            match fetched {
                Ok(image) => {
                    store.slots.remove(&key);
                    let bytes = image.as_bytes(0).map_or(0, |b| b.len());
                    let evicted = store
                        .ready
                        .insert(key, Arc::new(image), bytes, Instant::now());
                    // GPUI keeps a second copy of every painted image in each
                    // window's sprite atlas until it is told to drop it.
                    for old in evicted {
                        cx.drop_image(old, None);
                    }
                }
                Err(err) => {
                    // The query can hold the access token; keep it out of the log.
                    let shown = key.split('?').next().unwrap_or_default();
                    log::warn!("image {shown} failed: {err:#}");
                    let retry = retryable(&err);
                    store.slots.insert(key, failed(failures + 1, retry));
                    if retry {
                        retry_refresh(failures + 1, cx);
                    }
                }
            }
            schedule_refresh(cx);
        });
    })
    .detach();
    None
}

/// One tile out of a sheet of `columns` by `rows` tiles, as its own small
/// image. The player uses this for the preview frames of its timeline: a
/// sheet of them is some 24 MB of pixels, and painting from the sheet itself
/// would put all of it in the GPU atlas for one frame of 230 KB. The sheet is
/// decoded once and kept while tiles of it are asked for.
pub fn tile(
    url: &str,
    (columns, rows): (u32, u32),
    (column, row): (u32, u32),
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    let key = format!("{url}#{column},{row}");
    let store = cx.global_mut::<ImageStore>();
    if let Some(image) = store.ready.get(&key, Instant::now()) {
        return Some(image.clone());
    }
    if let Some((_, sheet)) = store.sheet.as_ref().filter(|(sheet_url, _)| sheet_url == url) {
        let tile = Arc::new(crop(sheet, (columns, rows), (column, row))?);
        let bytes = tile.as_bytes(0).map_or(0, |b| b.len());
        let evicted = store.ready.insert(key, tile.clone(), bytes, Instant::now());
        for old in evicted {
            cx.drop_image(old, None);
        }
        return Some(tile);
    }
    let failures = match store.slots.get(url) {
        None => 0,
        Some(Slot::Loading) => return None,
        Some(Slot::Failed { failures, at, session, retry }) => {
            let new_session = *session != crate::jellyfin::session_count();
            if !may_retry(*failures, at.elapsed(), *retry, new_session) {
                return None;
            }
            *failures
        }
    };
    store.slots.insert(url.to_string(), Slot::Loading);
    let agent = store.agent();
    let raw = store.raw.get(url).cloned();
    let key = url.to_string();
    let target = url.to_string();
    cx.spawn(async move |cx| {
        let permit = match raw {
            Some(_) => None,
            None => Some(gate().acquire().await),
        };
        let fetched = cx
            .background_executor()
            .spawn(async move {
                match raw {
                    Some(bytes) => decode_fetched("", &bytes),
                    None => download(&agent, &target),
                }
            })
            .await;
        drop(permit);
        cx.update(|cx| {
            let store = cx.global_mut::<ImageStore>();
            match fetched {
                Ok(sheet) => {
                    store.slots.remove(&key);
                    store.sheet = Some((key, Arc::new(sheet)));
                }
                Err(err) => {
                    let shown = key.split('?').next().unwrap_or_default();
                    log::warn!("image {shown} failed: {err:#}");
                    let retry = retryable(&err);
                    store.slots.insert(key, failed(failures + 1, retry));
                    if retry {
                        retry_refresh(failures + 1, cx);
                    }
                }
            }
            schedule_refresh(cx);
        });
    })
    .detach();
    None
}

/// Copies one tile out of a decoded sheet.
fn crop(sheet: &RenderImage, (columns, rows): (u32, u32), (column, row): (u32, u32)) -> Option<RenderImage> {
    let size = sheet.size(0);
    let (sheet_w, sheet_h) = (size.width.0 as u32, size.height.0 as u32);
    let (w, h) = (sheet_w / columns.max(1), sheet_h / rows.max(1));
    if w == 0 || h == 0 || column >= columns || row >= rows {
        return None;
    }
    let pixels = sheet.as_bytes(0)?;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in row * h..(row + 1) * h {
        let start = ((y * sheet_w + column * w) * 4) as usize;
        data.extend_from_slice(pixels.get(start..start + (w * 4) as usize)?);
    }
    let tile = image::RgbaImage::from_raw(w, h, data)?;
    Some(RenderImage::new(SmallVec::from_elem(Frame::new(tile), 1)))
}

fn failed(failures: u32, retry: bool) -> Slot {
    Slot::Failed { failures, at: Instant::now(), session: crate::jellyfin::session_count(), retry }
}

/// Paints again when a failed image may be tried again; the paint asks for
/// the image, and that starts the new try.
fn retry_refresh(failures: u32, cx: &mut App) {
    let delay = retry_delay(failures);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

/// Redraws the windows once for all images that arrive close together. A
/// page that opens gets dozens of images within a moment; one redraw for
/// each of them makes the page stutter.
fn schedule_refresh(cx: &mut App) {
    let store = cx.global_mut::<ImageStore>();
    if std::mem::replace(&mut store.refresh_pending, true) {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(40))
            .await;
        cx.update(|cx| {
            cx.global_mut::<ImageStore>().refresh_pending = false;
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Paints the image for `url` over the element. The image is requested only
/// while part of the element is visible, so the off-screen cards of a long
/// page are not downloaded and age out of the cache.
pub fn remote_with(url: String, radius: Pixels, fit: ObjectFit) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
            // The mask is empty when a scrolled ancestor clips the element away.
            let visible = window.content_mask().bounds;
            if visible.size.width <= px(0.)
                || visible.size.height <= px(0.)
                || !bounds.intersects(&visible)
            {
                return;
            }
            let Some(image) = image(&url, cx) else {
                return;
            };
            let target = fit.get_bounds(bounds, image.size(0));
            let _ = window.paint_image(bounds, target, Corners::all(radius), image, 0, false);
        },
    )
}

/// Paints a title logo: scaled to fit, at the left edge, centred vertically.
pub fn remote_logo(url: String) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
            let Some(image) = image(&url, cx) else {
                return;
            };
            let mut target = ObjectFit::Contain.get_bounds(bounds, image.size(0));
            target.origin.x = bounds.origin.x;
            let _ = window.paint_image(bounds, target, Corners::default(), image, 0, false);
        },
    )
}

/// Where the bytes of an image came from, and so what to do with them.
enum Origin {
    /// A file of this Mac: nothing to keep.
    Local,
    /// The cache on disk. A file that does not decode goes, and the
    /// network is asked once more.
    Disk(std::path::PathBuf),
    /// The network, with the file of the cache the bytes may be kept in
    /// once they are known to be an image.
    Network(Option<std::path::PathBuf>),
}

struct Fetched {
    mime: String,
    bytes: Vec<u8>,
    origin: Origin,
}

impl Fetched {
    /// Keeps bytes of the network in the cache on disk. Called only after
    /// the whole image decoded ([`decodes`] where the caller keeps the
    /// bytes and not the pixels): a 200 that is no image (the sign-in page
    /// of a proxy), or an image with a damaged body, must not sit in the
    /// cache for ever.
    fn commit(&self) {
        if let Origin::Network(Some(file)) = &self.origin {
            disk_write(file, &self.bytes);
        }
    }
}

/// The content type and the bytes of an image, from the cache on disk when
/// the image is there, else from the network. `cache` is the folder of the
/// cache (`None` for no cache); the bytes of the network are not written to
/// it here, see [`Fetched::commit`].
fn fetch(agent: &ureq::Agent, url: &str, cache: Option<&std::path::Path>) -> anyhow::Result<Fetched> {
    let local = |bytes: Vec<u8>| Fetched { mime: String::new(), bytes, origin: Origin::Local };
    // A file of this Mac, such as the poster of a download.
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(local(std::fs::read(path)?));
    }
    let file = disk_path_in(cache, url);
    if let Some(bytes) = file.as_deref().and_then(disk_read) {
        return Ok(Fetched {
            mime: String::new(),
            bytes,
            origin: Origin::Disk(file.unwrap_or_default()),
        });
    }
    // The artwork of a download is on this Mac, server or no server.
    if let Some(path) = crate::downloads::local_image(url)
        && let Ok(bytes) = std::fs::read(path)
    {
        return Ok(local(bytes));
    }
    let mut request = agent.get(url);
    // The header goes to the signed-in server only. A redirect to another
    // host drops it (ureq does not forward it).
    if let Some(auth) = crate::jellyfin::image_auth_header(url) {
        request = request.header("Authorization", auth);
    }
    let mut response = request.call()?;
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    let bytes = response.body_mut().read_to_vec()?;
    Ok(Fetched { mime, bytes, origin: Origin::Network(file) })
}

/// True when the whole image decodes, as the loader decodes it. The
/// downloads ask before they keep artwork. Blocks for a decode (about 1 to
/// 2 ms for a poster): not for a GPUI thread.
pub fn looks_like_image(bytes: &[u8]) -> bool {
    decodes(bytes)
}

/// The check for bytes that are kept and not shown now (the raw bytes of
/// the Now Playing tile, the prewarm of the posters): the complete image
/// decodes, with the same code the loader uses, so what passes here shows
/// there. A header check is not enough: a good header over a damaged body
/// would sit in the cache for ever, and every raw read would find it.
/// An SVG must parse and draw.
///
/// What this does not catch: PNG has a CRC per chunk and the zlib check, so
/// it catches damage and truncation. WebP (the lossless kind that the
/// decoder checks as it reads) and GIF are caught when the data is cut or
/// the structure is hit, but a changed byte inside lossless WebP data can
/// decode to a wrong pixel. A JPEG has no checksum and the decoder of the
/// `image` crate is lenient by design (it fills a damaged scan in without
/// an error), so [`complete`] adds the one check that finds the common
/// damage, a cut-off file; a changed byte inside the scan of a JPEG is not
/// found by any decoder here. Damage in the later frames of an animated
/// GIF is not found either (only the first frame is decoded, as the loader
/// does).
fn decodes(bytes: &[u8]) -> bool {
    decode_checked("", bytes).is_ok_and(|(_, whole)| whole)
}

/// True when the bytes are not cut short. Only a JPEG needs the question:
/// the other decoders fail on a cut file, and the lenient one of JPEG does
/// not. A JPEG ends with the marker EOI (`FF D9`), maybe followed by
/// zero padding.
fn complete(format: ImageFormat, bytes: &[u8]) -> bool {
    if !matches!(format, ImageFormat::Jpeg) {
        return true;
    }
    let end = bytes.iter().rposition(|&byte| byte != 0).map_or(0, |last| last + 1);
    bytes[..end].ends_with(&[0xFF, 0xD9])
}

// ----- cache on disk --------------------------------------------------------

/// Encoded images kept on disk before the least recently used are removed.
const DISK_MAX_BYTES: u64 = 500 * 1024 * 1024;

/// Folder of the image files: `~/Library/Caches/bloom/images` on macOS, or
/// `BLOOM_CACHE_DIR` (a test instance with a cache of its own).
/// `BLOOM_NO_DISK_CACHE` switches the cache off, for measurements.
fn disk_dir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if std::env::var_os("BLOOM_NO_DISK_CACHE").is_some() {
            return None;
        }
        let dir = match std::env::var_os("BLOOM_CACHE_DIR") {
            Some(dir) => std::path::PathBuf::from(dir),
            None => dirs::cache_dir()?.join(crate::brand::FOLDER).join("images"),
        };
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_deref()
}

/// File of an image, when the image may be kept. A Jellyfin image whose URL
/// has a tag never changes under that URL, and neither does a TMDB image.
/// The preview sheets of the player are left out: they are large and one
/// playback uses them.
fn disk_path_in(dir: Option<&std::path::Path>, url: &str) -> Option<std::path::PathBuf> {
    let keeps = (url.contains("tag=") || url.contains("image.tmdb.org/"))
        && !url.contains("api_key=")
        && !url.contains("/Trickplay/");
    if !keeps {
        return None;
    }
    // FNV-1a, twice with different starts: the same name in every run.
    let hash = |start: u64| {
        url.bytes()
            .fold(start, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3))
    };
    Some(dir?.join(format!(
        "{:016x}{:016x}",
        hash(0xcbf2_9ce4_8422_2325),
        hash(0x8422_2325_cbf2_9ce4)
    )))
}

/// Puts a file of this Mac in the cache on disk under a URL, ahead of a
/// request for it, so a page shows it without the server. The downloads do
/// this for their artwork, under each width the pages ask for: the copy is
/// a clone on APFS, so the sixteen files of an item take the space of one.
pub fn seed_file(url: &str, path: &std::path::Path) {
    seed_file_in(disk_dir(), url, path)
}

fn seed_file_in(cache: Option<&std::path::Path>, url: &str, path: &std::path::Path) {
    if let Some(file) = disk_path_in(cache, url)
        && !file.exists()
    {
        let partial = file.with_extension("part");
        if std::fs::copy(path, &partial).is_ok() && std::fs::rename(&partial, &file).is_err() {
            let _ = std::fs::remove_file(&partial);
        }
    }
}

fn disk_read(file: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(file).ok()?;
    // The time of the last use decides which files go first.
    if let Ok(handle) = std::fs::File::options().write(true).open(file) {
        let _ = handle.set_modified(std::time::SystemTime::now());
    }
    Some(bytes)
}

fn disk_write(file: &std::path::Path, bytes: &[u8]) {
    // Written under another name first, so a reader never sees half a file.
    let partial = file.with_extension("part");
    if std::fs::write(&partial, bytes).is_ok() && std::fs::rename(&partial, file).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

/// Removes the least recently used files when the folder is over its limit.
fn trim_disk_cache() {
    trim_disk_cache_in(disk_dir(), DISK_MAX_BYTES);
}

fn trim_disk_cache_in(dir: Option<&std::path::Path>, max_bytes: u64) {
    let Some(dir) = dir else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|file| file.1).sum();
    if total <= max_bytes {
        return;
    }
    files.sort();
    for (_, size, path) in files {
        // Some room is left, so the next few images do not start this again.
        if total <= max_bytes / 10 * 9 {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= size;
        }
    }
}

/// The HTTP agent of the cache, for a fetch of its own (see [`fetch_bytes`]).
pub fn agent(cx: &mut App) -> ureq::Agent {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    cx.global_mut::<ImageStore>().agent()
}

/// The encoded bytes of an image, from the cache on disk or the server.
/// Blocks; for a background thread. The Now Playing tile makes its artwork
/// from them.
pub fn fetch_bytes(agent: &ureq::Agent, url: &str) -> anyhow::Result<Vec<u8>> {
    fetch_bytes_in(agent, url, disk_dir())
}

/// The caller wants the bytes, not the pixels, but they are kept only when
/// the whole image decodes ([`decodes`]); a file of the cache that does not
/// decode goes and the server is asked once more. For a background thread:
/// the decode takes a few ms.
fn fetch_bytes_in(agent: &ureq::Agent, url: &str, cache: Option<&std::path::Path>) -> anyhow::Result<Vec<u8>> {
    let mut fetched = fetch(agent, url, cache)?;
    let mut whole = decodes(&fetched.bytes);
    if !whole && let Origin::Disk(file) = &fetched.origin {
        let _ = std::fs::remove_file(file);
        fetched = fetch(agent, url, cache)?;
        whole = decodes(&fetched.bytes);
    }
    if whole {
        fetched.commit();
    }
    Ok(fetched.bytes)
}

/// Images one prewarm fetches at most, in the order of the catalog: the
/// cache holds 500 MB, and a library of ten thousand titles would not fit.
const PREWARM_MAX_URLS: usize = 3000;
/// Lost connections in a row after which a prewarm gives up: with the
/// server gone, each try would wait for its timeout.
const PREWARM_GIVE_UP: u32 = 3;
/// A long prewarm trims the cache after this many bytes written, so it
/// does not go over the limit until its end.
const PREWARM_TRIM_EVERY: u64 = 64 * 1024 * 1024;

/// The bounds of a prewarm; a test sets small ones.
#[derive(Clone, Copy)]
struct PrewarmLimits {
    max_urls: usize,
    trim_every: u64,
    cache_max_bytes: u64,
}

const PREWARM_LIMITS: PrewarmLimits = PrewarmLimits {
    max_urls: PREWARM_MAX_URLS,
    trim_every: PREWARM_TRIM_EVERY,
    cache_max_bytes: DISK_MAX_BYTES,
};

/// Counts the prewarms started. A run checks it between two fetches and
/// stops when a newer one was started, or [`cancel_prewarm`] was called.
static PREWARM_RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Stops the prewarm under way, at its next fetch: the session it was for
/// is gone. For the place a session closes without a new one.
pub fn cancel_prewarm() {
    PREWARM_RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Fetches images to the cache on disk, so a page that shows them later has
/// no request to wait for. Each is decoded to check it and then dropped, so
/// none takes memory. A new
/// prewarm stops the one before it.
pub fn prewarm(urls: Vec<String>, cx: &mut App) {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    let agent = cx.global_mut::<ImageStore>().agent();
    let run = PREWARM_RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    cx.background_executor()
        .spawn(async move {
            let started = Instant::now();
            let wanted = urls.len();
            let outcome = prewarm_blocking(&agent, urls, disk_dir(), (&PREWARM_RUN, run), PREWARM_LIMITS);
            log::info!(
                "image prewarm: {} of {wanted} fetched in {} ms{}{}",
                outcome.fetched,
                started.elapsed().as_millis(),
                if outcome.failed > 0 { format!(", {} failed", outcome.failed) } else { String::new() },
                if outcome.stopped { ", stopped" } else { "" }
            );
        })
        .detach();
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PrewarmOutcome {
    fetched: usize,
    failed: usize,
    /// Ended before its list: offline, lost connections, or superseded.
    stopped: bool,
}

/// Fetches the images one after the other, lowest priority: one connection
/// beside the ones the pages use. Stops when the app is offline, after a
/// few lost connections in a row, and when `run` is no longer the number
/// in `current` (a newer prewarm started, or the session closed).
fn prewarm_blocking(
    agent: &ureq::Agent,
    mut urls: Vec<String>,
    cache: Option<&std::path::Path>,
    (current, run): (&std::sync::atomic::AtomicU64, u64),
    limits: PrewarmLimits,
) -> PrewarmOutcome {
    use std::sync::atomic::Ordering::Relaxed;
    urls.truncate(limits.max_urls);
    let mut outcome = PrewarmOutcome::default();
    let (mut lost_in_a_row, mut written, mut since_trim) = (0, 0u64, 0u64);
    for url in &urls {
        if crate::connection::is_offline()
            || lost_in_a_row >= PREWARM_GIVE_UP
            || current.load(Relaxed) != run
        {
            outcome.stopped = true;
            break;
        }
        let Some(file) = disk_path_in(cache, url) else { continue };
        if file.exists() {
            continue;
        }
        match fetch(agent, url, cache) {
            Ok(fetched) => {
                lost_in_a_row = 0;
                // The decode is the cost of a prewarm, on this thread; the
                // pixels are dropped at once, nothing stays in memory.
                if decodes(&fetched.bytes) {
                    fetched.commit();
                    outcome.fetched += 1;
                    written += fetched.bytes.len() as u64;
                } else {
                    outcome.failed += 1;
                }
            }
            Err(err) => {
                outcome.failed += 1;
                // An answer of the server (a 404) is not a lost connection.
                let answered = matches!(err.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(_)));
                lost_in_a_row = if answered { 0 } else { lost_in_a_row + 1 };
                log::debug!("prewarm of an image failed: {err:#}");
            }
        }
        if written - since_trim >= limits.trim_every {
            since_trim = written;
            trim_disk_cache_in(cache, limits.cache_max_bytes);
        }
    }
    // What came in counts against the limit of the cache.
    if written > since_trim {
        trim_disk_cache_in(cache, limits.cache_max_bytes);
    }
    outcome
}

fn decode_fetched(mime: &str, bytes: &[u8]) -> anyhow::Result<RenderImage> {
    decode_checked(mime, bytes).map(|(image, _)| image)
}

/// The image, and whether its bytes are whole ([`complete`]). A cut JPEG
/// still shows (the decoder fills it in), but it is not kept.
fn decode_checked(mime: &str, bytes: &[u8]) -> anyhow::Result<(RenderImage, bool)> {
    let format = ImageFormat::from_mime_type(mime)
        .or_else(|| sniff(bytes))
        .ok_or_else(|| anyhow::anyhow!("unknown image type {mime:?}"))?;
    Ok((decode(format, bytes)?, complete(format, bytes)))
}

fn download(agent: &ureq::Agent, url: &str) -> anyhow::Result<RenderImage> {
    download_in(agent, url, disk_dir())
}

/// Fetches and decodes an image. The bytes go to the cache on disk only
/// once they decoded whole ([`decodes`]). A file of the cache that does not
/// (kept by a run before this check, or damaged) goes, and the server is
/// asked once more.
fn download_in(agent: &ureq::Agent, url: &str, cache: Option<&std::path::Path>) -> anyhow::Result<RenderImage> {
    let started = Instant::now();
    let mut fetched = fetch(agent, url, cache)?;
    let fetch_took = started.elapsed();
    let mut result = decode_checked(&fetched.mime, &fetched.bytes);
    if !matches!(result, Ok((_, true)))
        && let Origin::Disk(file) = &fetched.origin
    {
        log::info!("image of the cache does not decode whole, fetched again");
        let _ = std::fs::remove_file(file);
        fetched = fetch(agent, url, cache)?;
        result = decode_checked(&fetched.mime, &fetched.bytes);
    }
    let (image, whole) = result?;
    if whole {
        fetched.commit();
    }
    let bytes = &fetched.bytes;
    if log::log_enabled!(log::Level::Debug) {
        let size = image.size(0);
        log::debug!(
            "image {}x{} {} KB: fetch {} ms, decode {} ms, {}",
            size.width.0,
            size.height.0,
            bytes.len() / 1024,
            fetch_took.as_millis(),
            (started.elapsed() - fetch_took).as_millis(),
            url.split('?').next().unwrap_or(url),
        );
    }
    Ok(image)
}

/// The most pixels a picture may have to be decoded: five times a 4K
/// backdrop, and 160 MB as RGBA. Artwork of a server is far below it.
const MAX_PIXELS: u64 = 40_000_000;

fn fits_in_memory(width: u32, height: u32) -> bool {
    width as u64 * height as u64 <= MAX_PIXELS
}

/// Decodes to the BGRA pixels GPUI paints. Only the first frame is kept; the
/// encoded bytes are dropped by the caller.
fn decode(format: ImageFormat, bytes: &[u8]) -> anyhow::Result<RenderImage> {
    let format = match format {
        ImageFormat::Svg => return decode_svg(bytes),
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Gif => image::ImageFormat::Gif,
        other => anyhow::bail!("unsupported image type {other:?}"),
    };
    let mut decoder = image::ImageReader::with_format(Cursor::new(bytes), format).into_decoder()?;
    // The pixels are allocated from the size the header names, before a
    // byte of them is read: a few bytes can name a picture of gigabytes.
    let (width, height) = image::ImageDecoder::dimensions(&decoder);
    anyhow::ensure!(
        fits_in_memory(width, height),
        "the picture is too large to decode ({width} by {height})"
    );
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let mut data = image.into_rgba8();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(RenderImage::new(SmallVec::from_elem(Frame::new(data), 1)))
}

/// Longest side of a drawn SVG, in pixels. Enough for a card at 2x scale.
const SVG_MAX_SIDE: f32 = 800.;

/// Reads an SVG into its tree, with the fonts of the system.
fn svg_tree(bytes: &[u8]) -> anyhow::Result<resvg::usvg::Tree> {
    use resvg::usvg;

    static FONTS: std::sync::OnceLock<Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let options = usvg::Options {
        fontdb: FONTS
            .get_or_init(|| {
                let mut fonts = usvg::fontdb::Database::new();
                fonts.load_system_fonts();
                Arc::new(fonts)
            })
            .clone(),
        // An SVG can carry or name raster pictures, and those are decoded
        // at their own size, whatever the size the SVG is drawn at: a small
        // file could ask for gigabytes. Artwork needs none; they are left out.
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    Ok(usvg::Tree::from_data(bytes, &options)?)
}

/// Draws an SVG to BGRA pixels, scaled so its longest side is `SVG_MAX_SIDE`.
fn decode_svg(bytes: &[u8]) -> anyhow::Result<RenderImage> {
    use resvg::tiny_skia;

    let tree = svg_tree(bytes)?;
    let size = tree.size();
    let scale = SVG_MAX_SIDE / size.width().max(size.height());
    let (width, height) = (
        (size.width() * scale).ceil().max(1.) as u32,
        (size.height() * scale).ceil().max(1.) as u32,
    );
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("svg of size {width}x{height}"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // The pixmap has premultiplied RGBA; GPUI wants straight BGRA.
    let mut data = pixmap.take();
    for pixel in data.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
        pixel.swap(0, 2);
    }
    let data = image::RgbaImage::from_raw(width, height, data)
        .ok_or_else(|| anyhow::anyhow!("svg pixel buffer"))?;
    Ok(RenderImage::new(SmallVec::from_elem(Frame::new(data), 1)))
}

fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    // An SVG is text: it starts with its tag or with an XML declaration.
    let head = &bytes[..bytes.len().min(256)];
    if let Ok(text) = std::str::from_utf8(head)
        && (text.trim_start().starts_with("<svg") || text.trim_start().starts_with("<?xml"))
    {
        return Some(ImageFormat::Svg);
    }
    match bytes {
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some(ImageFormat::Webp),
        [b'G', b'I', b'F', ..] => Some(ImageFormat::Gif),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::*;

    fn lru(max_bytes: usize) -> (Lru<u32>, Instant) {
        (Lru::new(max_bytes), Instant::now())
    }

    #[test]
    fn lru_evicts_least_recently_used_first() {
        let (mut cache, t0) = lru(30);
        assert!(cache.insert("a".into(), 1, 10, t0).is_empty());
        assert!(cache.insert("b".into(), 2, 10, t0 + Duration::from_secs(1)).is_empty());
        assert!(cache.insert("c".into(), 3, 10, t0 + Duration::from_secs(2)).is_empty());
        // "a" is the oldest entry until it is read again.
        assert_eq!(cache.get("a", t0 + Duration::from_secs(3)), Some(&1));
        let evicted = cache.insert("d".into(), 4, 10, t0 + Duration::from_secs(60));
        assert_eq!(evicted, vec![2]);
        assert_eq!(cache.bytes, 30);
        assert!(cache.entries.contains_key("a"));
        assert!(!cache.entries.contains_key("b"));
    }

    #[test]
    fn lru_evicts_until_under_budget() {
        let (mut cache, t0) = lru(30);
        for (i, key) in ["a", "b", "c"].into_iter().enumerate() {
            cache.insert(key.into(), i as u32, 10, t0 + Duration::from_secs(i as u64));
        }
        let mut evicted = cache.insert("big".into(), 9, 25, t0 + Duration::from_secs(60));
        evicted.sort();
        assert_eq!(evicted, vec![0, 1, 2]);
        assert_eq!(cache.bytes, 25);
    }

    #[test]
    fn lru_keeps_recently_used_entries_over_budget() {
        let (mut cache, t0) = lru(20);
        cache.insert("a".into(), 1, 10, t0);
        cache.insert("b".into(), 2, 10, t0);
        // Everything was used within KEEP_RECENT, so nothing can be dropped.
        assert!(cache.insert("c".into(), 3, 10, t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(cache.bytes, 30);
        // The overshoot is corrected by the next insert after the entries age.
        let mut evicted = cache.insert("d".into(), 4, 10, t0 + KEEP_RECENT);
        evicted.sort();
        assert_eq!(evicted, vec![1, 2]);
        assert_eq!(cache.bytes, 20);
    }

    #[test]
    fn lru_never_evicts_the_new_entry() {
        let (mut cache, t0) = lru(10);
        assert!(cache.insert("huge".into(), 1, 100, t0).is_empty());
        assert_eq!(cache.get("huge", t0), Some(&1));
    }

    #[test]
    fn lru_replacing_a_key_returns_the_old_value() {
        let (mut cache, t0) = lru(100);
        cache.insert("a".into(), 1, 10, t0);
        assert_eq!(cache.insert("a".into(), 2, 30, t0), vec![1]);
        assert_eq!(cache.bytes, 30);
    }

    #[test]
    fn decode_draws_svg_to_bgra_pixels() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
            <rect width="20" height="10" fill="#ff0000"/></svg>"##;
        assert_eq!(sniff(svg), Some(ImageFormat::Svg));
        let image = decode(ImageFormat::Svg, svg).unwrap();
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (800, 400));
        // Red, in BGRA order.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn retry_rule() {
        let s = Duration::from_secs;
        // Waits grow with the failures and stop at two minutes.
        assert_eq!(retry_delay(1), s(5));
        assert_eq!(retry_delay(2), s(30));
        assert_eq!(retry_delay(3), s(120));
        assert_eq!(retry_delay(40), s(120));
        assert!(!may_retry(1, s(4), true, false));
        assert!(may_retry(1, s(5), true, false));
        assert!(!may_retry(2, s(29), true, false));
        assert!(may_retry(2, s(30), true, false));
        assert!(!may_retry(5, s(119), true, false));
        assert!(may_retry(5, s(120), true, false));
        // A new session clears the wait.
        assert!(may_retry(5, s(0), true, true));
        // A 404 is never retried, not even after a new session.
        assert!(!may_retry(1, s(100_000), false, true));
    }

    #[test]
    fn which_errors_are_retried() {
        let err = |e: ureq::Error| anyhow::Error::from(e);
        assert!(!retryable(&err(ureq::Error::StatusCode(404))));
        assert!(!retryable(&err(ureq::Error::StatusCode(410))));
        assert!(retryable(&err(ureq::Error::StatusCode(500))));
        assert!(retryable(&err(ureq::Error::StatusCode(530))));
        assert!(retryable(&err(ureq::Error::Timeout(ureq::Timeout::Global))));
        assert!(!retryable(&anyhow::anyhow!("unknown image type")));
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
            std::thread::park();
        }
    }

    /// 40 fetches through the gate against a local server that counts open
    /// connections and requests in flight: at most `MAX_FETCHES` of each, and
    /// every image arrives.
    #[test]
    fn fetches_are_limited_and_all_arrive() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (open, peak_open) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (busy, peak_busy) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        {
            let (open, peak_open, busy, peak_busy) =
                (open.clone(), peak_open.clone(), busy.clone(), peak_busy.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let (open, peak_open, busy, peak_busy) =
                        (open.clone(), peak_open.clone(), busy.clone(), peak_busy.clone());
                    std::thread::spawn(move || {
                        peak_open.fetch_max(open.fetch_add(1, SeqCst) + 1, SeqCst);
                        let mut buf = [0u8; 4096];
                        // Keep-alive: answer requests until the client closes.
                        while let Ok(n) = stream.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            peak_busy.fetch_max(busy.fetch_add(1, SeqCst) + 1, SeqCst);
                            std::thread::sleep(Duration::from_millis(60));
                            let body = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"2\"/>";
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: {}\r\n\r\n",
                                body.len()
                            );
                            busy.fetch_sub(1, SeqCst);
                            if stream.write_all(head.as_bytes()).is_err() || stream.write_all(body).is_err() {
                                break;
                            }
                        }
                        open.fetch_sub(1, SeqCst);
                    });
                }
            });
        }
        let gate = Gate::new(MAX_FETCHES);
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(20)))
                .max_idle_connections(16)
                .max_idle_connections_per_host(8)
                .build(),
        );
        let handles: Vec<_> = (0..40)
            .map(|i| {
                let (gate, agent) = (gate.clone(), agent.clone());
                std::thread::spawn(move || {
                    let _permit = block_on(gate.acquire());
                    let url = format!("http://127.0.0.1:{port}/img/{i}");
                    fetch(&agent, &url, None).map(|f| f.bytes.len())
                })
            })
            .collect();
        for handle in handles {
            assert!(handle.join().unwrap().unwrap() > 0);
        }
        assert!(peak_busy.load(SeqCst) >= 2, "the test never ran in parallel");
        assert!(peak_busy.load(SeqCst) <= MAX_FETCHES, "requests: {}", peak_busy.load(SeqCst));
        assert!(peak_open.load(SeqCst) <= MAX_FETCHES, "connections: {}", peak_open.load(SeqCst));
        eprintln!("peak connections {}, peak requests {}", peak_open.load(SeqCst), peak_busy.load(SeqCst));
    }

    #[test]
    fn gate_serves_the_newest_waiter_first() {
        let gate = Gate::new(1);
        let first = block_on(gate.acquire());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut threads = Vec::new();
        for i in 0..3 {
            let (waiter, tx) = (gate.clone(), tx.clone());
            threads.push(std::thread::spawn(move || {
                let permit = block_on(waiter.acquire());
                tx.send(i).unwrap();
                drop(permit);
            }));
            // Each thread must be queued before the next one starts.
            while gate.state.lock().unwrap().waiting.len() < i + 1 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        drop(first);
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![2, 1, 0]);
    }

    /// A server for the cache tests: answers each request from `answers`
    /// in turn (content type and body), the last one again and again, and
    /// counts the requests. It ends with the test.
    struct ImageServer {
        port: u16,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl ImageServer {
        fn start(answers: Vec<(&'static str, Vec<u8>)>) -> Self {
            use std::io::{Read, Write};
            use std::sync::atomic::Ordering::SeqCst;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (count, stopping) = (requests.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                for mut stream in listener.incoming().flatten() {
                    if stopping.load(SeqCst) {
                        return;
                    }
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let n = count.fetch_add(1, SeqCst);
                    let (mime, body) = &answers[n.min(answers.len() - 1)];
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = stream.write_all(body);
                }
            });
            Self { port, requests, stop, thread: Some(thread) }
        }

        fn url(&self, item: &str) -> String {
            format!("http://127.0.0.1:{}/Items/{item}/Images/Primary?tag=t1", self.port)
        }

        fn requests(&self) -> usize {
            self.requests.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for ImageServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn png(side: u32) -> Vec<u8> {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(side, side, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn cache_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bloom-images-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn files_in(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    fn plain_agent() -> ureq::Agent {
        ureq::Agent::new_with_config(ureq::Agent::config_builder().build())
    }

    /// Finding 10 of the review: a 200 that is no image (a login page of a
    /// proxy) must not sit in the cache on disk for ever.
    #[test]
    fn a_bad_200_answer_does_not_poison_the_cache_on_disk() {
        let dir = cache_dir("html");
        let server = ImageServer::start(vec![
            ("text/html", b"<html><body>Please sign in</body></html>".to_vec()),
            ("image/png", png(2)),
        ]);
        let url = server.url("a");
        let agent = plain_agent();
        assert!(download_in(&agent, &url, Some(&dir)).is_err(), "a login page is no image");
        assert_eq!(files_in(&dir), 0, "the login page was kept");
        // The next ask, as after a restart of the app, gets the real image.
        let image = download_in(&agent, &url, Some(&dir)).expect("the real image after the bad answer");
        assert_eq!((image.size(0).width.0, image.size(0).height.0), (2, 2));
        assert_eq!(server.requests(), 2);
        // And it is in the cache now: a third ask needs no request.
        download_in(&agent, &url, Some(&dir)).unwrap();
        assert_eq!(server.requests(), 2);
        assert_eq!(files_in(&dir), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An image with a good header and a damaged body is not kept either:
    /// the bytes go to the disk only once they decoded.
    #[test]
    fn an_image_with_a_corrupt_body_is_not_kept() {
        let dir = cache_dir("corrupt");
        let mut damaged = png(8);
        let cut = damaged.len() / 2;
        damaged.truncate(cut);
        damaged.extend(std::iter::repeat_n(0xAAu8, cut));
        let server = ImageServer::start(vec![("image/png", damaged), ("image/png", png(8))]);
        let url = server.url("b");
        let agent = plain_agent();
        assert!(download_in(&agent, &url, Some(&dir)).is_err(), "a damaged image decoded");
        assert_eq!(files_in(&dir), 0, "the damaged image was kept");
        let image = download_in(&agent, &url, Some(&dir)).expect("the good image");
        assert_eq!(image.size(0).width.0, 8);
        assert_eq!(server.requests(), 2);
        download_in(&agent, &url, Some(&dir)).unwrap();
        assert_eq!(server.requests(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file the cache kept before this check (a login page under the
    /// name of an image) goes, and the image is fetched once more.
    #[test]
    fn a_poisoned_file_of_the_cache_is_replaced() {
        let dir = cache_dir("poisoned");
        let server = ImageServer::start(vec![("image/png", png(4))]);
        let url = server.url("c");
        let file = disk_path_in(Some(&dir), &url).unwrap();
        std::fs::write(&file, b"<html>Please sign in</html>").unwrap();
        let agent = plain_agent();
        let image = download_in(&agent, &url, Some(&dir)).expect("the image after the poisoned file");
        assert_eq!(image.size(0).width.0, 4);
        assert_eq!(server.requests(), 1);
        assert_eq!(std::fs::read(&file).unwrap(), png(4), "the cache still has the old file");
        download_in(&agent, &url, Some(&dir)).unwrap();
        assert_eq!(server.requests(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A prewarm, which does not decode, keeps an image and not a page.
    #[test]
    fn a_prewarm_keeps_images_only() {
        let dir = cache_dir("prewarm");
        let server = ImageServer::start(vec![
            ("text/html", b"<html>Please sign in</html>".to_vec()),
            ("image/png", png(2)),
        ]);
        let urls = vec![server.url("page"), server.url("image")];
        let outcome = prewarm_blocking(&plain_agent(), urls.clone(), Some(&dir), (&AtomicU64::new(0), 0), PREWARM_LIMITS);
        assert_eq!(outcome, PrewarmOutcome { fetched: 1, failed: 1, stopped: false });
        assert_eq!(files_in(&dir), 1);
        assert!(disk_path_in(Some(&dir), &urls[1]).unwrap().exists());
        assert!(!disk_path_in(Some(&dir), &urls[0]).unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A prewarm against a server that is gone stops after a few lost
    /// connections instead of waiting out a timeout for every poster.
    #[test]
    fn a_prewarm_gives_up_when_the_connections_are_lost() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        std::thread::spawn(move || {
            // Every connection is closed without an answer; the thread ends
            // with the listener, when the test process does.
            for stream in listener.incoming().flatten() {
                count.fetch_add(1, SeqCst);
                drop(stream);
            }
        });
        let dir = cache_dir("giveup");
        let urls: Vec<String> = (0..20).map(|i| format!("http://127.0.0.1:{port}/i/{i}?tag=t")).collect();
        let outcome = prewarm_blocking(&plain_agent(), urls, Some(&dir), (&AtomicU64::new(0), 0), PREWARM_LIMITS);
        assert_eq!(outcome, PrewarmOutcome { fetched: 0, failed: PREWARM_GIVE_UP as usize, stopped: true }, "requests: {}", requests.load(SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A prewarm stops between two fetches when a newer one started, or
    /// the session closed: the server moves the number on from inside the
    /// third request, as `prewarm` and `cancel_prewarm` do.
    #[test]
    fn a_prewarm_stops_when_it_is_superseded() {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering::SeqCst;
        let dir = cache_dir("cancel");
        let current = Arc::new(AtomicU64::new(7));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (moved, body) = (current.clone(), png(2));
        let server = std::thread::spawn(move || {
            // Three requests, then the listener goes.
            for (n, mut stream) in listener.incoming().flatten().take(3).enumerate() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                if n == 2 {
                    moved.fetch_add(1, SeqCst);
                }
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(&body);
            }
        });
        let urls: Vec<String> = (0..10).map(|i| format!("http://127.0.0.1:{port}/Items/i{i}/Images/Primary?tag=t1")).collect();
        let outcome = prewarm_blocking(&plain_agent(), urls.clone(), Some(&dir), (&current, 7), PREWARM_LIMITS);
        // The third image was on its way and is kept; the fourth is not asked for.
        assert_eq!(outcome, PrewarmOutcome { fetched: 3, failed: 0, stopped: true });
        assert_eq!(files_in(&dir), 3);
        server.join().unwrap();
        // The run with the old number does nothing more.
        let outcome = prewarm_blocking(&plain_agent(), urls, Some(&dir), (&current, 7), PREWARM_LIMITS);
        assert_eq!(outcome, PrewarmOutcome { fetched: 0, failed: 0, stopped: true });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `prewarm` and `cancel_prewarm` move the number of the app on: the
    /// run before them is not the current one any more.
    #[test]
    fn a_new_prewarm_and_a_closed_session_move_the_number_on() {
        let before = PREWARM_RUN.load(std::sync::atomic::Ordering::Relaxed);
        cancel_prewarm();
        assert!(PREWARM_RUN.load(std::sync::atomic::Ordering::Relaxed) > before);
    }

    /// A long prewarm trims the cache as it goes: the folder is never more
    /// than one trim interval and one image over its limit, also before
    /// the end of the run. The server looks at the folder at each request.
    #[test]
    fn a_long_prewarm_keeps_the_cache_near_its_limit_all_the_way() {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering::SeqCst;
        let dir = cache_dir("trimrun");
        let body = png(64);
        let size = body.len() as u64;
        let limits = PrewarmLimits { max_urls: 100, trim_every: 3 * size, cache_max_bytes: 10 * size };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let largest = Arc::new(AtomicU64::new(0));
        let (seen, folder, answer) = (largest.clone(), dir.clone(), body.clone());
        let server = std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten().take(60) {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let total: u64 = std::fs::read_dir(&folder)
                    .unwrap()
                    .flatten()
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum();
                seen.fetch_max(total, SeqCst);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", answer.len());
                let _ = stream.write_all(&answer);
            }
        });
        let urls: Vec<String> = (0..60).map(|i| format!("http://127.0.0.1:{port}/Items/t{i}/Images/Primary?tag=t1")).collect();
        let outcome = prewarm_blocking(&plain_agent(), urls, Some(&dir), (&AtomicU64::new(0), 0), limits);
        server.join().unwrap();
        assert_eq!(outcome, PrewarmOutcome { fetched: 60, failed: 0, stopped: false });
        let largest = largest.load(SeqCst);
        assert!(largest > 0);
        assert!(
            largest <= limits.cache_max_bytes + limits.trim_every,
            "the folder reached {largest} bytes during the run; the limit is {}",
            limits.cache_max_bytes
        );
        assert!(files_in(&dir) as u64 * size <= limits.cache_max_bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The artwork of a download goes to the cache under each URL the
    /// pages ask for, as a copy of the one file (a clone on APFS), and a
    /// page then gets it from the cache without a request.
    #[test]
    fn the_artwork_of_a_download_is_seeded_from_its_file() {
        let dir = cache_dir("seed");
        let poster = dir.join("poster-of-the-download");
        std::fs::write(&poster, png(4)).unwrap();
        let urls: Vec<String> = [240, 320, 400].iter().map(|w| format!("http://127.0.0.1:9/Items/s/Images/Primary?tag=t1&fillWidth={w}")).collect();
        for url in &urls {
            seed_file_in(Some(&dir), url, &poster);
        }
        assert_eq!(files_in(&dir), 4, "one file per width beside the poster, and no partial file");
        for url in &urls {
            // Nothing listens on the port: the image comes from the cache.
            let image = download_in(&plain_agent(), url, Some(&dir)).expect("the seeded image");
            assert_eq!(image.size(0).width.0, 4);
        }
        // A URL the cache does not keep (no tag) gets no file.
        seed_file_in(Some(&dir), "http://127.0.0.1:9/Items/s/Images/Primary", &poster);
        assert_eq!(files_in(&dir), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bytes for the Now Playing tile and the preload: a page under
    /// the name of an image in the cache goes, and a page of the network
    /// is not kept.
    #[test]
    fn the_bytes_of_an_image_do_not_come_from_a_poisoned_file() {
        let dir = cache_dir("bytes");
        let server = ImageServer::start(vec![
            ("image/png", png(4)),
            ("text/html", b"<html>Please sign in</html>".to_vec()),
        ]);
        let agent = plain_agent();
        let url = server.url("d");
        let file = disk_path_in(Some(&dir), &url).unwrap();
        std::fs::write(&file, b"<html>Please sign in</html>").unwrap();
        assert_eq!(fetch_bytes_in(&agent, &url, Some(&dir)).unwrap(), png(4));
        assert_eq!(std::fs::read(&file).unwrap(), png(4));
        assert_eq!(server.requests(), 1);
        // A page of the network for another image is not kept.
        let other = server.url("e");
        let _ = fetch_bytes_in(&agent, &other, Some(&dir));
        assert!(!disk_path_in(Some(&dir), &other).unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real picture of a format, big enough that its body is most of the
    /// file: a gradient with some noise, so the encoder has data to keep.
    fn picture(format: image::ImageFormat) -> Vec<u8> {
        let image = image::RgbImage::from_fn(96, 96, |x, y| {
            let noise = (x * 31 + y * 17) % 23;
            image::Rgb([(x * 2 + noise) as u8, (y * 2) as u8, ((x ^ y) * 3) as u8])
        });
        let mut bytes = Vec::new();
        image.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
        bytes
    }

    /// The same picture cut off after a third of its data: the header
    /// parses (the old check passed it) and the body is not whole. A cut
    /// is the damage of a lost connection, and the one every decoder here
    /// can find (a JPEG by its missing end marker, see [`complete`]).
    fn damaged(format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = picture(format);
        bytes.truncate(bytes.len() / 3);
        bytes
    }

    const FORMATS: [(&str, image::ImageFormat); 3] = [
        ("png", image::ImageFormat::Png),
        ("jpeg", image::ImageFormat::Jpeg),
        ("webp", image::ImageFormat::WebP),
    ];

    /// The damaged pictures are what the test says they are: the header
    /// parses (the old check passed them) and the decode fails.
    #[test]
    fn the_damaged_pictures_have_a_good_header_and_a_bad_body() {
        for (name, format) in FORMATS {
            let (good, bad) = (picture(format), damaged(format));
            assert!(decodes(&good), "{name}: the good picture does not decode");
            let kind = sniff(&bad).unwrap_or_else(|| panic!("{name}: no magic bytes"));
            let reader = |bytes: &[u8]| image::ImageReader::with_format(Cursor::new(bytes.to_vec()), format);
            assert!(reader(&bad).into_dimensions().is_ok(), "{name}: the header of the damaged picture does not parse");
            // The decoder of JPEG is lenient and fills a cut file in; the
            // others fail. Either way the picture is not whole.
            assert_eq!(decode(kind, &bad).is_err(), name != "jpeg", "{name}");
            assert!(!complete(kind, &bad) || name != "jpeg", "{name}: a cut JPEG counts as whole");
            assert!(!decodes(&bad), "{name}: the damaged picture counts as whole");
        }
    }

    /// A prewarm keeps no image whose body is damaged, and keeps a good
    /// one. On 0.1.2 the header was the check and the damaged one stayed.
    #[test]
    fn a_prewarm_does_not_keep_an_image_with_a_damaged_body() {
        for (name, format) in FORMATS {
            let dir = cache_dir(&format!("prewarm-damaged-{name}"));
            let server = ImageServer::start(vec![("image/x", damaged(format)), ("image/x", picture(format))]);
            let urls = vec![server.url("bad"), server.url("good")];
            let outcome =
                prewarm_blocking(&plain_agent(), urls.clone(), Some(&dir), (&AtomicU64::new(0), 0), PREWARM_LIMITS);
            assert_eq!(outcome, PrewarmOutcome { fetched: 1, failed: 1, stopped: false }, "{name}");
            assert!(!disk_path_in(Some(&dir), &urls[0]).unwrap().exists(), "{name}: the damaged image was kept");
            assert_eq!(
                std::fs::read(disk_path_in(Some(&dir), &urls[1]).unwrap()).unwrap(),
                picture(format),
                "{name}: the good image is not in the cache"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The raw bytes of an image: a damaged one from the network is not
    /// kept, and a good one is.
    #[test]
    fn the_raw_bytes_of_a_damaged_image_are_not_kept() {
        for (name, format) in FORMATS {
            let dir = cache_dir(&format!("raw-damaged-{name}"));
            let server = ImageServer::start(vec![("image/x", damaged(format)), ("image/x", picture(format))]);
            let agent = plain_agent();
            let (bad, good) = (server.url("bad"), server.url("good"));
            let _ = fetch_bytes_in(&agent, &bad, Some(&dir));
            assert!(!disk_path_in(Some(&dir), &bad).unwrap().exists(), "{name}: the damaged image was kept");
            assert_eq!(fetch_bytes_in(&agent, &good, Some(&dir)).unwrap(), picture(format), "{name}");
            assert_eq!(
                std::fs::read(disk_path_in(Some(&dir), &good).unwrap()).unwrap(),
                picture(format),
                "{name}: the good image is not in the cache"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A file of the cache with a good header and a damaged body (kept by
    /// 0.1.2) is not given out by a raw read: it goes, and the image is
    /// fetched once more and kept.
    #[test]
    fn a_damaged_file_of_the_cache_is_replaced_on_a_raw_read() {
        for (name, format) in FORMATS {
            let dir = cache_dir(&format!("raw-poisoned-{name}"));
            let server = ImageServer::start(vec![("image/x", picture(format))]);
            let url = server.url("p");
            let file = disk_path_in(Some(&dir), &url).unwrap();
            std::fs::write(&file, damaged(format)).unwrap();
            let bytes = fetch_bytes_in(&plain_agent(), &url, Some(&dir)).unwrap();
            assert_eq!(bytes, picture(format), "{name}: the raw read gave the damaged file");
            assert_eq!(std::fs::read(&file).unwrap(), picture(format), "{name}: the file was not replaced");
            assert_eq!(server.requests(), 1, "{name}");
            // And now the file is good: no second request.
            fetch_bytes_in(&plain_agent(), &url, Some(&dir)).unwrap();
            assert_eq!(server.requests(), 1, "{name}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The path of the loader still replaces such a file (it did before).
    #[test]
    fn a_damaged_file_of_the_cache_is_replaced_on_a_decoding_read() {
        for (name, format) in FORMATS {
            let dir = cache_dir(&format!("decode-poisoned-{name}"));
            let server = ImageServer::start(vec![("image/x", picture(format))]);
            let url = server.url("q");
            let file = disk_path_in(Some(&dir), &url).unwrap();
            std::fs::write(&file, damaged(format)).unwrap();
            download_in(&plain_agent(), &url, Some(&dir)).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(std::fs::read(&file).unwrap(), picture(format), "{name}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An SVG is kept when it draws and not when it does not parse.
    /// A header can name a picture of gigabytes in a few bytes, and the
    /// pixels are allocated from the header: such a picture is refused
    /// before the allocation, on every path that decodes.
    #[test]
    fn a_picture_too_large_for_memory_is_not_decoded() {
        assert!(fits_in_memory(3840, 2160));
        assert!(fits_in_memory(6000, 6000));
        assert!(!fits_in_memory(8000, 6000));
        assert!(!fits_in_memory(u32::MAX, u32::MAX));
        // A real file: one grey value, 48 megapixels, a few kilobytes.
        let big = image::GrayImage::new(8000, 6000);
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(big)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        assert!(bytes.len() < 1_000_000, "the test file is small next to its 48 MB of pixels: {}", bytes.len());
        let err = decode(ImageFormat::Png, &bytes).err().expect("the picture was decoded");
        assert!(format!("{err:#}").contains("too large"), "{err:#}");
        assert!(!decodes(&bytes), "a picture that is not decoded is not kept");
    }

    /// A raster picture inside an SVG is decoded at its own size by the
    /// SVG library, past the limit of `decode`: it is not loaded at all.
    #[test]
    fn a_picture_inside_an_svg_is_not_decoded() {
        let big = image::GrayImage::new(8000, 6000);
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(big).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><rect width="10" height="10" fill="red"/><image width="10" height="10" xlink:href="data:image/png;base64,{data}"/></svg>"#
        );
        use resvg::usvg;
        let tree = svg_tree(svg.as_bytes()).unwrap();
        fn has_image(group: &usvg::Group) -> bool {
            group.children().iter().any(|node| match node {
                usvg::Node::Image(_) => true,
                usvg::Node::Group(inner) => has_image(inner),
                _ => false,
            })
        }
        assert!(!has_image(tree.root()), "the picture inside the SVG was loaded");
        // And the SVG itself still draws.
        assert!(decode(ImageFormat::Svg, svg.as_bytes()).is_ok());
    }

    #[test]
    fn an_svg_is_kept_when_it_draws() {
        let good = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#;
        let bad = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10""#;
        assert!(decodes(good));
        assert!(!decodes(bad), "a cut SVG was accepted");
        assert!(!decodes(b"<html>Please sign in</html>"));
    }

    /// A prewarm takes the first `PREWARM_MAX_URLS` of its list and no
    /// more; and the trim removes the least recently used files first.
    #[test]
    fn a_prewarm_is_capped_and_the_trim_takes_the_oldest_files() {
        let dir = cache_dir("cap");
        let server = ImageServer::start(vec![("image/png", png(2))]);
        // The cap: one URL more than the cap, each with a cache file already
        // there but the last one, which must not be fetched.
        let urls: Vec<String> = (0..=PREWARM_MAX_URLS).map(|i| server.url(&format!("c{i}"))).collect();
        for url in &urls[..PREWARM_MAX_URLS] {
            std::fs::write(disk_path_in(Some(&dir), url).unwrap(), png(2)).unwrap();
        }
        let outcome = prewarm_blocking(&plain_agent(), urls, Some(&dir), (&AtomicU64::new(0), 0), PREWARM_LIMITS);
        assert_eq!(outcome, PrewarmOutcome::default());
        assert_eq!(server.requests(), 0);
        // The trim: over the limit, the least recently used files go.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = disk_path_in(Some(&dir), &server.url("old")).unwrap();
        std::fs::write(&old, vec![0u8; 1000]).unwrap();
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(hour_ago).unwrap();
        for i in 0..3 {
            std::fs::write(disk_path_in(Some(&dir), &server.url(&format!("new{i}"))).unwrap(), vec![0u8; 400]).unwrap();
        }
        trim_disk_cache_in(Some(&dir), 1500);
        assert!(!old.exists(), "the oldest file stayed");
        assert_eq!(files_in(&dir), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_produces_bgra_pixels() {
        let mut png = Vec::new();
        let pixels = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
        pixels
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let image = decode(ImageFormat::Png, &png).unwrap();
        let bytes = image.as_bytes(0).unwrap();
        assert_eq!(bytes.len(), 3 * 2 * 4);
        assert_eq!(&bytes[..4], &[30, 20, 10, 255]);
    }
}
