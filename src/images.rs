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
    Failed,
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
            let fetched = cx
                .background_executor()
                .spawn(async move { fetch(&agent, &target) })
                .await;
            cx.update(|cx| {
                let store = cx.global_mut::<ImageStore>();
                if store.raw_epoch != epoch {
                    return;
                }
                match fetched {
                    Ok((_, bytes)) => {
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
    if store.slots.contains_key(url) {
        return None;
    }
    store.slots.insert(url.to_string(), Slot::Loading);
    let agent = store.agent();
    let raw = store.raw.get(url).cloned();
    let key = url.to_string();
    let target = url.to_string();
    cx.spawn(async move |cx| {
        let fetched = cx
            .background_executor()
            .spawn(async move {
                match raw {
                    Some(bytes) => decode_fetched("", &bytes),
                    None => download(&agent, &target),
                }
            })
            .await;
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
                    store.slots.insert(key, Slot::Failed);
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
    if store.slots.contains_key(url) {
        return None;
    }
    store.slots.insert(url.to_string(), Slot::Loading);
    let agent = store.agent();
    let raw = store.raw.get(url).cloned();
    let key = url.to_string();
    let target = url.to_string();
    cx.spawn(async move |cx| {
        let fetched = cx
            .background_executor()
            .spawn(async move {
                match raw {
                    Some(bytes) => decode_fetched("", &bytes),
                    None => download(&agent, &target),
                }
            })
            .await;
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
                    store.slots.insert(key, Slot::Failed);
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

/// The content type and the bytes of an image, from the cache on disk when
/// the image is there, else from the network (and then to the disk).
fn fetch(agent: &ureq::Agent, url: &str) -> anyhow::Result<(String, Vec<u8>)> {
    // A file of this Mac, such as the poster of a download.
    if let Some(path) = url.strip_prefix("file://") {
        return Ok((String::new(), std::fs::read(path)?));
    }
    let file = disk_path(url);
    if let Some(bytes) = file.as_deref().and_then(disk_read) {
        return Ok((String::new(), bytes));
    }
    // The artwork of a download is on this Mac, server or no server.
    if let Some(path) = crate::downloads::local_image(url)
        && let Ok(bytes) = std::fs::read(path)
    {
        return Ok((String::new(), bytes));
    }
    let mut response = agent.get(url).call()?;
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    let bytes = response.body_mut().read_to_vec()?;
    if let Some(file) = &file {
        disk_write(file, &bytes);
    }
    Ok((mime, bytes))
}

// ----- cache on disk --------------------------------------------------------

/// Encoded images kept on disk before the least recently used are removed.
const DISK_MAX_BYTES: u64 = 500 * 1024 * 1024;

/// Folder of the image files: `~/Library/Caches/bloom/images` on macOS.
/// `BLOOM_NO_DISK_CACHE` switches the cache off, for measurements.
fn disk_dir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if std::env::var_os("BLOOM_NO_DISK_CACHE").is_some() {
            return None;
        }
        let dir = dirs::cache_dir()?.join(crate::brand::FOLDER).join("images");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_deref()
}

/// File of an image, when the image may be kept. A Jellyfin image whose URL
/// has a tag never changes under that URL, and neither does a TMDB image.
/// The preview sheets of the player are left out: they are large and one
/// playback uses them.
fn disk_path(url: &str) -> Option<std::path::PathBuf> {
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
    Some(disk_dir()?.join(format!(
        "{:016x}{:016x}",
        hash(0xcbf2_9ce4_8422_2325),
        hash(0x8422_2325_cbf2_9ce4)
    )))
}

/// Puts an image in the cache on disk ahead of a request for it, so a page
/// shows it without the server. The downloads do this for their artwork.
pub fn seed(url: &str, bytes: &[u8]) {
    if let Some(file) = disk_path(url)
        && !file.exists()
    {
        disk_write(&file, bytes);
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
    let Some(dir) = disk_dir() else { return };
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
    if total <= DISK_MAX_BYTES {
        return;
    }
    files.sort();
    for (_, size, path) in files {
        // Some room is left, so the next few images do not start this again.
        if total <= DISK_MAX_BYTES / 10 * 9 {
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
    fetch(agent, url).map(|(_, bytes)| bytes)
}

/// Fetches images to the cache on disk, so a page that shows them later has
/// no request to wait for. They are not decoded and take no memory.
pub fn prewarm(urls: Vec<String>, cx: &mut App) {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    let agent = cx.global_mut::<ImageStore>().agent();
    cx.background_executor()
        .spawn(async move {
            let started = Instant::now();
            let mut fetched = 0;
            for url in &urls {
                let Some(file) = disk_path(url) else { continue };
                if file.exists() {
                    continue;
                }
                match fetch(&agent, url) {
                    Ok(_) => fetched += 1,
                    Err(err) => log::debug!("prewarm of an image failed: {err:#}"),
                }
            }
            log::info!(
                "image prewarm: {fetched} of {} fetched in {} ms",
                urls.len(),
                started.elapsed().as_millis()
            );
        })
        .detach();
}

fn decode_fetched(mime: &str, bytes: &[u8]) -> anyhow::Result<RenderImage> {
    let format = ImageFormat::from_mime_type(mime)
        .or_else(|| sniff(bytes))
        .ok_or_else(|| anyhow::anyhow!("unknown image type {mime:?}"))?;
    decode(format, bytes)
}

fn download(agent: &ureq::Agent, url: &str) -> anyhow::Result<RenderImage> {
    let started = Instant::now();
    let (mime, bytes) = fetch(agent, url)?;
    let fetched = started.elapsed();
    let image = decode_fetched(&mime, &bytes)?;
    if log::log_enabled!(log::Level::Debug) {
        let size = image.size(0);
        log::debug!(
            "image {}x{} {} KB: fetch {} ms, decode {} ms, {}",
            size.width.0,
            size.height.0,
            bytes.len() / 1024,
            fetched.as_millis(),
            (started.elapsed() - fetched).as_millis(),
            url.split('?').next().unwrap_or(url),
        );
    }
    Ok(image)
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

/// Draws an SVG to BGRA pixels, scaled so its longest side is `SVG_MAX_SIDE`.
fn decode_svg(bytes: &[u8]) -> anyhow::Result<RenderImage> {
    use resvg::{tiny_skia, usvg};

    static FONTS: std::sync::OnceLock<Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let options = usvg::Options {
        fontdb: FONTS
            .get_or_init(|| {
                let mut fonts = usvg::fontdb::Database::new();
                fonts.load_system_fonts();
                Arc::new(fonts)
            })
            .clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options)?;
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
