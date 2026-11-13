// SPDX-License-Identifier: AGPL-3.0-or-later
//! Downloads for offline use: the engine that fetches the files, the pages
//! and buttons that drive it, and the offline mode of the app when the
//! server cannot be reached.
//!
//! The files live under `~/Library/Application Support/bloom/downloads/`
//! (`BLOOM_DOWNLOAD_DIR` for tests). A complete file plays in place of
//! the stream: see [`local_source`], called where the player gets its URL.

mod debug;
mod ui;

pub use bloom_core::downloads::{dir, engine, offline};

use bloom_core::downloads::ENGINE;

use std::{
    cell::RefCell,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui_kit::{Context, Task, Window};

pub use engine::{Engine, Entry, EntryState, Key, format_bytes};

use crate::{
    app::{Bloom, Page},
    jellyfin::{Client, Item},
    player::Player,
};

/// The engine of this process; opened on first use.
pub fn engine() -> &'static Engine {
    ENGINE.get_or_init(|| {
        // The engine keeps artwork that the image loader can show, and
        // puts it in the loader's cache.
        let artwork = engine::Artwork { is_image: crate::images::looks_like_image, seed: crate::images::seed_file };
        Engine::open(folder(), engine::Options { artwork, ..Default::default() })
    })
}

/// The folder of the engine of this process. The tests get one of their
/// own: they open sessions with mock servers, and the engine of the user's
/// folder would take such a server for the one of his downloads.
fn folder() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("bloom-test-downloads-{}", std::process::id()))
    }
    #[cfg(not(test))]
    dir()
}

/// The complete local file of an item of the server `server_id`, when
/// there is one; the file of another server with the same item id does not
/// count. Cheap; it reads the index in memory and asks the disk once.
pub fn local_path(server_id: Option<&str>, item_id: &str) -> Option<PathBuf> {
    ENGINE.get()?.local_path(Key::of(server_id, item_id))
}

