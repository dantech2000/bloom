// SPDX-License-Identifier: AGPL-3.0-or-later
//! Downloads for offline use: the engine that fetches the files, the pages
//! and buttons that drive it, and the offline mode of the app when the
//! server cannot be reached.
//!
//! The files live under `~/Library/Application Support/bloom/downloads/`
//! (`BLOOM_DOWNLOAD_DIR` for tests). A complete file plays in place of
//! the stream: see [`local_source`], called where the player gets its URL.

pub mod engine;
mod stall;
pub mod offline;
mod ui;

use std::{
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

use gpui_kit::{Context, Task, Window};

pub use engine::{Engine, Entry, EntryState, format_bytes};

use crate::{
    app::{Bloom, Page},
    jellyfin::{Client, Item},
    player::Player,
};

/// The engine of this process; opened on first use.
static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Folder of the downloads.
pub fn dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BLOOM_DOWNLOAD_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::brand::FOLDER)
        .join("downloads")
}

pub fn engine() -> &'static Engine {
    ENGINE.get_or_init(|| Engine::open(dir(), engine::Options::default()))
}

/// The complete local file of an item of the server `server_id`, when
/// there is one; the file of another server with the same item id does not
/// count. Cheap; it reads the index in memory and asks the disk once.
pub fn local_path(server_id: Option<&str>, item_id: &str) -> Option<PathBuf> {
    ENGINE.get()?.local_path_of(server_id, item_id)
}

/// The artwork of a download for an image URL of the server; see
/// [`Engine::local_image`]. The image loader asks before the network.
pub fn local_image(url: &str) -> Option<PathBuf> {
    ENGINE.get()?.local_image(url)
}

/// The source the player loads for an item: its local file when the item
/// is downloaded, with its external subtitles beside it, else `None` and
/// the caller uses the stream. This is the one hook of playback.
pub fn local_source(player: &Player, server_id: Option<&str>, item_id: &str) -> Option<String> {
    let path = local_path(server_id, item_id);
    let subtitles: Vec<String> = path
        .as_ref()
        .and_then(|p| p.parent())
        .and_then(|folder| ENGINE.get()?.meta(item_id).map(|meta| (folder.to_path_buf(), meta)))
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
    // A stream must not get the subtitles of the item before.
    player.set_property("sub-files", &subtitles.join(":"));
    path.map(|p| p.display().to_string())
}

/// The intro and credits ranges that `meta.json` of a downloaded item
/// holds. `None` when the item has no local file or the file has no range;
/// the caller then asks the server.
pub fn stored_segments(server_id: Option<&str>, item_id: &str) -> Option<Vec<crate::jellyfin::MediaSegment>> {
    local_path(server_id, item_id)?;
    segments_of_meta(&ENGINE.get()?.meta(item_id)?)
}

/// The `Segments` array of a `meta.json`: the `Items` of `/MediaSegments`.
fn segments_of_meta(meta: &serde_json::Value) -> Option<Vec<crate::jellyfin::MediaSegment>> {
    let segments: Vec<crate::jellyfin::MediaSegment> =
        serde_json::from_value(meta.get("Segments")?.clone()).ok()?;
    (!segments.is_empty()).then_some(segments)
}

/// Downloads as the app holds them: the offline mode and the poll that
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
    version_seen: u64,
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

