// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Remote image cache. The native GPUI platform ships without an HTTP client,
//! so artwork is fetched and decoded by the loader of the core crate
//! (`core/src/images.rs`) and handed to GPUI here as ready pixels.
//! Decoded pixels are the real cost (a 300x450 poster is 540 KB, plus the same
//! again in the GPU atlas), so the cache is bounded by decoded bytes.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bloom_core::images::{self as loader, Bgra, Lru, gate, may_retry, retry_delay, retryable};
use gpui_kit::{
    App, Bounds, Canvas, Corners, Global, ObjectFit, Pixels, RenderImage, Window, canvas, px,
};
use image::Frame;
use smallvec::SmallVec;

pub use loader::{fetch_bytes, looks_like_image, pixels, seed_file, set_local_image};

/// Decoded bytes kept before the least recently shown images are dropped.
const MAX_BYTES: usize = 64 * 1024 * 1024;

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
        std::thread::spawn(loader::trim_disk_cache);
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
        self.agent.get_or_insert_with(loader::new_agent).clone()
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
                    Some(bytes) => loader::decode_fetched("", &bytes),
                    None => loader::download(&agent, &target),
                }
                .map(render)
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
                    Some(bytes) => loader::decode_fetched("", &bytes),
                    None => loader::download(&agent, &target),
                }
                .map(render)
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

/// The image GPUI paints, from the pixels of the loader.
fn render(pixels: Bgra) -> RenderImage {
    RenderImage::new(SmallVec::from_elem(Frame::new(pixels), 1))
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
    Some(render(Bgra::from_raw(w, h, data)?))
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
    remote_corners(url, Corners::all(radius), fit)
}

/// `remote_with` with a radius for each corner, for an image that fills
/// the top of a card with round corners: `overflow_hidden` clips to the
/// rectangle of the card, not to its corners.
pub fn remote_corners(url: String, corners: Corners<Pixels>, fit: ObjectFit) -> Canvas<()> {
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
            let _ = window.paint_image(bounds, target, corners, image, 0, false);
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

/// The HTTP agent of the cache, for a fetch of its own (see [`fetch_bytes`]).
pub fn agent(cx: &mut App) -> ureq::Agent {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::new());
    }
    cx.global_mut::<ImageStore>().agent()
}

/// Counts the prewarms started. A run checks it between two fetches and
/// stops when a newer one was started, or [`cancel_prewarm`] was called.
static PREWARM_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The counter of the prewarms. The app has one. A test has one for its
/// own thread: the tests run side by side, each with an app of its own,
/// and a session that closes in one stopped the prewarm of another.
fn prewarm_run() -> &'static std::sync::atomic::AtomicU64 {
    #[cfg(test)]
    {
        thread_local! {
            static OWN: &'static std::sync::atomic::AtomicU64 =
                Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
        }
        let _ = &PREWARM_COUNT;
        OWN.with(|own| *own)
    }
    #[cfg(not(test))]
    &PREWARM_COUNT
}

/// Stops the prewarm under way, at its next fetch: the session it was for
/// is gone. For the place a session closes without a new one.
pub fn cancel_prewarm() {
    prewarm_run().fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    let run = prewarm_run().fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    cx.background_executor()
        .spawn(async move {
            let started = Instant::now();
            let wanted = urls.len();
            let outcome = loader::prewarm(&agent, urls, prewarm_run(), run);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `prewarm` and `cancel_prewarm` move the number of the app on: the
    /// run before them is not the current one any more.
    #[test]
    fn a_new_prewarm_and_a_closed_session_move_the_number_on() {
        let before = prewarm_run().load(std::sync::atomic::Ordering::Relaxed);
        cancel_prewarm();
        assert!(prewarm_run().load(std::sync::atomic::Ordering::Relaxed) > before);
    }

    /// A tile is the right part of its sheet: each tile of a 2 by 2 sheet
    /// has the one colour that was put there.
    #[test]
    fn a_tile_is_cut_from_its_place_in_the_sheet() {
        let sheet = render(Bgra::from_fn(4, 6, |x, y| image::Rgba([(x / 2) as u8, (y / 3) as u8, 0, 255])));
        for (column, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let tile = crop(&sheet, (2, 2), (column, row)).expect("a tile");
            assert_eq!((tile.size(0).width.0, tile.size(0).height.0), (2, 3));
            let pixels = tile.as_bytes(0).unwrap();
            assert!(pixels.chunks_exact(4).all(|p| p == [column as u8, row as u8, 0, 255]), "{column},{row}");
        }
        assert!(crop(&sheet, (2, 2), (2, 0)).is_none(), "a tile past the last column");
    }
}