/// The source the player loads for an item: its local file when the item
/// is downloaded, with its external subtitles beside it, else `None` and
/// the caller uses the stream. This is the one hook of playback.
pub fn local_source(player: &Player, server_id: Option<&str>, item_id: &str) -> Option<String> {
    let path = local_path(server_id, item_id);
    let subtitles: Vec<String> = path
        .as_ref()
        .and_then(|p| p.parent())
        .and_then(|folder| ENGINE.get()?.meta(Key::of(server_id, item_id)).map(|meta| (folder.to_path_buf(), meta)))
        .map(|(folder, meta)| {
            meta["Subtitles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["File"].as_str().filter(|f| engine::check_file_name(f).is_ok()))
                .map(|f| folder.join(f).display().to_string())
                .collect()
        })
        .unwrap_or_default();
    // A stream must not get the subtitles of the item before. An empty
    // string is not an empty list to mpv (it tried to open '' at each
    // load), so the list is cleared as one.
    if subtitles.is_empty() {
        player.command("change-list", &["sub-files", "clr", ""]);
    } else {
        player.set_property("sub-files", &subtitles.join(":"));
    }
    path.map(|p| p.display().to_string())
}

/// The intro and credits ranges that `meta.json` of a downloaded item
/// holds. `None` when the item has no local file or the file has no range;
/// the caller then asks the server.
pub fn stored_segments(server_id: Option<&str>, item_id: &str) -> Option<Vec<crate::jellyfin::MediaSegment>> {
    local_path(server_id, item_id)?;
    segments_of_meta(&ENGINE.get()?.meta(Key::of(server_id, item_id))?)
}

/// The `Segments` array of a `meta.json`: the `Items` of `/MediaSegments`.
fn segments_of_meta(meta: &serde_json::Value) -> Option<Vec<crate::jellyfin::MediaSegment>> {
    let segments: Vec<crate::jellyfin::MediaSegment> =
        serde_json::from_value(meta.get("Segments")?.clone()).ok()?;
    (!segments.is_empty()).then_some(segments)
}

/// Downloads as the app holds them: the offline mode and the task that
/// redraws for progress.
#[derive(Default)]
pub struct DownloadsState {
    /// The server cannot be reached: the pages do not ask it, and the
    /// Downloads page is the home.
    pub offline: bool,
    /// `EnableContentDownloading` of the user; unknown until the server
    /// answered.
    pub allowed: Option<bool>,
    poll: Option<Task<()>>,
    /// What the Downloads page lists; see [`ui::Listing`].
    listing: RefCell<ui::Listing>,
    /// What the notice "in the queue" counts: the items queued since the
    /// notice came up, and when the last one came.
    queued_notice: Option<(Instant, usize)>,
    /// The real client of the session while a test points the session at
    /// an address that answers nothing (`BLOOM_OFFLINE`, `downloads
    /// offline on`).
    online_client: Option<Client>,
}

impl DownloadsState {
    /// True while a test points the session at a dead address.
    pub fn is_unreachable_simulated(&self) -> bool {
        self.online_client.is_some()
    }
}

/// How long the notice "in the queue" stays, and so how long it counts on.
const QUEUED_NOTICE: Duration = Duration::from_secs(6);

/// The words of the notice: the title for one item, the count for more.
fn queued_text(count: usize, title: Option<&str>) -> String {
    match (count, title) {
        (1, Some(title)) => format!("{title} is in the queue."),
        (1, None) => "1 item is in the queue.".to_string(),
        (count, _) => format!("{count} items are in the queue."),
    }
}

/// Progress reaches the UI this often at most: the wait after a frame
/// that a change of the engine made.
const POLL_EVERY: Duration = Duration::from_millis(250);

/// The word when another instance holds the download folder.
const NOT_OWNER_TEXT: &str = "Another window of the app holds the download folder; downloads are off in this one.";

impl Bloom {
    /// Gives the engine the session and starts the poll. Called when a
    /// session opens.
    pub fn start_downloads(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        let engine = engine();
        let (parallel, limit) = (self.config.download_parallel, self.config.download_limit_gb);
        engine.set_options(move |o| {
            o.parallel = parallel.unwrap_or(1).clamp(1, 2) as usize;
            o.limit_bytes = limit.map(|gb| gb.saturating_mul(1_000_000_000));
        });
        self.downloads.allowed = None;
        self.downloads.offline = false;
        self.downloads.online_client = None;
        self.connection_reset();
        if crate::options::options().offline {
            self.simulate_unreachable(true);
        }
        let Some(session) = self.session.as_ref() else { return };
        engine.set_client(Some(session.client.clone()), &session.server_id);
        if !engine.owns_storage() {
            self.toast("Downloads", NOT_OWNER_TEXT, cx);
        }
        self.load_download_policy(cx);
        self.start_downloads_poll(cx);
    }

    /// Asks what the server lets the user do. A positive answer also means
    /// the server is there: positions kept from offline plays go now.
    pub(crate) fn load_download_policy(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.session.as_ref().map(|s| s.user_id.clone()) else {
            return;
        };
        self.fetch(
            cx,
            |client| {
                let me = client.me()?;
                let allowed = me["Policy"]["EnableContentDownloading"].as_bool().unwrap_or(false);
                let sent = offline::flush(&client).unwrap_or_else(|err| {
                    log::warn!("offline positions not sent: {err:#}");
                    0
                });
                Ok((allowed, sent))
            },
            move |this, result, cx| {
                if this.session.as_ref().map(|s| &s.user_id) != Some(&opened) {
                    return;
                }
                if let Ok((allowed, sent)) = result {
                    this.downloads.allowed = Some(allowed);
                    if sent > 0 {
                        this.toast("Downloads", format!("Sent the position of {sent} offline play(s)."), cx);
                    }
                    this.rebuild_menu(cx);
                    cx.notify();
                }
            },
        );
    }

    fn start_downloads_poll(&mut self, cx: &mut Context<Self>) {
        if self.downloads.poll.is_some() {
            return;
        }
        // The engine says when its state changed: one message for all
        // the changes since the last one taken. With nothing under way
        // none comes, and the app sleeps; a timer that asked four times
        // a second woke it for the whole life of the window.
        let changes = engine().changes();
        self.downloads.poll = Some(cx.spawn(async move |this, cx| {
            while changes.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
                cx.background_executor().timer(POLL_EVERY).await;
            }
        }));
    }

    /// True when the download buttons show: the server allows it, or it
    /// has not said yet and there is something downloaded already.
    pub fn downloads_allowed(&self) -> bool {
        match self.downloads.allowed {
            Some(allowed) => allowed,
            None => engine().summary().entries > 0,
        }
    }

    /// Puts an item in the queue, with a word when it cannot go.
    pub fn download_item(&mut self, item: &Item, cx: &mut Context<Self>) {
        if self.downloads.offline {
            self.toast("Downloads", "The server cannot be reached.", cx);
            return;
        }
        if !item.is_playable() {
            return;
        }
        let engine = engine();
        if !engine.owns_storage() {
            self.toast("Downloads", NOT_OWNER_TEXT, cx);
            return;
        }
        let fresh = engine.state_of(&item.id).is_none();
        if let Err(err) = engine.add(item) {
            log::warn!("download refused: {err:#}");
            self.toast("Downloads", "The server sent an item that cannot be saved safely.", cx);
            return;
        }
        if fresh {
            self.note_queued(1, Some(&item.display_title()), cx);
        }
        cx.notify();
    }

    /// Says that items went into the queue, in one notice: a second item
    /// within the life of the notice adds to its count and does not put a
    /// second card on top. The ring in the top bar shows the progress.
    fn note_queued(&mut self, added: usize, title: Option<&str>, cx: &mut Context<Self>) {
        let now = Instant::now();
        let before = match self.downloads.queued_notice {
            Some((at, count)) if now.duration_since(at) < QUEUED_NOTICE => count,
            _ => 0,
        };
        let count = before + added;
        self.downloads.queued_notice = Some((now, count));
        self.toast_as("download-queued", "Download", queued_text(count, title), cx);
    }

    /// Queues the episodes of a season that are not downloaded.
    pub fn download_episodes(&mut self, episodes: &[Item], cx: &mut Context<Self>) {
        if self.downloads.offline {
            self.toast("Downloads", "The server cannot be reached.", cx);
            return;
        }
        let engine = engine();
        if !engine.owns_storage() {
            self.toast("Downloads", NOT_OWNER_TEXT, cx);
            return;
        }
        // One step for the season: the index is written once.
        match engine.add_many(episodes.iter().filter(|e| e.is_playable())) {
            Ok(0) => {}
            Ok(added) => self.note_queued(added, None, cx),
            Err(err) => log::warn!("download refused: {err:#}"),
        }
        cx.notify();
    }

    pub fn open_downloads(&mut self, cx: &mut Context<Self>) {
        if matches!(self.nav.page, Page::Downloads) {
            self.nav.scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
            cx.notify();
            return;
        }
        self.navigate(Page::Downloads, cx);
    }

    /// Tries the server again now. A test that pointed the session at a
    /// dead address gets the server back. When the probe answers, the app
    /// is online again and the page loads (`connection::recovered`).
    pub fn retry_connection(&mut self, cx: &mut Context<Self>) {
        self.simulate_unreachable(false);
        if self.connection.core.state != crate::connection::State::Online {
            self.probe_now(cx);
            return;
        }
        // Nothing to recover: the session is whole again, the page loads.
        if let Some(session) = self.session.as_ref() {
            engine().set_client(Some(session.client.clone()), &session.server_id);
        }
        self.load_page(cx);
    }

    /// Points the session at an address that answers nothing, or back at
    /// the server. A test of the offline mode uses it.
    pub(crate) fn simulate_unreachable(&mut self, on: bool) {
        let device_id = self.config.device_id.clone();
        if on {
            if self.downloads.online_client.is_some() {
                return;
            }
            let Some(session) = self.session.as_mut() else { return };
            let dead = Client::new("http://127.0.0.1:9", &device_id);
            let dead = match (&session.client.token, &session.client.user_id) {
                (Some(token), Some(user)) => dead.with_session(token, user),
                _ => dead,
            };
            // The same identity: a position of an offline play is kept
            // under the server and user of the session.
            let dead = dead.with_server(&session.server_id);
            let real = std::mem::replace(&mut session.client, dead);
            self.downloads.online_client = Some(real);
            self.stop_sync();
        } else if let Some(real) = self.downloads.online_client.take()
            && let Some(session) = self.session.as_mut()
        {
            session.client = real;
        }
    }

    /// Loads a page without the server while offline. True when the page
    /// is handled here.
    pub fn offline_load_page(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.downloads.offline {
            return false;
        }
        match &mut self.nav.page {
            Page::Detail(data) => {
                if let Some(item) = engine().item(&data.item.id) {
                    data.set_item(item);
                }
                data.loading = false;
                self.rebuild_detail_menus(cx);
                cx.notify();
                true
            }
            Page::Home(data) => {
                data.loading = false;
                true
            }
            Page::Library(data) => {
                data.loading = false;
                true
            }
            Page::Search(data) => {
                data.loading = false;
                true
            }
            Page::Playlist(data) => {
                data.loading = false;
                true
            }
            Page::Admin(data) => {
                data.loading = false;
                true
            }
            _ => false,
        }
    }

    /// True, with a notice, for a download that is of another server than
    /// the session's: the Downloads page lists them all, but a page or a
    /// play of the item would go through this session, whose server knows
    /// the id as another item or not at all.
    fn download_of_another_server(&mut self, of: Option<&str>, item_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(server) = self.session.as_ref().map(|s| s.server_id.clone()) else {
            return false;
        };
        // The row names its server; a caller without one (the debug
        // channel) means the download of this session when there is one.
        let other = match of {
            Some(of) => of != server,
            None => {
                let entries = engine().entries();
                !entries.iter().any(|e| e.item_id == item_id && e.server_id == server)
                    && entries.iter().any(|e| e.item_id == item_id)
            }
        };
        if !other {
            return false;
        }
        self.toast("Downloads", "This download is of another server. Sign in to that server to open it.", cx);
        true
    }

    /// Opens the page of a downloaded item: from `meta.json`, so it works
    /// offline too.
    pub fn open_download(&mut self, server_id: Option<&str>, item_id: &str, cx: &mut Context<Self>) {
        if self.download_of_another_server(server_id, item_id, cx) {
            return;
        }
        let item = engine().item(Key::of(server_id, item_id)).or_else(|| {
            engine().entry(Key::of(server_id, item_id)).map(|e| Item {
                id: e.item_id.clone(),
                name: e.name.clone(),
                kind: e.kind.clone(),
                series_name: e.series_name.clone(),
                series_id: e.series_id.clone(),
                parent_index_number: e.season,
                index_number: e.episode,
                production_year: e.year,
                ..Default::default()
            })
        });
        if let Some(item) = item {
            self.open_item(item, cx);
        }
    }

    /// Plays a downloaded item, from its local resume point when it has one.
    pub fn play_download(&mut self, server_id: Option<&str>, item_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.download_of_another_server(server_id, item_id, cx) {
            return;
        }
        let Some(mut item) = engine().item(Key::of(server_id, item_id)) else {
            self.toast("Downloads", "The item is not downloaded.", cx);
            return;
        };
        // An offline play of this profile left a position; it counts until
        // the server takes it.
        if let Some(who) = self.session.as_ref().and_then(|s| offline::Identity::of(&s.client))
            && let Some(ticks) = offline::position(&who, item_id)
            && ticks > item.user_data.playback_position_ticks
        {
            item.user_data.playback_position_ticks = ticks;
        }
        let resume = item.resume_secs() > 0;
        self.play(&item, resume, window, cx);
    }
}