/// Progress reaches the UI this often at most.
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
            o.limit_bytes = limit.map(|gb| gb * 1_000_000_000);
        });
        self.downloads.allowed = None;
        self.downloads.offline = false;
        self.downloads.online_client = None;
        self.connection_reset();
        if std::env::var_os("BLOOM_OFFLINE").is_some() {
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
                let me: serde_json::Value = client.get("/Users/Me", &[])?;
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
        self.downloads.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_EVERY).await;
                let alive = this.update(cx, |this, cx| {
                    let version = engine().version();
                    if version != this.downloads.version_seen {
                        this.downloads.version_seen = version;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// True when the download buttons show: the server allows it, or it
    /// has not said yet and there is something downloaded already.
    pub fn downloads_allowed(&self) -> bool {
        match self.downloads.allowed {
            Some(allowed) => allowed,
            None => !engine().entries().is_empty(),
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
        let fresh = engine.entry(&item.id).is_none();
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
        let mut added = 0;
        for episode in episodes.iter().filter(|e| e.is_playable()) {
            let state = engine.entry(&episode.id).map(|e| e.state);
            if state.is_none() || matches!(state, Some(EntryState::Paused | EntryState::Failed)) {
                match engine.add(episode) {
                    Ok(()) => added += 1,
                    Err(err) => log::warn!("download refused: {err:#}"),
                }
            }
        }
        if added > 0 {
            self.note_queued(added, None, cx);
        }
        cx.notify();
    }

    pub fn open_downloads(&mut self, cx: &mut Context<Self>) {
        if matches!(self.page, Page::Downloads) {
            self.page_scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
            cx.notify();
            return;
        }
        self.navigate(Page::Downloads, cx);
    }

    /// A page could not load. When nothing reached the server
    /// (`connection::classify`), a probe decides whether the app is
    /// offline; then the Downloads page opens when there are downloads,
    /// and the pages stop asking the server. True when the error is of
    /// that kind: the page shows no raw text for it.
    pub fn downloads_server_failed(&mut self, err: &anyhow::Error, cx: &mut Context<Self>) -> bool {
        self.request_failed(err, cx)
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
        match &mut self.page {
            Page::Detail(data) => {
                if let Some(item) = engine().item(&data.item.id) {
                    data.item = item;
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

    /// Opens the page of a downloaded item: from `meta.json`, so it works
    /// offline too.
    pub fn open_download(&mut self, item_id: &str, cx: &mut Context<Self>) {
        let item = engine().item(item_id).or_else(|| {
            engine().entry(item_id).map(|e| Item {
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
    pub fn play_download(&mut self, item_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut item) = engine().item(item_id) else {
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

    /// Where the player got its item: the local file or the stream, and
    /// the path mpv reports. For the debug channel.
    pub fn downloads_source_label(&self) -> String {
        let Some(playing) = self.playing.as_ref() else {
            return "none".to_string();
        };
        let server = self.session.as_ref().and_then(|s| s.client.server_id.clone());
        let source = match local_path(server.as_deref(), &playing.id) {
            Some(path) => format!("local:{}", path.display()),
            None => "stream".to_string(),
        };
        format!("{source} mpv_path={:?}", self.player_status.path)
    }

    /// The debug channel: `downloads list|add <id>|cancel <id>|resume <id>|
    /// remove <id>|remove-all|page|offline on|off|play <id>|state`.
    pub fn debug_downloads(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        let engine = engine();
        match verb {
            "list" => {
                let mut lines: Vec<String> = engine
                    .entries()
                    .iter()
                    .map(|e| {
                        format!(
                            "{} | {} | {:?} | {}/{} | {}/s{}",
                            e.item_id,
                            e.title(),
                            e.state,
                            e.done,
                            e.total.map_or("?".to_string(), |t| t.to_string()),
                            format_bytes(e.speed as u64),
                            e.error.as_deref().map(|err| format!(" | {err}")).unwrap_or_default(),
                        )
                    })
                    .collect();
                if lines.is_empty() {
                    lines.push("no downloads".to_string());
                }
                return lines.join("\n");
            }
            "add" => {
                let id = arg.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    |this, result, cx| match result {
                        Ok(item) => this.download_item(&item, cx),
                        Err(err) => log::warn!("debug downloads add: {err:#}"),
                    },
                );
            }
            "cancel" => engine.cancel(arg),
            "resume" => engine.resume(arg),
            "remove" => engine.remove(arg),
            "remove-all" => engine.remove_all(),
            "page" => self.open_downloads(cx),
            // The page of a downloaded item, from its `meta.json`.
            "open" => self.open_download(arg, cx),
            // The entries the card menu of an item gets, by their ids.
            "menu" => {
                let item = engine.item(arg).or_else(|| {
                    engine.entry(arg).map(|e| Item { id: e.item_id.clone(), kind: e.kind.clone(), ..Default::default() })
                });
                let Some(item) = item else {
                    return "error: not a downloaded item; use `add` first".into();
                };
                let labels: Vec<String> = self
                    .download_menu_items(&item, cx)
                    .iter()
                    .filter_map(|entry| entry.debug_label())
                    .collect();
                return labels.join(" | ");
            }
            "offline" => match arg {
                "on" => {
                    self.simulate_unreachable(true);
                    engine.set_client(None, "");
                    self.open_home(cx);
                }
                "off" => self.retry_connection(cx),
                _ => return "error: downloads offline on|off".into(),
            },
            "play" => {
                self.muted = true;
                self.play_download(arg, window, cx);
            }
            "state" | "" => {}
            _ => {
                return "error: downloads list|add <id>|cancel <id>|resume <id>|remove <id>|remove-all|page|open <id>|menu <id>|offline on|off|play <id>|state".into();
            }
        }
        cx.notify();
        let entries = engine.entries();
        format!(
            "downloads offline={} allowed={:?} owner={} entries={} active={} done={} used={} free={} dir={} source={} segments=[{}]",
            self.downloads.offline,
            self.downloads.allowed,
            engine.owns_storage(),
            entries.len(),
            entries.iter().filter(|e| e.is_active()).count(),
            entries.iter().filter(|e| e.state == EntryState::Done).count(),
            format_bytes(engine.used_bytes()),
            engine.free_bytes().map_or("?".to_string(), format_bytes),
            engine.dir().display(),
            self.downloads_source_label(),
            self.segments
                .iter()
                .map(|s| format!("{} {:.1}-{:.1}", s.kind, s.start_secs(), s.end_secs()))
                .collect::<Vec<_>>()
                .join(", "),
        )
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
        assert_eq!(segments[0].kind, "Intro");
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