#[cfg(test)]
mod notice_tests {
    use super::queued_text;

    #[test]
    fn the_queue_notice_names_one_item_and_counts_more() {
        assert_eq!(queued_text(1, Some("S2:E5 · Premature Death")), "S2:E5 · Premature Death is in the queue.");
        assert_eq!(queued_text(1, None), "1 item is in the queue.");
        assert_eq!(queued_text(3, Some("the third title")), "3 items are in the queue.");
        assert_eq!(queued_text(12, None), "12 items are in the queue.");
    }
}

#[cfg(test)]
mod segment_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_segments_of_a_meta_file_are_read() {
        let meta = json!({"ItemId": "a", "Segments": [
            {"Id": "x", "ItemId": "a", "Type": "Intro", "StartTicks": 1098400000_i64, "EndTicks": 1951950000_i64},
            {"Type": "Outro", "StartTicks": 5, "EndTicks": 10}
        ]});
        let segments = segments_of_meta(&meta).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].kind, crate::jellyfin::SegmentKind::Intro);
        assert!((segments[0].start_secs() - 109.84).abs() < 1e-6);
        assert!((segments[0].end_secs() - 195.195).abs() < 1e-6);
    }

    #[test]
    fn a_meta_file_with_no_range_leaves_it_to_the_server() {
        assert!(segments_of_meta(&json!({"Segments": []})).is_none());
        assert!(segments_of_meta(&json!({"ItemId": "a"})).is_none());
        assert!(segments_of_meta(&json!({"Segments": "bad"})).is_none());
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;

    /// The engine of a test run is not in the folder of the user.
    #[test]
    fn the_engine_of_the_tests_has_a_folder_of_its_own() {
        assert!(folder().starts_with(std::env::temp_dir()));
        assert_ne!(folder(), dir());
    }
}
