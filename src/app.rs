// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Root view: session, navigation, background fetching, player polling.

use std::{cell::RefCell, collections::HashMap, time::Duration};

use anyhow::Result;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, Styled, Task, Window, div, px, rgb, rgba,
};

use crate::{
    config::{Config, Profile},
    jellyfin::{Client, Item, ItemQuery, SeerrItem, MediaSegment},
    player::{PlayState, Player, PlayerStatus},
    ui::menu::MenuItem,
    ui::{
        input::InputState,
        menu::MenuState,
        slider::{SliderEvent, SliderState},
        theme::{UiRadius, UiTheme},
        toast::ToastState,
    },
    video_surface::VideoFrame,
    views::{cards::Metrics, connect::ConnectState},
};

pub const PAGE_SIZE: usize = 100;
/// Slides in the home hero.
pub const HERO_ITEMS: usize = 20;
/// Time one hero slide stays on screen.
pub const HERO_SLIDE: Duration = Duration::from_millis(7000);
/// Id of the synthetic library that lists the user's favourites.
pub const FAVORITES_ID: &str = "favorites";
/// Pointer idle time before the player controls fade out.
const CONTROLS_HIDE_AFTER: Duration = Duration::from_millis(2500);
/// Time a paused player waits with no input before the pause screen shows.
const PAUSE_SCREEN_AFTER: Duration = Duration::from_secs(5);

/// The Abyss look of the user's Jellyfin web UI: near-black page, off-white
/// accent, 12 px corners, translucent "glass" panels.
pub fn theme(dark: bool) -> UiTheme {
    let mut t = if dark {
        UiTheme::neutral_dark()
    } else {
        UiTheme::neutral_light()
    };
    let accent = rgb(0xf5f5f7);
    let ink = rgb(0x121212);
    if dark {
        t.colors.background = rgb(0x101010);
        t.colors.foreground = rgba(0xffffffde);
        t.colors.card = rgb(0x1a1a1a);
        t.colors.card_foreground = rgba(0xffffffde);
        t.colors.popover = rgb(0x2a2a2a);
        t.colors.popover_foreground = accent;
        t.colors.secondary = rgba(0xffffff1f);
        t.colors.secondary_foreground = accent;
        t.colors.muted = rgb(0x1c1c1c);
        t.colors.muted_foreground = rgba(0xffffff99);
        t.colors.accent = rgba(0xf5f5f71f);
        t.colors.accent_foreground = accent;
        t.colors.border = rgba(0xf5f5f729);
        t.colors.input = rgba(0xffffff17);
        t.colors.sidebar = rgb(0x101010);
        t.colors.sidebar_foreground = accent;
        t.colors.sidebar_accent = rgba(0xf5f5f71f);
        t.colors.sidebar_accent_foreground = accent;
        t.colors.sidebar_border = rgba(0xf5f5f729);
        t.colors.primary = accent;
        t.colors.primary_foreground = ink;
        t.colors.ring = rgba(0xf5f5f799);
        t.colors.sidebar_primary = accent;
        t.colors.sidebar_primary_foreground = ink;
        t.colors.sidebar_ring = rgba(0xf5f5f799);
        t.colors.destructive = rgb(0xf92672);
    } else {
        t.colors.background = rgb(0xfafafa);
        t.colors.sidebar = rgb(0xf3f3f6);
        t.colors.primary = ink;
        t.colors.primary_foreground = accent;
        t.colors.ring = ink;
    }
    t.fonts.body = "Google Sans".into();
    t.radius = UiRadius::new(px(12.));
    t
}

/// Which session is open: moves on when a session opens, switches or ends.
/// A request notes the epoch it started in, and its answer is dropped when
/// the epoch moved meanwhile: the answer belongs to a session that is gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionEpoch(pub(crate) u64);

/// One request, or one instance of an editor, among the requests of the
/// same kind: the latest one owns the answer. From `Bloom::next_revision`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Revision(pub(crate) u64);

/// True while a test keeps this process away from the files and sockets of
/// the user: no write to the config, no download engine, no server socket.
#[cfg(test)]
fn sandboxed() -> bool {
    race_harness::sandboxed()
}

#[cfg(not(test))]
fn sandboxed() -> bool {
    false
}

pub struct Session {
    pub server_id: String,
    pub server_name: String,
    pub user_id: String,
    pub user_name: String,
    pub user_image: Option<String>,
    pub client: Client,
    /// The user may open the server dashboard.
    pub is_admin: bool,
    /// Audio language the user set on the server, such as "eng".
    pub audio_language: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Connect,
    Main,
}

#[derive(Default)]
pub struct HomeData {
    pub loading: bool,
    /// Section kinds in display order (`homesection0..`).
    pub order: Vec<String>,
    /// Items of the hero slideshow at the top of the page.
    pub hero: Vec<Item>,
    pub resume: Vec<Item>,
    pub next_up: Vec<Item>,
    pub latest: Vec<(Item, Vec<Item>)>,
}

pub struct LibraryData {
    pub view: Item,
    pub title: String,
    pub loading: bool,
    /// The items of the current page.
    pub items: Vec<Item>,
    pub total: usize,
    /// Index of the first item of the current page.
    pub start: usize,
    pub sort: &'static str,
    pub descending: bool,
    /// One of the server's item filters, such as "IsUnplayed".
    pub filter: Option<&'static str>,
    /// Letter picked at the right edge; '#' stands for names before "A".
    pub letter: Option<char>,
    /// What of the library shows; picked in the menu of the title.
    pub show: LibraryShow,
}

/// The views of a library, as in the title menu of the web client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryShow {
    /// The movies or the shows of the library.
    #[default]
    All,
    Favorites,
    /// The collections of the server (in a movie library).
    Collections,
    /// All episodes (in a show library).
    Episodes,
}

impl LibraryShow {
    pub fn label(self, library: &str) -> String {
        match self {
            LibraryShow::All => library.to_string(),
            LibraryShow::Favorites => "Favorites".to_string(),
            LibraryShow::Collections => "Collections".to_string(),
            LibraryShow::Episodes => "Episodes".to_string(),
        }
    }

    /// The views a library of this collection type has.
    pub fn of(collection_type: Option<&str>) -> &'static [LibraryShow] {
        match collection_type {
            Some("movies") => &[
                LibraryShow::All,
                LibraryShow::Favorites,
                LibraryShow::Collections,
            ],
            Some("tvshows") => &[
                LibraryShow::All,
                LibraryShow::Favorites,
                LibraryShow::Episodes,
            ],
            _ => &[],
        }
    }
}

impl LibraryData {
    pub fn new(view: Item) -> Self {
        Self {
            title: view.name.clone(),
            view,
            loading: true,
            items: Vec::new(),
            total: 0,
            start: 0,
            sort: "SortName",
            descending: false,
            filter: None,
            letter: None,
            show: LibraryShow::All,
        }
    }
}

/// Sort keys of the library toolbar: label and the server's `sortBy` value.
pub const LIBRARY_SORTS: [(&str, &str); 8] = [
    ("Name", "SortName"),
    ("Random", "Random"),
    ("Community Rating", "CommunityRating,SortName"),
    ("Critic Rating", "CriticRating,SortName"),
    ("Date Added", "DateCreated,SortName"),
    ("Date Played", "DatePlayed,SortName"),
    ("Release Date", "ProductionYear,PremiereDate,SortName"),
    ("Runtime", "Runtime,SortName"),
];

/// Filters of the library toolbar: label and the server's `filters` value.
pub const LIBRARY_FILTERS: [(&str, &str); 4] = [
    ("Unplayed", "IsUnplayed"),
    ("Played", "IsPlayed"),
    ("Favorites", "IsFavorite"),
    ("Resumable", "IsResumable"),
];

#[derive(Default)]
pub struct DetailData {
    pub item: Item,
    pub loading: bool,
    /// Seasons of a series.
    pub seasons: Vec<Item>,
    /// Episodes of a season.
    pub episodes: Vec<Item>,
    /// Next episode to watch of a series.
    pub next_up: Option<Item>,
    /// "More Like This" for a movie or a series.
    pub similar: Vec<Item>,
    /// Audio track picked on the page: its place among the audio streams.
    pub audio: Option<usize>,
    /// Subtitle picked on the page: `Some(None)` is "Off".
    pub subtitle: Option<Option<usize>>,
}

/// The home row a card sits in, for the "remove from row" menu entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardRow {
    None,
    Resume,
    NextUp,
}

/// Id prefix of the synthetic library that lists a person's titles.
pub const PERSON_VIEW: &str = "person";

#[derive(Default)]
pub struct SearchData {
    /// Text of the server search.
    pub query: String,
    /// Text in the field; the results of the title index are for it. It runs
    /// ahead of `query` while the user types.
    pub typed: String,
    pub loading: bool,
    pub results: Vec<Item>,
    /// Titles from Seerr for the same text ("Discover on Seerr").
    pub seerr: Vec<SeerrItem>,
    /// Text of the server search on its way, while one is.
    pub pending: Option<String>,
    /// The last answer of the server, with the text it is for. `typed` may
    /// run ahead of it; a return to that text shows it with no request.
    pub answered: Option<(String, Vec<Item>, Vec<SeerrItem>)>,
}

pub enum Page {
    Home(HomeData),
    Library(LibraryData),
    Detail(DetailData),
    Search(SearchData),
    /// Server dashboard for administrators.
    Admin(crate::admin::AdminData),
    /// Settings of the signed-in user.
    Settings(crate::settings::Section),
    /// One playlist, with its entries in order.
    Playlist(crate::lists::PlaylistData),
    /// The files downloaded for offline use.
    Downloads,
}

pub struct Bloom {
    pub config: Config,
    pub screen: Screen,
    pub session: Option<Session>,
    pub toasts: Entity<ToastState>,
    pub player: Player,
    pub player_status: PlayerStatus,
    player_poll: Option<Task<()>>,
    /// The display-sleep assertion while a video plays (src/awake.rs).
    pub awake: crate::awake::Awake,
    /// The Now Playing tile and the media keys (src/nowplaying.rs).
    pub nowplaying: crate::nowplaying::State,
    /// The AirPlay sender; idle until an item is sent.
    pub airplay: crate::airplay::AirPlay,
    pub connect: ConnectState,
    pub views: Vec<Item>,
    pub page: Page,
    pub history: Vec<Page>,
    /// Scroll position of each page in `history`.
    history_scroll: Vec<gpui_kit::Point<gpui_kit::Pixels>>,
    /// Focus of the main screen, so its keyboard shortcuts work.
    pub app_focus: FocusHandle,
    pub profile_menu: Entity<MenuState>,
    pub search_input: Entity<InputState>,
    /// Scroll position of the current page.
    pub page_scroll: ScrollHandle,
    /// Scroll positions of the horizontal rows, by row id.
    row_scrolls: RefCell<HashMap<SharedString, ScrollHandle>>,
    /// Window width at the last render; card sizes follow it.
    pub viewport_w: f32,
    pub viewport_h: f32,
    /// Current slide of the home hero and the time it came on screen.
    pub hero_index: usize,
    pub hero_since: std::time::Instant,
    /// Time shown on the current slide before the user paused the hero.
    pub hero_paused: Option<Duration>,
    pub hero_tick: Option<Task<()>>,
    /// Pending write of the window place to the config file.
    window_save: Option<Task<()>>,
    /// Pending search for the text in the search field.
    search_debounce: Option<Task<()>>,
    /// Titles of the movies and shows of the server, for a search with no
    /// request, and the time they were loaded.
    pub catalog: Vec<Item>,
    catalog_loaded: Option<std::time::Instant>,
    /// The request of the title index whose answer is wanted.
    catalog_request: Revision,
    /// Trailer backdrop of the hero.
    pub hero_video: crate::views::hero::HeroVideo,
    /// Hero slides; kept for the session so the list survives navigation.
    pub hero: Vec<Item>,
    /// The home rows as a view GPUI can cache between frames.
    pub rows_view: Entity<crate::views::home::HomeRows>,
    /// Set by a redraw request that changes the hero alone (trailer frame,
    /// progress line), so the observer below does not count it.
    pub hero_only_redraw: bool,
    /// True when something other than the hero asked for a redraw since the
    /// last render.
    content_dirty: bool,
    /// True while the current frame may reuse the cached home rows.
    pub hero_only_frame: bool,
    /// Slide the hero showed before the current one, and when it changed;
    /// the new backdrop fades in over the old one.
    pub hero_previous: Option<usize>,
    pub hero_changed: std::time::Instant,
    /// Whether the embedded video view covers the content area.
    pub player_open: bool,
    /// `player_open` at the frame before, and whether the window was full
    /// screen when the player opened: full screen that began with the
    /// player ends with it (`follow_player_fullscreen`).
    player_was_open: bool,
    fullscreen_before_player: bool,
    pub current_frame: Option<VideoFrame>,
    frame_seq: u64,
    /// When the frames task last woke, and when the last draw ran (pacing
    /// trace).
    frame_wake_ns: u64,
    last_draw_ns: u64,
    /// The position the seek slider was last set to by the frames task.
    slider_pos: f64,
    /// The newest frame whose wait was measured (`dev/jctl pacing`).
    noticed_frame_seq: u64,
    pub seek_slider: Entity<SliderState>,
    pub scrubbing: bool,
    pub audio_menu: Entity<MenuState>,
    /// Gear menu of the player: audio track and playback speed.
    pub settings_menu: Entity<MenuState>,
    pub volume_slider: Entity<SliderState>,
    /// Player volume from 0 to 100.
    pub volume: f32,
    pub muted: bool,
    pub speed: f32,
    /// The item in the player, for its rating, end time and favourite state.
    pub playing: Option<Item>,
    /// Intro and credits ranges of the item in the player.
    pub segments: Vec<MediaSegment>,
    /// Items that follow in the player, and its chapters and previews.
    pub queue: crate::queue::PlayQueue,
    pub sort_menu: Entity<MenuState>,
    /// Views of the library, under its title.
    pub show_menu: Entity<MenuState>,
    pub filter_menu: Entity<MenuState>,
    pub detail_audio_menu: Entity<MenuState>,
    pub detail_subtitle_menu: Entity<MenuState>,
    pub more_menu: Entity<MenuState>,
    /// Context menu of the card under the pointer.
    pub card_menu: Entity<MenuState>,
    /// Quality tag settings of the user in Jellyfin Enhanced: on, resolution
    /// tag, dynamic range tag.
    pub je_quality_tags: (bool, bool, bool),
    /// Question of the dashboard that waits for an answer.
    pub admin_confirm: Option<crate::admin::Confirm>,
    /// Form of the dashboard that waits for its text.
    pub admin_prompt: Option<crate::admin::Prompt>,
    /// The prompt opened since the last render: its fields are not ready.
    pub admin_prompt_fresh: bool,
    /// Editing state of the prompt fields: a plain one and a masked one.
    pub admin_inputs: [Entity<InputState>; 3],
    /// Value the dashboard shows once, such as a new API key.
    pub admin_secret: Option<crate::admin::Secret>,
    /// Timer that loads the live part of the dashboard again.
    pub admin_tick: Option<Task<()>>,
    /// SyncPlay: the session with the server and the panel of the groups.
    pub sync: crate::syncplay::ui::SyncState,
    /// Remote control of and by other devices ("Play On").
    pub cast: crate::cast::CastState,
    /// Metadata manager: its dialog and the editing state of its fields.
    pub metadata: crate::metadata::State,
    /// Playlists and collections: the dialog and the playlists of the user.
    pub lists: crate::lists::State,
    /// Features of the Jellyfin Enhanced plugin.
    pub enhanced: crate::enhanced::State,
    /// Playback negotiation: the tracks chosen for the next item.
    pub stream: crate::stream::State,
    /// Google Cast: the devices and the one connection.
    pub chromecast: crate::chromecast::State,
    /// Downloads for offline use and the offline mode.
    pub downloads: crate::downloads::DownloadsState,
    /// Whether the server answers (src/connection.rs).
    pub connection: crate::connection::ConnectionState,
    pub subtitle_menu: Entity<MenuState>,
    /// Search for subtitles and the timing offsets (`subtitles.rs`).
    pub subs: crate::subtitles::State,
    pub(crate) tracks_version: u64,
    pub player_focus: FocusHandle,
    /// Whether the player overlay controls are shown; hidden after pointer idle.
    /// The player shows its data about the video: codecs, bitrates,
    /// dropped frames, cache.
    pub playback_info: bool,
    /// What the server keeps for the user: configuration and display
    /// preferences. Playback follows some of them.
    pub prefs: crate::settings::Prefs,
    /// The saves of settings on their way; they outlive a session.
    pub(crate) pref_lanes: crate::settings::SaveLanes,
    /// Episode list of the player.
    pub episode_picker: crate::views::episodes::EpisodePicker,
    pub controls_visible: bool,
    /// The close, minimise and zoom buttons are hidden (see the root render).
    window_buttons_hidden: bool,
    /// The pointer is hidden with the controls of the player.
    pointer_hidden: bool,
    /// The version the "update is ready" toast was shown for.
    pub update_said: Option<String>,
    /// The debug channel: where its listeners send the commands, and the
    /// listener that the setting turned on.
    pub debug_requests: Option<async_channel::Sender<crate::debug::Request>>,
    pub debug_listener: Option<crate::debug::Listener>,
    /// The paused player shows the item's details in place of the controls.
    pub pause_screen: bool,
    /// Frame of the normal window while the player is in picture in
    /// picture; `None` when it is not.
    pub pip: Option<crate::pip::Frame>,
    last_pointer_activity: std::time::Instant,
    pub(crate) generation: u64,
    /// See [`SessionEpoch`].
    pub(crate) session_epoch: SessionEpoch,
    /// The last revision given out by `next_revision`.
    revisions: Revision,
    _subscriptions: Vec<gpui_kit::Subscription>,
}

impl Bloom {
    pub fn new(config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let toasts = cx.new(ToastState::new);
        let profile_menu = cx.new(|cx| MenuState::new([], cx));
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search movies, shows, episodes…"));
        let connect = ConnectState::new(window, cx);
        let seek_slider = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.1));
        let audio_menu = cx.new(|cx| MenuState::new([], cx));
        let settings_menu = cx.new(|cx| MenuState::new([], cx));
        let volume_slider =
            cx.new(|_| SliderState::new().min(0.).max(100.).step(1.).default_value(100.));
        let sort_menu = cx.new(|cx| MenuState::new([], cx));
        let show_menu = cx.new(|cx| MenuState::new([], cx));
        let filter_menu = cx.new(|cx| MenuState::new([], cx));
        let detail_audio_menu = cx.new(|cx| MenuState::new([], cx));
        let detail_subtitle_menu = cx.new(|cx| MenuState::new([], cx));
        let more_menu = cx.new(|cx| MenuState::new([], cx));
        let card_menu = cx.new(|cx| MenuState::new([], cx));
        let subtitle_menu = cx.new(|cx| MenuState::new([], cx));

        let mut subscriptions = Vec::new();
        let player = Player::default();
        let player_frames = player.frames();
        subscriptions.push(
            cx.subscribe_in(&search_input, window, |this, _, event, _, cx| {
                match event {
                    crate::ui::input::InputEvent::PressEnter { .. } => {
                        this.search_debounce = None;
                        let query = this.search_input.read(cx).value().to_string();
                        this.open_search(query, cx);
                    }
                    // The title index answers each keystroke; the server
                    // search follows once the typing rests.
                    crate::ui::input::InputEvent::Change => {
                        let typed = this.search_input.read(cx).value().to_string();
                        this.search_titles(typed, cx);
                        this.search_debounce = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(150))
                                .await;
                            this.update(cx, |this, cx| {
                                let query = this.search_input.read(cx).value().to_string();
                                let shown = match &this.page {
                                    Page::Search(data) => data.query.clone(),
                                    _ => return,
                                };
                                if query.trim() != shown {
                                    this.open_search(query, cx);
                                }
                            })
                            .ok();
                        }));
                    }
                    _ => {}
                }
            }),
        );
        // Enter in a field of a dashboard form submits the form.
        let admin_inputs = [
            cx.new(|cx| InputState::new(window, cx)),
            cx.new(|cx| InputState::new(window, cx).masked(true)),
            cx.new(|cx| InputState::new(window, cx).masked(true)),
        ];
        for input in &admin_inputs {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event, window, cx| {
                if let crate::ui::input::InputEvent::PressEnter { .. } = event {
                    this.submit_admin_prompt(window, cx);
                }
            }));
        }
        subscriptions.push(cx.subscribe(
            &volume_slider,
            |this, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                this.set_volume(value.end(), cx);
            },
        ));
        // Remember the window's place and size for the next launch. A resize
        // sends many changes, so the file is written once they stop.
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            if window.is_fullscreen() {
                return;
            }
            if this.pip.is_some() {
                // The small window has its own saved place.
                let place = crate::pip::frame(window);
                if place.is_some() && this.config.pip_window != place {
                    this.config.pip_window = place;
                    this.window_save = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(600))
                            .await;
                        this.update(cx, |this, cx| this.save_config(cx)).ok();
                    }));
                }
                return;
            }
            let bounds = window.window_bounds().get_bounds();
            let place = [
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ];
            if this.config.window == Some(place) {
                return;
            }
            this.config.window = Some(place);
            this.window_save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(600))
                    .await;
                this.update(cx, |this, cx| this.save_config(cx)).ok();
            }));
        }));
        subscriptions.extend(connect.subscribe(window, cx));
        // Every redraw request passes here. One from the hero tick leaves the
        // page content clean, so the next frame can reuse the cached rows.
        subscriptions.push(cx.observe(&cx.entity(), |this, _, _| {
            if !std::mem::take(&mut this.hero_only_redraw) {
                this.content_dirty = true;
            }
        }));
        subscriptions.push(cx.subscribe(
            &seek_slider,
            |this, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => {
                    log::debug!("seek slider change: {}", value.end());
                    this.scrubbing = true;
                    // The preview over the timeline follows the thumb.
                    cx.notify();
                }
                SliderEvent::Release(value) => {
                    log::debug!("seek slider release: {}", value.end());
                    this.scrubbing = false;
                    this.request_seek_to(value.end() as f64, cx);
                    cx.notify();
                }
            },
        ));

        // One popup at a time, in the whole app: a menu that opens closes
        // every other menu and panel. See `close_popups`.
        for menu in [
            &profile_menu,
            &audio_menu,
            &settings_menu,
            &sort_menu,
            &show_menu,
            &filter_menu,
            &detail_audio_menu,
            &detail_subtitle_menu,
            &more_menu,
            &card_menu,
            &subtitle_menu,
        ] {
            subscriptions.push(cx.observe_in(menu, window, |this, opened, window, cx| {
                if opened.read(cx).is_open() {
                    this.close_popups(Some(&opened), window, cx);
                }
            }));
        }
        // The bitrate limit of the config, and the end of a transcode at quit.
        subscriptions.push(crate::stream::install(&config, cx));
        // The updater says when a version is on its way or ready.
        let update_changes = crate::updates::changes();
        cx.spawn(async move |this, cx| {
            while update_changes.recv().await.is_ok() {
                if this.update(cx, |this, cx| this.update_status_changed(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Each new video frame asks for a draw (see `sync_frame`). With
        // display pacing the draw runs at a set phase after the tick of the
        // frame, not at gpui's own step at the next tick: that step leaves
        // the GPU about 4 ms before the compositor's deadline, which a frame
        // of a 4K picture misses one time in seven (see pacing.rs). The task
        // waits until the phase, asks for the draw and runs the step at once.
        crate::pacing::trace::enable_from_env();
        let frames = player_frames.clone();
        cx.spawn(async move |this, cx| {
            while frames.recv().await.is_ok() {
                let wait = this
                    .update(cx, |this, _| {
                        let (seq, tick) = this.player.frame_timing();
                        if seq == this.frame_seq {
                            return None;
                        }
                        this.frame_wake_ns = crate::player::clock_ns();
                        this.note_frame_wait();
                        let (now, period) = this.player.clock_time();
                        let phased = tick != 0 && this.player.display_pacing();
                        Some(phased.then(|| crate::pacing::frame_phase(period)).flatten().map(|phase| (tick + phase).saturating_sub(now)))
                    })
                    .ok()
                    .flatten();
                let Some(wait) = wait else {
                    continue;
                };
                if let Some(wait) = wait
                    && wait > 0
                {
                    cx.background_executor().timer(Duration::from_nanos(wait)).await;
                }
                let drawn = this.update(cx, |this, cx| {
                    let started = crate::player::clock_ns();
                    this.follow_position(cx);
                    cx.notify();
                    crate::pacing::trace::main_job("frame task", started, crate::player::clock_ns());
                });
                if drawn.is_err() {
                    break;
                }
                if wait.is_some() {
                    gpui_macos::wake_frame_sources();
                }
            }
        })
        .detach();
        // The media keys, and the end of the Now Playing tile at quit.
        subscriptions.push(crate::nowplaying::install(cx));
        let app = cx.weak_entity();
        let rows_view = cx.new(|_| crate::views::home::HomeRows { app });

        let mut this = Self {
            config,
            screen: Screen::Connect,
            session: None,
            toasts,
            player,
            player_status: PlayerStatus::default(),
            player_poll: None,
            awake: Default::default(),
            nowplaying: Default::default(),
            airplay: Default::default(),
            connect,
            views: Vec::new(),
            page: Page::Home(HomeData::default()),
            history: Vec::new(),
            history_scroll: Vec::new(),
            app_focus: cx.focus_handle(),
            profile_menu,
            search_input,
            page_scroll: ScrollHandle::new(),
            row_scrolls: RefCell::new(HashMap::new()),
            viewport_w: 1280.,
            viewport_h: 820.,
            hero_index: 0,
            hero_since: std::time::Instant::now(),
            hero_paused: None,
            hero_tick: None,
            window_save: None,
            search_debounce: None,
            catalog: Vec::new(),
            catalog_loaded: None,
            catalog_request: Revision::default(),
            hero_video: Default::default(),
            hero: Vec::new(),
            rows_view,
            hero_only_redraw: false,
            content_dirty: true,
            hero_only_frame: false,
            hero_previous: None,
            hero_changed: std::time::Instant::now(),
            player_open: false,
            player_was_open: false,
            fullscreen_before_player: false,
            current_frame: None,
            frame_seq: 0,
            frame_wake_ns: 0,
            last_draw_ns: 0,
            slider_pos: -1.,
            noticed_frame_seq: 0,
            seek_slider,
            scrubbing: false,
            audio_menu,
            settings_menu,
            volume_slider,
            volume: 100.,
            // A test instance never makes a sound (`dev/run-test` sets this).
            muted: std::env::var_os("BLOOM_MUTED").is_some(),
            speed: 1.,
            playing: None,
            segments: Vec::new(),
            queue: Default::default(),
            sort_menu,
            show_menu,
            filter_menu,
            detail_audio_menu,
            detail_subtitle_menu,
            more_menu,
            card_menu,
            admin_confirm: None,
            je_quality_tags: (false, true, true),
            admin_prompt: None,
            admin_prompt_fresh: false,
            admin_inputs,
            admin_secret: None,
            admin_tick: None,
            sync: Default::default(),
            cast: Default::default(),
            metadata: crate::metadata::State::new(window, cx),
            lists: crate::lists::State::new(window, cx),
            enhanced: Default::default(),
            stream: Default::default(),
            chromecast: Default::default(),
            downloads: Default::default(),
            connection: Default::default(),
            subtitle_menu,
            subs: Default::default(),
            tracks_version: 0,
            player_focus: cx.focus_handle(),
            playback_info: false,
            prefs: Default::default(),
            pref_lanes: Default::default(),
            episode_picker: Default::default(),
            controls_visible: true,
            window_buttons_hidden: false,
            pointer_hidden: false,
            update_said: None,
            debug_requests: None,
            debug_listener: None,
            pause_screen: false,
            pip: None,
            last_pointer_activity: std::time::Instant::now(),
            generation: 0,
            session_epoch: SessionEpoch::default(),
            revisions: Revision::default(),
            _subscriptions: subscriptions,
        };

        if let Some((server_id, user_id)) = this.config.active.clone() {
            this.open_session(&server_id, &user_id, cx);
        }
        this.connect.selected_server = this
            .config
            .active
            .as_ref()
            .map(|(s, _)| s.clone())
            .or_else(|| this.config.servers.first().map(|s| s.id.clone()));
        // The sign-in page needs the server's users and settings.
        if this.session.is_none()
            && let Some(server_id) = this.connect.selected_server.clone()
        {
            this.select_server(server_id, cx);
        }
        this.rebuild_menu(cx);
        this.start_debug_channel(window, cx);
        this.start_hero_tick(window, cx);
        this
    }

    // ----- infrastructure ---------------------------------------------------

    pub fn toast(&self, title: impl Into<String>, description: impl Into<String>, cx: &mut App) {
        let title = title.into();
        let description = description.into();
        let id = format!("toast-{}", uuid::Uuid::new_v4());
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(id, title, description, Some(Duration::from_secs(6)), cx);
        });
    }

    pub fn save_config(&self, cx: &mut App) {
        if sandboxed() {
            return;
        }
        if let Err(err) = self.config.save() {
            log::error!("saving config failed: {err:#}");
            self.toast("Could not save settings", format!("{err:#}"), cx);
        }
    }

    /// Runs blocking work with a client on a background thread and hands the
    /// result back to this view.
    pub fn fetch_with<T, W, D>(&self, client: Client, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        // The work goes to a background thread now, not when the UI thread
        // next gets to its tasks: at startup that is after the first frame.
        let work = cx.background_executor().spawn(async move { work(client) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| done(this, result, cx)).ok();
        })
        .detach();
    }

    pub fn fetch<T, W, D>(&self, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        self.fetch_with(client, cx, work, done);
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// A new revision, larger than every one before it.
    pub(crate) fn next_revision(&mut self) -> Revision {
        self.revisions.0 += 1;
        self.revisions
    }

    /// The session changes: every answer still on its way is for the old one.
    fn next_session_epoch(&mut self) {
        self.session_epoch.0 += 1;
        // The posters of the session before are of no use to the next one.
        crate::images::cancel_prewarm();
    }

    /// Ends the session in the app (the server and the config are the
    /// caller's business), so that what the old session asked for is
    /// dropped when it answers.
    pub fn drop_session(&mut self) {
        self.session = None;
        self.next_session_epoch();
    }

    // ----- sessions ---------------------------------------------------------

    pub fn open_session(&mut self, server_id: &str, user_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(server) = self.config.server(server_id) else {
            return false;
        };
        let Some(profile) = server.profiles.iter().find(|p| p.user_id == user_id) else {
            return false;
        };
        let client = Client::new(&server.url, &self.config.device_id)
            .with_server(&server.id)
            .with_session(&profile.token, &profile.user_id);
        let (id, server_name, profile) = (server.id.clone(), server.name.clone(), profile.clone());
        self.next_session_epoch();
        let user_image = profile
            .image_tag
            .as_ref()
            .map(|tag| client.user_image_url(&profile.user_id, tag));
        self.session = Some(Session {
            server_id: id,
            server_name,
            user_id: profile.user_id.clone(),
            user_name: profile.name.clone(),
            user_image,
            client,
            is_admin: false,
            audio_language: None,
        });
        if !sandboxed() {
            self.start_downloads(cx);
        }
        self.prefs = Default::default();
        self.load_prefs(cx);
        // The title index of the search loads a moment after the home page,
        // so it does not slow the first content.
        self.catalog.clear();
        self.catalog_loaded = None;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |this, cx| this.refresh_catalog(cx)).ok();
        })
        .detach();
        // The socket of the server, for SyncPlay and for live news.
        if !sandboxed() {
            self.start_sync(cx);
        }
        // The speed of the connection, for the "Auto" quality.
        self.adaptive_signed_in(cx);
        self.lists.playlists.clear();
        self.load_lists(cx);
        // The dashboard entry of the menu shows once the server confirms
        // that this user is an administrator.
        let opened = user_id.to_string();
        self.fetch(
            cx,
            |client| client.user_settings(),
            move |this, result, cx| {
                if let (Ok((is_admin, audio_language, sync_access)), Some(session)) =
                    (result, this.session.as_mut())
                    && session.user_id == opened
                {
                    session.audio_language = audio_language;
                    this.sync.access = sync_access;
                    cx.notify();
                    if is_admin {
                        session.is_admin = true;
                        this.rebuild_menu(cx);
                        cx.notify();
                    }
                }
            },
        );
        // Quality tags follow the user's Jellyfin Enhanced setting, unless
        // the profile menu of this app set them.
        crate::jellyfin::set_quality_tags(self.config.quality_tags.unwrap_or(false), true, true);
        let opened = user_id.to_string();
        self.fetch(
            cx,
            |client| Ok(client.quality_tag_settings()),
            move |this, result, cx| {
                if let (Ok(Some(settings)), Some(session)) = (result, this.session.as_ref())
                    && session.user_id == opened
                {
                    this.je_quality_tags = settings;
                    this.apply_quality_tags(cx);
                }
            },
        );
        self.load_enhanced(cx);
        self.config.active = Some((server_id.to_string(), user_id.to_string()));
        self.save_config(cx);
        self.screen = Screen::Main;
        self.history.clear();
        self.history_scroll.clear();
        self.views.clear();
        self.page = Page::Home(HomeData::default());
        // The home page loads the libraries with its own data.
        self.load_page(cx);
        self.rebuild_menu(cx);
        cx.notify();
        true
    }

    /// Ends the session on the server, removes its saved profile and shows
    /// the sign-in page of that server.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.stop_sync();
        if let Some(session) = self.session.as_ref() {
            let (server_id, user_id) = (session.server_id.clone(), session.user_id.clone());
            self.forget_profile(&server_id, &user_id, cx);
            self.select_server(server_id, cx);
        }
        // `forget_profile` dropped the session, and with it what it asked for.
        self.config.active = None;
        self.save_config(cx);
        // The pages of the user who left must not show again.
        self.history.clear();
        self.history_scroll.clear();
        self.views.clear();
        self.page = Page::Home(HomeData::default());
        self.screen = Screen::Connect;
        self.rebuild_menu(cx);
        cx.notify();
    }

    pub fn show_connect(&mut self, cx: &mut Context<Self>) {
        if let Some(server_id) = self.session.as_ref().map(|s| s.server_id.clone()) {
            self.select_server(server_id, cx);
        }
        self.connect.error = None;
        self.screen = Screen::Connect;
        cx.notify();
    }

    pub fn add_profile(&mut self, server_id: &str, profile: Profile, cx: &mut Context<Self>) {
        self.config.upsert_profile(server_id, profile.clone());
        self.save_config(cx);
        self.open_session(server_id, &profile.user_id, cx);
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let dark = !self.config.dark.unwrap_or(true);
        self.config.dark = Some(dark);
        self.save_config(cx);
        UiTheme::set(cx, theme(dark));
        self.rebuild_menu(cx);
        cx.notify();
    }

    /// Card and padding sizes for the current window width.
    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.viewport_w)
    }

    /// Scroll state of every row, for the debug channel.
    pub fn debug_rows(&self) -> String {
        self.row_scrolls
            .borrow()
            .iter()
            .map(|(id, handle)| {
                format!(
                    "{id}: offset={} max={} width={}",
                    f32::from(handle.offset().x),
                    f32::from(handle.max_offset().x),
                    f32::from(handle.bounds().size.width),
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// The scroll handle of a horizontal row, created on first use.
    pub fn row_scroll(&self, id: &SharedString) -> ScrollHandle {
        self.row_scrolls
            .borrow_mut()
            .entry(id.clone())
            .or_default()
            .clone()
    }

    /// Marks an item watched or unwatched, then reloads the page data.
    pub fn set_item_played(&mut self, item: &Item, played: bool, cx: &mut Context<Self>) {
        let item_id = item.id.clone();
        self.fetch(
            cx,
            move |client| client.set_played(&item_id, played),
            |this, result, cx| match result {
                Ok(_) => this.load_page(cx),
                Err(err) => this.toast("Could not update watched state", format!("{err:#}"), cx),
            },
        );
    }

    /// Adds an item to the favourites or removes it, then reloads the page data.
    pub fn set_item_favorite(&mut self, item: &Item, favorite: bool, cx: &mut Context<Self>) {
        let item_id = item.id.clone();
        self.fetch(
            cx,
            move |client| client.set_favorite(&item_id, favorite),
            |this, result, cx| match result {
                Ok(_) => this.load_page(cx),
                Err(err) => this.toast("Could not update favorites", format!("{err:#}"), cx),
            },
        );
    }

    // ----- navigation -------------------------------------------------------

    /// True when poster cards show quality tags: the choice in this app, or
    /// else the user's Jellyfin Enhanced setting.
    pub fn quality_tags(&self) -> bool {
        self.config.quality_tags.unwrap_or(self.je_quality_tags.0)
    }

    /// Makes the cards follow the quality tag settings. A change loads the
    /// page again, because only a query with tags on brings the streams.
    pub fn apply_quality_tags(&mut self, cx: &mut Context<Self>) {
        let on = self.quality_tags();
        let changed = on != crate::jellyfin::quality_tags();
        crate::jellyfin::set_quality_tags(on, self.je_quality_tags.1, self.je_quality_tags.2);
        self.rebuild_menu(cx);
        if changed {
            self.load_page(cx);
        }
        cx.notify();
    }

    pub fn toggle_quality_tags(&mut self, cx: &mut Context<Self>) {
        self.config.quality_tags = Some(!self.quality_tags());
        self.save_config(cx);
        self.apply_quality_tags(cx);
    }

    pub fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        // Keep the place in the page, so Back returns to it.
        self.history_scroll.push(self.page_scroll.offset());
        self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        let previous = std::mem::replace(&mut self.page, page);
        self.history.push(previous);
        if self.history.len() > 40 {
            self.history.remove(0);
            self.history_scroll.remove(0);
        }
        self.load_page(cx);
        cx.notify();
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = self.history.pop() {
            let offset = self.history_scroll.pop().unwrap_or_default();
            self.page_scroll.set_offset(offset);
            self.page = page;
            if matches!(self.page, Page::Library(_)) {
                self.rebuild_library_menus(cx);
            }
            if matches!(self.page, Page::Admin(_)) {
                self.start_admin_tick(cx);
            }
            if self.page_loading() {
                // The user left before the answer came, and the answer was
                // thrown away (`generation`); the page asks again.
                self.load_page(cx);
            } else {
                self.next_generation();
            }
            cx.notify();
        }
    }

    /// True while the current page waits for its data.
    fn page_loading(&self) -> bool {
        match &self.page {
            Page::Home(d) => d.loading,
            Page::Library(d) => d.loading,
            Page::Detail(d) => d.loading,
            Page::Search(d) => d.loading,
            Page::Admin(d) => d.loading,
            Page::Playlist(d) => d.loading,
            Page::Settings(_) | Page::Downloads => false,
        }
    }

    pub fn open_home(&mut self, cx: &mut Context<Self>) {
        self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.history.clear();
        self.history_scroll.clear();
        self.page = Page::Home(HomeData::default());
        self.load_page(cx);
        cx.notify();
    }

    pub fn open_library(&mut self, view: Item, cx: &mut Context<Self>) {
        self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.history.clear();
        self.history_scroll.clear();
        self.page = Page::Library(LibraryData::new(view));
        self.rebuild_library_menus(cx);
        self.load_page(cx);
        cx.notify();
    }

    pub fn open_favorites(&mut self, cx: &mut Context<Self>) {
        self.open_library(
            Item {
                id: FAVORITES_ID.into(),
                name: "Favorites".into(),
                collection_type: Some(FAVORITES_ID.into()),
                ..Default::default()
            },
            cx,
        );
    }

    /// Opens a random movie or series.
    pub fn open_random(&mut self, cx: &mut Context<Self>) {
        let query = ItemQuery {
            include_types: vec!["Movie".to_string(), "Series".to_string()],
            recursive: Some(true),
            sort_by: Some("Random".to_string()),
            limit: Some(1),
            ..Default::default()
        };
        self.fetch(
            cx,
            move |client| client.items(&query),
            |this, result, cx| match result {
                Ok(page) => {
                    if let Some(item) = page.items.into_iter().next() {
                        this.open_item(item, cx);
                    }
                }
                Err(err) => this.toast("Could not pick a random item", format!("{err:#}"), cx),
            },
        );
    }

    /// Opens the context menu of a card at the pointer. `row` names the home
    /// row of the card when the item can be removed from it.
    pub fn open_card_menu(
        &mut self,
        item: &Item,
        row: CardRow,
        position: gpui_kit::Point<gpui_kit::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        let mut items = Vec::new();
        if item.is_playable() || item.is_series() {
            let (handle, target) = (this.clone(), item.clone());
            items.push(
                MenuItem::new("card.menu.play", "Play").on_click(move |_, window, cx| {
                    handle
                        .update(cx, |this, cx| {
                            if target.is_series() {
                                this.play_series(&target, window, cx);
                            } else {
                                let resume = target.resume_secs() > 0;
                                this.play(&target, resume, window, cx);
                            }
                        })
                        .ok();
                }),
            );
        }
        // In a SyncPlay group an item can go to the queue of the group.
        if self.sync.in_group() && item.is_playable() {
            for (id, label, next) in [
                ("card.menu.group-next", "Play next in the group", true),
                ("card.menu.group-queue", "Add to the group queue", false),
            ] {
                let (handle, item_id) = (this.clone(), item.id.clone());
                items.push(MenuItem::new(id, label).on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| {
                            let intent = crate::syncplay::core::Intent::Enqueue {
                                item_ids: vec![item_id.clone()],
                                next,
                            };
                            this.sync_user(intent, cx);
                            this.toast("SyncPlay", "Added to the queue of the group.", cx);
                        })
                        .ok();
                }));
            }
        }
        let played = item.user_data.played;
        let (handle, target) = (this.clone(), item.clone());
        items.push(
            MenuItem::new(
                "card.menu.played",
                if played {
                    "Mark unplayed"
                } else {
                    "Mark played"
                },
            )
            .on_click(move |_, _, cx| {
                handle
                    .update(cx, |this, cx| this.set_item_played(&target, !played, cx))
                    .ok();
            }),
        );
        let favorite = item.user_data.is_favorite;
        let (handle, target) = (this.clone(), item.clone());
        items.push(
            MenuItem::new(
                "card.menu.favorite",
                if favorite {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                },
            )
            .on_click(move |_, _, cx| {
                handle
                    .update(cx, |this, cx| {
                        this.set_item_favorite(&target, !favorite, cx)
                    })
                    .ok();
            }),
        );
        if row != CardRow::None {
            let next_up = row == CardRow::NextUp;
            let (handle, item_id) = (this.clone(), item.id.clone());
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new(
                    "card.menu.hide",
                    if next_up {
                        "Remove from Next Up"
                    } else {
                        "Remove from Continue Watching"
                    },
                )
                .on_click(move |_, _, cx| {
                    let item_id = item_id.clone();
                    handle
                        .update(cx, |this, cx| {
                            this.fetch(
                                cx,
                                move |client| client.hide_from_row(next_up, &item_id),
                                |this, result, cx| match result {
                                    Ok(()) => this.load_page(cx),
                                    Err(err) => {
                                        this.toast("Could not remove the item", format!("{err:#}"), cx)
                                    }
                                },
                            )
                        })
                        .ok();
                }),
            );
        }
        let list_items = self.list_menu_items(item, cx);
        let in_collection = self.collection_card_items(item, cx);
        if !list_items.is_empty() || !in_collection.is_empty() {
            items.push(MenuItem::separator());
            items.extend(list_items);
            items.extend(in_collection);
        }
        items.extend(self.cast_menu_items(item, cx));
        items.extend(self.download_menu_items(item, cx));
        items.extend(self.hide_menu_items(item, false, cx));
        items.extend(self.metadata_menu_items(&item.id, &item.name, &item.kind, cx));
        if let Some(url) = self.session.as_ref().map(|s| s.client.web_url(&item.id)) {
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new("card.menu.web", "Open in Jellyfin Web")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        self.card_menu.update(cx, |menu, cx| {
            menu.set_items(items, cx);
            menu.open_at(Some(position), window, cx);
        });
    }

    /// Menu of a Seerr card for a title that is not in the library.
    pub fn open_seerr_menu(
        &mut self,
        item: &SeerrItem,
        position: gpui_kit::Point<gpui_kit::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        let mut items = Vec::new();
        let label = match item.status() {
            Some(2) => Some("Pending approval"),
            Some(3) => Some("Requested"),
            Some(5) => Some("Available"),
            _ => None,
        };
        match label {
            Some(text) => items.push(MenuItem::new("seerr.status", text).disabled(true)),
            None => {
                let (handle, target) = (this.clone(), item.clone());
                let text = if item.media_type == "tv" {
                    "Request all seasons"
                } else {
                    "Request"
                };
                items.push(MenuItem::new("seerr.request", text).on_click(move |_, _, cx| {
                    let target = target.clone();
                    handle
                        .update(cx, |this, cx| {
                            let name = target.display_name();
                            this.fetch(
                                cx,
                                move |client| client.seerr_request(&target),
                                move |this, result, cx| match result {
                                    Ok(()) => {
                                        this.toast("Requested", name.clone(), cx);
                                        this.load_page(cx);
                                    }
                                    Err(err) => {
                                        this.toast("Request failed", format!("{err:#}"), cx)
                                    }
                                },
                            )
                        })
                        .ok();
                }));
            }
        }
        let url = format!(
            "https://www.themoviedb.org/{}/{}",
            item.media_type, item.id
        );
        items.push(MenuItem::separator());
        items.push(
            MenuItem::new("seerr.tmdb", "Open on TMDB").on_click(move |_, _, cx| cx.open_url(&url)),
        );
        self.card_menu.update(cx, |menu, cx| {
            menu.set_items(items, cx);
            menu.open_at(Some(position), window, cx);
        });
    }

    /// Opens the library item of an id (used by Seerr cards in the library).
    pub fn open_item_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.fetch(
            cx,
            move |client| client.item(&id),
            |this, result, cx| match result {
                Ok(item) => this.open_item(item, cx),
                Err(err) => this.toast("Could not open the item", format!("{err:#}"), cx),
            },
        );
    }

    /// Lists the movies and series a person takes part in.
    pub fn open_person(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.open_folder(
            Item {
                id: id.to_string(),
                name: name.to_string(),
                collection_type: Some(PERSON_VIEW.into()),
                ..Default::default()
            },
            cx,
        );
    }

    /// Rebuilds the track menus and the "more" menu of the detail page.
    pub fn rebuild_detail_menus(&mut self, cx: &mut Context<Self>) {
        self.subs_load_rights(cx);
        let Page::Detail(data) = &self.page else {
            return;
        };
        let this = cx.weak_entity();
        let title = |stream: &crate::jellyfin::MediaStream, n: usize| {
            stream
                .display_title
                .clone()
                .unwrap_or_else(|| format!("Track {}", n + 1))
        };

        let audio_streams = data.item.streams("Audio");
        let current_audio = data
            .audio
            .or_else(|| audio_streams.iter().position(|s| s.is_default))
            .unwrap_or(0);
        let audio: Vec<MenuItem> = audio_streams
            .iter()
            .enumerate()
            .map(|(n, stream)| {
                let handle = this.clone();
                MenuItem::new(SharedString::from(format!("detail.audio.{n}")), title(stream, n))
                    .radio(n == current_audio)
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| {
                                if let Page::Detail(data) = &mut this.page {
                                    data.audio = Some(n);
                                }
                                this.rebuild_detail_menus(cx);
                                cx.notify();
                            })
                            .ok();
                    })
            })
            .collect();

        let subtitle_streams = data.item.streams("Subtitle");
        let current_subtitle = data
            .subtitle
            .unwrap_or_else(|| subtitle_streams.iter().position(|s| s.is_default));
        let pick_subtitle = |choice: Option<usize>, label: String| {
            let handle = this.clone();
            let id = match choice {
                Some(n) => format!("detail.subtitle.{n}"),
                None => "detail.subtitle.off".to_string(),
            };
            MenuItem::new(SharedString::from(id), label)
                .radio(choice == current_subtitle)
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| {
                            if let Page::Detail(data) = &mut this.page {
                                data.subtitle = Some(choice);
                            }
                            this.rebuild_detail_menus(cx);
                            cx.notify();
                        })
                        .ok();
                })
        };
        let mut subtitles = vec![pick_subtitle(None, "Off".to_string())];
        subtitles.extend(
            subtitle_streams
                .iter()
                .enumerate()
                .map(|(n, stream)| pick_subtitle(Some(n), title(stream, n))),
        );

        if let Some(search) = self.subs_search_item(&data.item.id, &data.item.display_title(), cx) {
            subtitles.push(MenuItem::separator());
            subtitles.push(search);
        }

        let web = self
            .session
            .as_ref()
            .map(|s| s.client.web_url(&data.item.id));
        let handle = this.clone();
        let mut more = vec![MenuItem::new("detail.refresh", "Refresh").on_click(
            move |_, _, cx| {
                handle.update(cx, |this, cx| this.load_page(cx)).ok();
            },
        )];
        if let Some(url) = web {
            more.push(
                MenuItem::new("detail.web", "Open in Jellyfin Web")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }

        let (id, name, kind) = (data.item.id.clone(), data.item.name.clone(), data.item.kind.clone());
        let item = data.item.clone();
        let list_items = self.list_menu_items(&item, cx);
        if !list_items.is_empty() {
            more.push(MenuItem::separator());
            more.extend(list_items);
        }
        let hide = self.hide_menu_items(&data.item.clone(), true, cx);
        more.extend(hide);
        more.extend(self.metadata_menu_items(&id, &name, &kind, cx));

        self.detail_audio_menu
            .update(cx, |menu, cx| menu.set_items(audio, cx));
        self.detail_subtitle_menu
            .update(cx, |menu, cx| menu.set_items(subtitles, cx));
        self.more_menu.update(cx, |menu, cx| menu.set_items(more, cx));
    }

    /// Plays the detail page item with the tracks picked on the page.
    pub fn play_detail(&mut self, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.page else {
            return;
        };
        let (item, audio, subtitle) = (data.item.clone(), data.audio, data.subtitle);
        if item.is_series() {
            self.play_series(&item, window, cx);
            return;
        }
        // The server gets the choice too, for a transcode.
        self.stream.wanted_audio = audio;
        self.stream.wanted_subtitle = subtitle;
        self.play(&item, resume, window, cx);
        // mpv numbers the tracks of a kind from 1 in file order.
        if let Some(n) = audio {
            self.queue.explicit_audio = true;
            self.player.set_audio(Some(n as i64 + 1));
        }
        if let Some(choice) = subtitle {
            self.queue.explicit_subtitle = true;
            self.player.set_subtitle(choice.map(|n| n as i64 + 1));
        }
    }

    pub fn open_folder(&mut self, folder: Item, cx: &mut Context<Self>) {
        self.navigate(Page::Library(LibraryData::new(folder)), cx);
        self.rebuild_library_menus(cx);
    }

    pub fn open_item(&mut self, item: Item, cx: &mut Context<Self>) {
        match item.kind.as_str() {
            "Playlist" => self.open_playlist(item, cx),
            "CollectionFolder" | "UserView" | "Folder" | "BoxSet" => self.open_folder(item, cx),
            _ => self.navigate(
                Page::Detail(DetailData {
                    item,
                    loading: true,
                    ..Default::default()
                }),
                cx,
            ),
        }
    }

    pub fn open_search(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if let Page::Search(data) = &mut self.page {
            data.typed = query.clone();
            data.query = query;
        } else {
            self.navigate(
                Page::Search(SearchData {
                    typed: query.clone(),
                    query,
                    ..Default::default()
                }),
                cx,
            );
            return;
        }
        self.load_page(cx);
        cx.notify();
    }

    /// Shows the titles that fit the text at once, from the title index.
    /// The server search for the same text replaces them when it answers.
    pub(crate) fn search_titles(&mut self, typed: String, cx: &mut Context<Self>) {
        let typed = typed.trim().to_string();
        if self.session.is_none() {
            return;
        }
        self.refresh_catalog(cx);
        let results = crate::search_index::search(&self.catalog, &typed);
        match &mut self.page {
            Page::Search(data) => {
                if data.typed == typed {
                    return;
                }
                // Back at the text of the server search before the typing
                // rested: the debounce skips a text equal to the last one.
                // The answer shows again when it came, is waited for when
                // it is on its way, and is asked for when neither (it
                // failed, or it was thrown away while the text differed).
                let back_at_query = typed == data.query;
                let on_its_way = data.loading && data.pending.as_deref() == Some(typed.as_str());
                let answered = match &data.answered {
                    Some((text, items, seerr)) if back_at_query && *text == typed => Some((items.clone(), seerr.clone())),
                    _ => None,
                };
                data.typed = typed.clone();
                self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
                if let Some((items, seerr)) = answered {
                    data.results = items;
                    data.seerr = seerr;
                    data.loading = false;
                    cx.notify();
                    return;
                }
                data.loading = !typed.is_empty();
                // Without an index the old results stay until the server answers.
                if !self.catalog.is_empty() || typed.is_empty() {
                    data.results = results;
                    data.seerr.clear();
                }
                if back_at_query && !on_its_way {
                    self.load_page(cx);
                }
            }
            _ if typed.is_empty() => return,
            _ => self.navigate(
                Page::Search(SearchData {
                    typed,
                    loading: true,
                    results,
                    ..Default::default()
                }),
                cx,
            ),
        }
        cx.notify();
    }

    /// Loads the title index, when there is none or it is older than ten
    /// minutes, and fetches the posters of its titles to the disk.
    /// Every menu of the app. A new menu must be added here (and to the
    /// observers in `new`), or it can be open next to another popup.
    fn menus(&self) -> [&Entity<MenuState>; 11] {
        [
            &self.profile_menu,
            &self.audio_menu,
            &self.settings_menu,
            &self.sort_menu,
            &self.show_menu,
            &self.filter_menu,
            &self.detail_audio_menu,
            &self.detail_subtitle_menu,
            &self.more_menu,
            &self.card_menu,
            &self.subtitle_menu,
        ]
    }

    /// Closes every popup but `keep`: the menus, the episode list of the
    /// player and the SyncPlay panel. The app shows one popup at a time;
    /// whatever opens one calls this first. A panel that is not a menu (it
    /// has no `MenuState`) must be closed here by hand.
    pub fn close_popups(
        &mut self,
        keep: Option<&Entity<MenuState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open: Vec<Entity<MenuState>> = self
            .menus()
            .into_iter()
            .filter(|menu| Some(*menu) != keep && menu.read(cx).is_open())
            .cloned()
            .collect();
        for menu in open {
            menu.update(cx, |menu, cx| menu.close(false, window, cx));
        }
        if std::mem::take(&mut self.episode_picker.open)
            | std::mem::take(&mut self.sync.panel_open)
            | std::mem::take(&mut self.cast.panel_open)
            | self.close_subs_panel()
        {
            cx.notify();
        }
        if self.close_bookmarks_panel() {
            cx.notify();
        }
    }

    /// Loads the title index again now: the server said the library changed.
    pub fn reload_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_loaded = None;
        self.refresh_catalog(cx);
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        const MAX_AGE: Duration = Duration::from_secs(600);
        if self.catalog_loaded.is_some_and(|at| at.elapsed() < MAX_AGE) {
            return;
        }
        if self.session.is_none() {
            return;
        }
        self.catalog_loaded = Some(std::time::Instant::now());
        // The answer is for the session that asked and for the newest
        // request: another profile or server may be open by then with an
        // index of its own, or the library changed and a newer request is
        // on its way. An older answer is dropped, posters and all.
        let epoch = self.session_epoch;
        let request = self.next_revision();
        self.catalog_request = request;
        let query = ItemQuery {
            include_types: vec!["Movie".into(), "Series".into()],
            recursive: Some(true),
            sort_by: Some("SortName".into()),
            ..Default::default()
        };
        self.fetch(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let page = client.items(&query)?;
                log::info!(
                    "title index: {} titles in {} ms",
                    page.items.len(),
                    started.elapsed().as_millis()
                );
                Ok(page.items)
            },
            move |this, result, cx| {
                if this.session_epoch != epoch || this.catalog_request != request {
                    return;
                }
                match result {
                    Ok(items) => {
                        if let Some(session) = &this.session {
                            let width = (this.metrics().portrait_w * 2.) as u32;
                            let posters = items
                                .iter()
                                .filter_map(|item| item.poster_url(&session.client, width))
                                .collect();
                            crate::images::prewarm(posters, cx);
                        }
                        this.catalog = items;
                    }
                    // The next search tries again.
                    Err(err) => {
                        log::warn!("title index failed: {err:#}");
                        this.catalog_loaded = None;
                    }
                }
            },
        );
    }

    /// Changes the library view (sort, filter, letter or page) and reloads it.
    pub fn update_library(&mut self, change: impl FnOnce(&mut LibraryData), cx: &mut Context<Self>) {
        if let Page::Library(data) = &mut self.page {
            let start = data.start;
            change(data);
            // A new sort, filter or letter starts again at the first page.
            if data.start == start {
                data.start = 0;
            }
            self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            self.rebuild_library_menus(cx);
            self.load_page(cx);
            cx.notify();
        }
    }

    /// Rebuilds the sort and filter dropdowns of the library toolbar.
    pub fn rebuild_library_menus(&mut self, cx: &mut Context<Self>) {
        let Page::Library(data) = &self.page else {
            return;
        };
        let this = cx.weak_entity();
        let mut sorts: Vec<MenuItem> = LIBRARY_SORTS
            .iter()
            .map(|(label, key)| {
                let handle = this.clone();
                let key: &'static str = key;
                MenuItem::new(SharedString::from(format!("library.sort.{key}")), *label)
                    .radio(data.sort == key)
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| this.update_library(|d| d.sort = key, cx))
                            .ok();
                    })
            })
            .collect();
        sorts.push(MenuItem::separator());
        for (label, descending) in [("Ascending", false), ("Descending", true)] {
            let handle = this.clone();
            sorts.push(
                MenuItem::new(SharedString::from(format!("library.order.{label}")), label)
                    .radio(data.descending == descending)
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| {
                                this.update_library(|d| d.descending = descending, cx)
                            })
                            .ok();
                    }),
            );
        }
        let mut filters: Vec<MenuItem> = vec![{
            let handle = this.clone();
            MenuItem::new("library.filter.none", "All")
                .radio(data.filter.is_none())
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| this.update_library(|d| d.filter = None, cx))
                        .ok();
                })
        }];
        for (label, key) in LIBRARY_FILTERS {
            let handle = this.clone();
            filters.push(
                MenuItem::new(SharedString::from(format!("library.filter.{key}")), label)
                    .radio(data.filter == Some(key))
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| {
                                this.update_library(|d| d.filter = Some(key), cx)
                            })
                            .ok();
                    }),
            );
        }
        let library = data.title.clone();
        let shows: Vec<MenuItem> = LibraryShow::of(data.view.collection_type.as_deref())
            .iter()
            .map(|show| {
                let (show, handle) = (*show, this.clone());
                MenuItem::new(
                    SharedString::from(format!("library.show.{show:?}")),
                    show.label(&library),
                )
                .radio(data.show == show)
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| this.update_library(|d| d.show = show, cx))
                        .ok();
                })
            })
            .collect();
        self.show_menu.update(cx, |menu, cx| menu.set_items(shows, cx));
        self.sort_menu.update(cx, |menu, cx| menu.set_items(sorts, cx));
        self.filter_menu
            .update(cx, |menu, cx| menu.set_items(filters, cx));
        self.rebuild_collection_menu(cx);
    }

    /// Plays the first item of the current library view, or a random one.
    pub fn play_library(&mut self, shuffle: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Library(data) = &self.page else {
            return;
        };
        let mut query = library_query(data);
        query.start = Some(0);
        // The first item plays; the others wait in the queue.
        query.limit = Some(30);
        if shuffle {
            query.sort_by = Some("Random".to_string());
        }
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let Ok(client) = this.read_with(cx, |this, _| {
                this.session.as_ref().map(|s| s.client.clone())
            }) else {
                return;
            };
            let Some(client) = client else { return };
            let result = cx
                .background_executor()
                .spawn(async move { client.items(&query) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    // A series in the queue plays its episodes when its turn
                    // comes; see `expand_series`.
                    Ok(page) => this.start_queue(page.items, window, cx),
                    Err(err) => this.toast("Could not start playback", format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// (Re)loads the data for the current page.
    pub fn load_page(&mut self, cx: &mut Context<Self>) {
        let generation = self.next_generation();
        // Without the server, the pages show what is on this Mac.
        if self.offline_load_page(cx) {
            return;
        }
        match &mut self.page {
            Page::Home(data) => {
                data.loading = true;
                let libraries = home_libraries(&self.views);
                let need_hero = self.hero.is_empty();
                // At sign-in the libraries are not known yet. They load in
                // the same pass as the page, so the page does not wait for
                // them first.
                let need_views = self.views.is_empty();
                self.fetch(
                    cx,
                    move |client| {
                        let started = std::time::Instant::now();
                        // The requests do not depend on each other, so they
                        // run at the same time; the page waits for the slowest
                        // one and not for their sum.
                        let loaded = std::thread::scope(|scope| {
                            let views = scope.spawn(|| {
                                if need_views {
                                    client.views().map(library_views)
                                } else {
                                    Ok(Vec::new())
                                }
                            });
                            let order = scope.spawn(|| client.home_sections());
                            let resume = scope.spawn(|| client.resume(12));
                            let next_up = scope.spawn(|| client.next_up(24));
                            let hero = scope.spawn(|| {
                                if !need_hero {
                                    return Vec::new();
                                }
                                // The hero is optional; the page loads without it.
                                let mut items =
                                    client.hero_items(HERO_ITEMS * 2).unwrap_or_default();
                                items.retain(crate::views::hero::usable);
                                items.truncate(HERO_ITEMS);
                                items
                            });
                            let join = |what: &str| anyhow::anyhow!("{what} request stopped");
                            // The latest rows need the libraries; they start
                            // as soon as those are in, while the rest runs.
                            let views = views.join().map_err(|_| join("libraries"))??;
                            let libraries = if need_views {
                                home_libraries(&views)
                            } else {
                                libraries
                            };
                            let latest = std::thread::scope(|scope| {
                                let latest: Vec<_> = libraries
                                    .iter()
                                    .map(|library| scope.spawn(|| client.latest(&library.id, 16)))
                                    .collect();
                                latest
                                    .into_iter()
                                    .map(|handle| handle.join().map_err(|_| join("latest"))?)
                                    .collect::<Result<Vec<_>>>()
                            })?;
                            let latest: Vec<(Item, Vec<Item>)> = libraries
                                .into_iter()
                                .zip(latest)
                                .filter(|(_, items)| !items.is_empty())
                                .collect();
                            anyhow::Ok((
                                views,
                                // A failed settings read falls back to the web defaults.
                                order
                                    .join()
                                    .map_err(|_| join("settings"))?
                                    .unwrap_or_else(|_| {
                                        crate::jellyfin::DEFAULT_HOME_SECTIONS
                                            .iter()
                                            .map(|s| s.to_string())
                                            .collect()
                                    }),
                                resume.join().map_err(|_| join("resume"))??,
                                next_up.join().map_err(|_| join("next up"))??,
                                latest,
                                hero.join().map_err(|_| join("hero"))?,
                            ))
                        })?;
                        let (views, order, resume, next_up, latest, hero) = loaded;
                        log::info!(
                            "home data loaded in {} ms ({} ms after start)",
                            started.elapsed().as_millis(),
                            crate::perf::since_start_ms()
                        );
                        Ok((
                            views,
                            HomeData {
                                loading: false,
                                order,
                                hero,
                                resume,
                                next_up,
                                latest,
                            },
                        ))
                    },
                    move |this, result, cx| {
                        if this.generation != generation {
                            return;
                        }
                        if let Page::Home(data) = &mut this.page {
                            match result {
                                Ok((views, mut fresh)) => {
                                    if !views.is_empty() {
                                        this.views = views;
                                    }
                                    let hero = std::mem::take(&mut fresh.hero);
                                    *data = fresh;
                                    if this.hero.is_empty() && !hero.is_empty() {
                                        this.hero = hero;
                                        this.show_hero_slide(0);
                                    }
                                }
                                Err(err) => {
                                    data.loading = false;
                                    // No server at all: the offline state takes over.
                                    if !this.downloads_server_failed(&err, cx) {
                                        this.toast("Could not load home", format!("{err:#}"), cx);
                                    }
                                }
                            }
                        }
                        cx.notify();
                    },
                );
            }
            Page::Library(data) => {
                data.loading = true;
                let query = library_query(data);
                self.fetch(
                    cx,
                    move |client| {
                        let started = std::time::Instant::now();
                        let page = client.items(&query)?;
                        log::info!(
                            "library data loaded in {} ms ({} items)",
                            started.elapsed().as_millis(),
                            page.items.len()
                        );
                        Ok(page)
                    },
                    move |this, result, cx| {
                        if this.generation != generation {
                            return;
                        }
                        if let Page::Library(data) = &mut this.page {
                            data.loading = false;
                            match result {
                                Ok(page) => {
                                    data.total = page.total_record_count;
                                    data.items = page.items;
                                }
                                Err(err) => {
                                    if !this.request_failed(&err, cx) {
                                        this.toast("Could not load library", format!("{err:#}"), cx)
                                    }
                                }
                            }
                        }
                        cx.notify();
                    },
                );
            }
            Page::Detail(data) => {
                data.loading = true;
                let item_id = data.item.id.clone();
                // The card already says what kind of item this is, so the
                // requests for its rows start with the item request.
                let kind = data.item.kind.clone();
                let series_id = data.item.series_id.clone();
                let season_id = data.item.season_id.clone();
                self.fetch(
                    cx,
                    move |client| {
                        let started = std::time::Instant::now();
                        let is = |wanted: &str| kind == wanted;
                        let (item, seasons, next_up, episodes, similar) =
                            std::thread::scope(|scope| {
                                let item = scope.spawn(|| client.item(&item_id));
                                let seasons =
                                    scope.spawn(|| match is("Series") {
                                        true => client.seasons(&item_id),
                                        false => Ok(Vec::new()),
                                    });
                                let next_up = scope.spawn(|| match is("Series") {
                                    true => client.series_next_up(&item_id).unwrap_or(None),
                                    false => None,
                                });
                                // A season shows its episodes; an episode
                                // shows the others of its season.
                                let episodes = scope.spawn(|| match (&series_id, &season_id) {
                                    (Some(series), _) if is("Season") => {
                                        client.episodes(series, &item_id)
                                    }
                                    (Some(series), Some(season)) if is("Episode") => {
                                        Ok(client.episodes(series, season).unwrap_or_default())
                                    }
                                    _ => Ok(Vec::new()),
                                });
                                // The row is optional; the page loads without it.
                                let similar = scope.spawn(|| {
                                    if is("Movie") || is("Series") {
                                        client.similar(&item_id, 12).unwrap_or_default()
                                    } else {
                                        Vec::new()
                                    }
                                });
                                let stopped = || anyhow::anyhow!("detail request stopped");
                                anyhow::Ok((
                                    item.join().map_err(|_| stopped())??,
                                    seasons.join().map_err(|_| stopped())??,
                                    next_up.join().map_err(|_| stopped())?,
                                    episodes.join().map_err(|_| stopped())??,
                                    similar.join().map_err(|_| stopped())?,
                                ))
                            })?;
                        let mut fresh = DetailData {
                            seasons,
                            next_up,
                            episodes,
                            similar,
                            ..Default::default()
                        };
                        // An item opened by id alone has no kind until now.
                        if kind.is_empty() {
                            match item.kind.as_str() {
                                "Series" => {
                                    fresh.seasons = client.seasons(&item_id)?;
                                    fresh.next_up =
                                        client.series_next_up(&item_id).unwrap_or(None);
                                }
                                "Season" => {
                                    if let Some(series) = &item.series_id {
                                        fresh.episodes = client.episodes(series, &item_id)?;
                                    }
                                }
                                _ => {}
                            }
                            if matches!(item.kind.as_str(), "Movie" | "Series") {
                                fresh.similar = client.similar(&item_id, 12).unwrap_or_default();
                            }
                        }
                        // The card that opened an episode page does not
                        // always carry the season id; the item does.
                        if item.kind == "Episode"
                            && fresh.episodes.is_empty()
                            && let (Some(series), Some(season)) = (&item.series_id, &item.season_id)
                        {
                            fresh.episodes = client.episodes(series, season).unwrap_or_default();
                        }
                        log::info!(
                            "detail data loaded in {} ms",
                            started.elapsed().as_millis()
                        );
                        fresh.item = item;
                        Ok(fresh)
                    },
                    move |this, result, cx| {
                        if this.generation != generation {
                            return;
                        }
                        if let Page::Detail(data) = &mut this.page {
                            match result {
                                Ok(fresh) => {
                                    *data = fresh;
                                    // The row of the season starts at the
                                    // episode of the page; the ones before
                                    // it are a scroll to the left away.
                                    if data.item.kind == "Episode" {
                                        let at = data
                                            .episodes
                                            .iter()
                                            .position(|ep| ep.id == data.item.id)
                                            .unwrap_or(0);
                                        let step = (this.viewport_w * 0.173).round()
                                            + crate::views::cards::CARD_GAP;
                                        let offset = at as f32 * step;
                                        this.row_scroll(&"detail.season-episodes".into())
                                            .set_offset(gpui_kit::point(px(-offset), px(0.)));
                                    }
                                    this.rebuild_detail_menus(cx);
                                    this.load_enhanced_detail(cx);
                                }
                                Err(err) => {
                                    data.loading = false;
                                    if !this.request_failed(&err, cx) {
                                        this.toast("Could not load details", format!("{err:#}"), cx)
                                    }
                                }
                            }
                        }
                        cx.notify();
                    },
                );
            }
            Page::Admin(_) => self.load_admin(generation, cx),
            Page::Playlist(_) => self.load_playlist(generation, cx),
            // The downloads page reads the index of this Mac; nothing loads.
            Page::Downloads => {}
            // The settings load with the session and stay in the app.
            Page::Settings(_) => {
                if !self.prefs.loaded {
                    self.load_prefs(cx);
                }
            }
            Page::Search(data) => {
                if data.query.is_empty() {
                    // The title index may already show results for the text
                    // in the field; the server search starts when typing rests.
                    if data.typed.is_empty() {
                        data.results.clear();
                        data.loading = false;
                    }
                    return;
                }
                data.loading = true;
                data.pending = Some(data.query.clone());
                let query = ItemQuery {
                    search: Some(data.query.clone()),
                    include_types: vec!["Movie".into(), "Series".into(), "Episode".into()],
                    recursive: Some(true),
                    limit: Some(100),
                    ..Default::default()
                };
                let text = data.query.clone();
                let asked = text.clone();
                self.fetch(
                    cx,
                    move |client| {
                        let started = std::time::Instant::now();
                        // The library and Seerr requests do not depend on each
                        // other, so they run at the same time.
                        let (page, seerr) = std::thread::scope(|scope| {
                            let page = scope.spawn(|| client.items(&query));
                            // Seerr is optional; the library results still show.
                            let seerr = scope.spawn(|| {
                                if client.seerr_active() {
                                    client.seerr_search(&text).unwrap_or_default()
                                } else {
                                    Vec::new()
                                }
                            });
                            let stopped = || anyhow::anyhow!("search request stopped");
                            anyhow::Ok((
                                page.join().map_err(|_| stopped())??,
                                seerr.join().map_err(|_| stopped())?,
                            ))
                        })?;
                        log::info!("search data loaded in {} ms", started.elapsed().as_millis());
                        Ok((page.items, seerr))
                    },
                    move |this, result, cx| {
                        if this.generation != generation {
                            return;
                        }
                        if let Page::Search(data) = &mut this.page {
                            if data.pending.as_deref() == Some(asked.as_str()) {
                                data.pending = None;
                            }
                            // The user typed on; a search for the new text
                            // follows. The answer is kept for a return to
                            // this text.
                            let shown = data.typed == data.query && data.query == asked;
                            match result {
                                Ok((items, seerr)) => {
                                    if shown {
                                        data.results = items.clone();
                                        data.seerr = seerr.clone();
                                        data.loading = false;
                                    }
                                    data.answered = Some((asked, items, seerr));
                                }
                                Err(err) => {
                                    if !shown {
                                        return;
                                    }
                                    data.loading = false;
                                    if !this.request_failed(&err, cx) {
                                        this.toast("Search failed", format!("{err:#}"), cx)
                                    }
                                }
                            }
                        }
                        cx.notify();
                    },
                );
            }
        }
    }

    // ----- user data --------------------------------------------------------

    /// Flips the watched state of the item shown on the detail page.
    pub fn toggle_played(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.page else {
            return;
        };
        let item_id = data.item.id.clone();
        let played = !data.item.user_data.played;
        let generation = self.generation;
        self.fetch(
            cx,
            move |client| client.set_played(&item_id, played),
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(user_data) => {
                        if let Page::Detail(data) = &mut this.page {
                            data.item.user_data = user_data;
                            if data.item.is_series() {
                                // Episodes and season counts changed server-side too.
                                this.load_page(cx);
                            }
                        }
                    }
                    Err(err) => {
                        this.toast("Could not update watched state", format!("{err:#}"), cx)
                    }
                }
                cx.notify();
            },
        );
    }

    /// Flips the favorite state of the item shown on the detail page.
    pub fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.page else {
            return;
        };
        let item_id = data.item.id.clone();
        let favorite = !data.item.user_data.is_favorite;
        let generation = self.generation;
        self.fetch(
            cx,
            move |client| client.set_favorite(&item_id, favorite),
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(user_data) => {
                        if let Page::Detail(data) = &mut this.page {
                            data.item.user_data = user_data;
                        }
                    }
                    Err(err) => this.toast("Could not update favorite", format!("{err:#}"), cx),
                }
                cx.notify();
            },
        );
    }

    // ----- playback ---------------------------------------------------------

    /// Plays the next unwatched episode of a series, or the first episode
    /// when everything has been watched.
    pub fn play_series(&mut self, series: &Item, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        let series_id = series.id.clone();
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<Item>> = cx
                .background_executor()
                .spawn(async move {
                    if let Some(next) = client.series_next_up(&series_id)? {
                        return Ok(Some(next));
                    }
                    let seasons = client.seasons(&series_id)?;
                    let Some(first) = seasons
                        .iter()
                        .find(|s| s.index_number != Some(0))
                        .or_else(|| seasons.first())
                    else {
                        return Ok(None);
                    };
                    let episodes = client.episodes(&series_id, &first.id)?;
                    Ok(episodes.into_iter().next())
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(Some(episode)) => {
                        let resume = episode.resume_secs() > 0;
                        this.play(&episode, resume, window, cx);
                    }
                    Ok(None) => this.toast("Nothing to play", "This series has no episodes.", cx),
                    Err(err) => this.toast("Could not start playback", format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn play(&mut self, item: &Item, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        // With a target set, the item plays there (see `src/cast`).
        if self.cast_play(std::slice::from_ref(item), resume, cx)
            || self.sync_play(std::slice::from_ref(item), resume, cx)
        {
            return;
        }
        self.clear_queue();
        self.begin(item, resume, cx);
        window.focus(&self.player_focus, cx);
        self.start_player_poll(cx);
        self.queue_followers(item, cx);
    }

    /// Starts an item in the player. The queue stays as it is, so this is
    /// also the step from one item of the queue to the next.
    pub fn begin(&mut self, item: &Item, resume: bool, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        let start_secs = if resume { item.resume_secs() } else { 0 };
        // While an item is on an AirPlay receiver, the next one goes there
        // too; the local player stays quiet.
        if self.airplay.active() {
            self.airplay_send(item, start_secs as f64, cx);
            return;
        }
        self.queue.explicit_audio = false;
        self.queue.explicit_subtitle = false;
        // A downloaded item plays from its file on this Mac; otherwise the
        // server says how the item plays (the file, or a transcode).
        self.stream_begin(item, start_secs as f64);
        self.player.set_volume(self.volume as f64);
        self.player.set_muted(self.muted);
        self.player.set_speed(self.speed as f64);
        self.apply_subtitle_style();
        self.playing = Some(item.clone());
        self.episode_picker.open = false;
        self.segments.clear();
        let item_id = item.id.clone();
        self.fetch(
            cx,
            {
                let item_id = item_id.clone();
                // The ranges are optional; playback does not need them. A
                // downloaded item has them in its `meta.json`, so a play
                // with no server still skips the intro.
                move |client| {
                    Ok(crate::downloads::stored_segments(client.server_id.as_deref(), &item_id)
                        .unwrap_or_else(|| client.media_segments(&item_id).unwrap_or_default()))
                }
            },
            move |this, result, cx| {
                if let Ok(segments) = result
                    && this.playing.as_ref().is_some_and(|i| i.id == item_id)
                {
                    this.segments = segments;
                    cx.notify();
                }
            },
        );
        self.load_timeline(item.id.clone(), cx);
        self.player_status = self.player.status();
        self.player_open = true;
        self.show_controls();
        self.tracks_version = 0;
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// When the last local input happened.
    pub fn last_activity(&self) -> std::time::Instant {
        self.last_pointer_activity
    }

    /// Called on pointer activity over the video; controls hide again after a delay.
    pub fn show_controls(&mut self) {
        self.last_pointer_activity = std::time::Instant::now();
        self.controls_visible = true;
    }

    /// Controls stay while paused, buffering, scrubbing, or when a track menu is open.
    fn controls_pinned(&self, cx: &App) -> bool {
        self.player_status.paused
            || self.player_status.state != PlayState::Playing
            || self.scrubbing
            || self.audio_menu.read(cx).is_open()
            || self.subtitle_menu.read(cx).is_open()
            || self.settings_menu.read(cx).is_open()
            || self.episode_picker.open
            || self.sync.panel_open
            || self.cast.panel_open
            || self.subs.panel_open
    }

    /// Switches the player between the full window and picture in picture.
    pub fn toggle_pip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.pip.take() {
            Some(normal) => crate::pip::leave(window, normal),
            None => {
                if window.is_fullscreen() {
                    window.toggle_fullscreen();
                }
                self.pip = crate::pip::enter(window, self.config.pip_window);
            }
        }
        self.show_controls();
        cx.notify();
    }

    /// Ends picture in picture when the player goes away.
    pub fn leave_pip(&mut self, window: &mut Window) {
        if let Some(normal) = self.pip.take() {
            crate::pip::leave(window, normal);
        }
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0., 100.);
        self.player.set_volume(self.volume as f64);
        if self.muted && self.volume > 0. {
            self.muted = false;
            self.player.set_muted(false);
        }
        cx.notify();
    }

    /// Changes the volume by a step and moves the slider with it.
    pub fn nudge_volume(&mut self, step: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = (self.volume + step).clamp(0., 100.);
        self.volume_slider
            .update(cx, |slider, cx| slider.set_value(volume, window, cx));
        self.set_volume(volume, cx);
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.player.set_muted(self.muted);
        cx.notify();
    }

    pub fn set_speed(&mut self, speed: f32, cx: &mut Context<Self>) {
        // A group plays at one speed.
        if self.sync.following() {
            self.toast("SyncPlay", "The speed stays at 1x while you play with a group.", cx);
            return;
        }
        self.speed = speed;
        self.player.set_speed(speed as f64);
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// Favourite toggle for the item in the player.
    pub fn toggle_playing_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(item) = &self.playing else {
            return;
        };
        let item_id = item.id.clone();
        let favorite = !item.user_data.is_favorite;
        self.fetch(
            cx,
            {
                let item_id = item_id.clone();
                move |client| client.set_favorite(&item_id, favorite)
            },
            move |this, result, cx| {
                match result {
                    Ok(user_data) => {
                        if let Some(item) = this.playing.as_mut().filter(|i| i.id == item_id) {
                            item.user_data = user_data;
                        }
                    }
                    Err(err) => this.toast("Could not update favorites", format!("{err:#}"), cx),
                }
                cx.notify();
            },
        );
    }

    /// The marked range the playback position is in, if any.
    pub fn current_segment(&self) -> Option<&MediaSegment> {
        let position = self.player_status.position;
        self.segments.iter().find(|s| {
            // A range under three seconds is not worth a prompt.
            s.end_secs() - s.start_secs() >= 3.
                && position >= s.start_secs()
                && position < s.end_secs() - 1.
        })
    }

    /// Shows or hides the data of the player about the video that plays.
    pub fn toggle_playback_info(&mut self, cx: &mut Context<Self>) {
        self.playback_info = !self.playback_info;
        self.player
            .command("script-binding", &["stats/display-stats-toggle"]);
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// Full screen that the user entered for an item ends when the player
    /// closes: the page it gives way to has no key and no button to leave
    /// full screen, and nobody asked for the library in full screen. A
    /// window that was full screen before the item started stays so, and
    /// the next item of a queue keeps the player, and so the full screen.
    fn follow_player_fullscreen(&mut self, window: &mut Window) {
        if self.player_open == self.player_was_open {
            return;
        }
        self.player_was_open = self.player_open;
        if self.player_open {
            self.fullscreen_before_player = window.is_fullscreen();
        } else if leaves_fullscreen_with_the_player(self.fullscreen_before_player, window.is_fullscreen()) {
            window.toggle_fullscreen();
        }
    }

    pub fn close_player_view(&mut self, cx: &mut Context<Self>) {
        self.player_open = false;
        // The preview sheets of the timeline are not needed any more.
        crate::images::preload(Vec::new(), cx);
        self.news_player_closed(cx);
        cx.notify();
    }

    /// The seek slider and the time label follow the position, by whole
    /// quarter seconds, in the draw of a frame (see the frames task): while
    /// frames flow, the poll asks for no draw of its own for the position.
    fn follow_position(&mut self, cx: &mut Context<Self>) {
        let status = &self.player_status;
        if self.scrubbing || status.duration <= 0. {
            return;
        }
        let pos = (status.position * 4.).floor() / 4.;
        if pos == self.slider_pos {
            return;
        }
        self.slider_pos = pos;
        let (pos, dur) = (status.position as f32, status.duration as f32);
        self.seek_slider.update(cx, |s, cx| {
            // Update in place: a fresh state has no bounds, and a click
            // before the next paint would divide by zero.
            *s = std::mem::replace(s, SliderState::new())
                .min(0.)
                .max(dur)
                .step(1.)
                .default_value(pos.clamp(0., dur));
            cx.notify();
        });
    }

    /// Measures how long a new frame waited for the UI, once for a frame.
    fn note_frame_wait(&mut self) {
        let seq = self.player.frame_seq();
        if seq != self.noticed_frame_seq {
            self.noticed_frame_seq = seq;
            crate::perf::video_frame_noticed(self.player.frame_wait_ms());
        }
    }

    /// Swaps in the newest video frame. With display pacing (`pacing.rs`)
    /// a frame is shown one tick of the display after its own.
    fn sync_frame(&mut self, window: &mut Window) {
        self.player.ui_seen();
        let display = self.player.display_pacing();
        if display && self.player_open {
            crate::pacing::note_window(window, &self.player);
        }
        let prev_draw = std::mem::replace(&mut self.last_draw_ns, crate::player::clock_ns());
        // A closed player draws no frame: the surface goes back to the pool
        // at once, not when the next item plays.
        if !self.player_open {
            self.current_frame = None;
        }
        if self.player.frame_seq() == self.frame_seq {
            return;
        }
        // One read of the frame, its number and its tick: a frame that is
        // published between two reads cannot be shown with the tick of the
        // one before.
        let published = self.player.frame();
        let seq = published.seq;
        if seq == self.frame_seq {
            return;
        }
        let draw_id = gpui_apple::present_trace::next_draw_id();
        if display {
            let (now, period) = self.player.clock_time();
            // The frame of this tick came before this draw: it waits for
            // the draw the frames task asks for, like every frame.
            if !crate::pacing::due(published.tick_ns, now, period) {
                crate::pacing::count_held();
                crate::pacing::trace::held(seq);
                window.request_animation_frame();
                return;
            }
            if crate::pacing::late(published.tick_ns, now, period) {
                crate::pacing::count_late();
            }
            // The phase loop hears where this draw comes out.
            crate::pacing::phase::adopted(seq, draw_id, published.tick_ns, published.vsync_ns, period, now);
        }
        self.frame_seq = seq;
        crate::pacing::trace::adopted(seq, self.frame_wake_ns, self.last_draw_ns, prev_draw, draw_id);
        crate::perf::video_frame_shown();
        self.current_frame = if self.player_open { published.frame } else { None };
    }

    /// Rebuilds the audio and subtitle menus from the current track list.
    pub fn rebuild_track_menus(&mut self, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let status = &self.player_status;
        let build = |kind: &str, none_label: &str, set: fn(&Player, Option<i64>)| {
            let mut items = Vec::new();
            let tracks: Vec<&crate::player::Track> =
                status.tracks.iter().filter(|t| t.kind == kind).collect();
            let any_selected = tracks.iter().any(|t| t.selected);
            if kind == "sub" {
                let handle = this.clone();
                items.push(
                    MenuItem::new(
                        gpui_kit::SharedString::from(format!("track.{kind}.none")),
                        none_label,
                    )
                    .radio(!any_selected)
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |t, _| set(&t.player, None)).ok();
                    }),
                );
            }
            for track in tracks {
                let id = track.id;
                let handle = this.clone();
                items.push(
                    MenuItem::new(
                        gpui_kit::SharedString::from(format!("track.{kind}.{id}")),
                        track.label(),
                    )
                    .radio(track.selected)
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |t, _| set(&t.player, Some(id))).ok();
                    }),
                );
            }
            if items.is_empty() {
                items.push(
                    MenuItem::new(
                        gpui_kit::SharedString::from(format!("track.{kind}.empty")),
                        "No tracks",
                    )
                    .disabled(true),
                );
            }
            items
        };
        // While a transcode plays, the server picks the tracks.
        let audio = self
            .stream_track_items("audio", cx)
            .unwrap_or_else(|| build("audio", "", |p, id| p.set_audio(id)));
        let subs = self
            .stream_track_items("sub", cx)
            .unwrap_or_else(|| build("sub", "Off", |p, id| p.set_subtitle(id)));
        let mut audio = audio;
        audio.push(MenuItem::separator());
        audio.push(self.timing_menu_item(crate::subtitles::Delay::Audio, cx));
        let mut subs = subs;
        subs.push(MenuItem::separator());
        subs.push(self.timing_menu_item(crate::subtitles::Delay::Subtitle, cx));
        if let Some((id, title)) = self.subs_playing()
            && let Some(search) = self.subs_search_item(&id, &title, cx)
        {
            subs.push(search);
        }
        self.subs_load_rights(cx);
        let mut settings = vec![MenuItem::label("settings.audio", "Audio")];
        settings.extend(audio.clone());
        settings.push(MenuItem::separator());
        settings.push(self.quality_menu_item(cx));
        settings.push(MenuItem::separator());
        settings.push(MenuItem::label("settings.speed", "Playback speed"));
        for speed in [0.5_f32, 0.75, 1., 1.25, 1.5, 2.] {
            let handle = this.clone();
            settings.push(
                MenuItem::new(
                    gpui_kit::SharedString::from(format!("settings.speed.{speed}")),
                    format!("{speed}x"),
                )
                .radio(self.speed == speed)
                .on_click(move |_, _, cx| {
                    handle.update(cx, |t, cx| t.set_speed(speed, cx)).ok();
                }),
            );
        }
        settings.push(MenuItem::separator());
        let handle = this.clone();
        settings.push(
            MenuItem::new("settings.info", "Playback info")
                .checked(self.playback_info)
                .on_click(move |_, _, cx| {
                    handle.update(cx, |t, cx| t.toggle_playback_info(cx)).ok();
                }),
        );
        self.settings_menu
            .update(cx, |menu, cx| menu.set_items(settings, cx));
        self.audio_menu
            .update(cx, |menu, cx| menu.set_items(audio, cx));
        self.subtitle_menu
            .update(cx, |menu, cx| menu.set_items(subs, cx));
    }

    pub(crate) fn start_player_poll(&mut self, cx: &mut Context<Self>) {
        self.player_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let fast = this
                    .read_with(cx, |this, _| this.player_open)
                    .unwrap_or(false);
                // The frames have their own signal; only the old pacing
                // mode finds them through this poll, and needs its 8 ms.
                // Everything else here moves by quarter seconds or slower,
                // or is a change the eye sees within 50 ms (a pause, the
                // end, a seek's new position; the frame of a seek comes
                // through the frames task).
                let interval = match (fast, crate::player::old_pacing()) {
                    (true, true) => 8,
                    (true, false) => 50,
                    (false, _) => 500,
                };
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        let poll_started = crate::player::clock_ns();
                        let status = this.player.status();
                        let ended = status.state == PlayState::Ended;
                        // The frames have their own signal; the poll only
                        // sees them with the timing of before.
                        let frame_changed = crate::player::old_pacing()
                            && this.player.frame_seq() != this.frame_seq;
                        if frame_changed {
                            this.note_frame_wait();
                        }
                        // The seek slider and the time label move by whole
                        // seconds; a redraw for each 8 ms change of the
                        // position cost the whole UI about 48 draws a
                        // second. `player_status` itself stays current for
                        // everything that reads it.
                        let position_moved = (status.position * 4.).floor()
                            != (this.player_status.position * 4.).floor();
                        // While frames come, each of them draws the UI and
                        // moves the slider (`follow_position`), so a draw
                        // for the position alone is not needed: it lands at
                        // the tick, misses the compositor and pushes the
                        // next frame a refresh late (see pacing.rs).
                        let frames_flow = this.player.display_pacing()
                            && status.state == PlayState::Playing
                            && !status.paused
                            && this.player.frame_wait_ms() < 100.;
                        let changed = frame_changed
                            || (position_moved && !frames_flow)
                            || status.paused != this.player_status.paused
                            || status.buffering != this.player_status.buffering
                            || status.state != this.player_status.state
                            || status.tracks_version != this.player_status.tracks_version;
                        this.player_status = status;
                        this.subs_status_changed(cx);
                        if this.player_status.tracks_version != this.tracks_version {
                            this.tracks_version = this.player_status.tracks_version;
                            this.rebuild_track_menus(cx);
                        }
                        // The slider follows the position; an update with no
                        // change would only cause a redraw.
                        if changed && !this.scrubbing && this.player_status.duration > 0. {
                            let (pos, dur) = (
                                this.player_status.position as f32,
                                this.player_status.duration as f32,
                            );
                            this.seek_slider.update(cx, |s, cx| {
                                // Update in place: a fresh state has no bounds, and a
                                // click before the next paint would divide by zero.
                                *s = std::mem::replace(s, SliderState::new())
                                    .min(0.)
                                    .max(dur)
                                    .step(1.)
                                    .default_value(pos.clamp(0., dur));
                                cx.notify();
                            });
                        }
                        this.enhanced_tick(cx);
                        this.adaptive_tick(cx);
                        // At the end of a file the queue goes on.
                        // In a group the group decides what comes after an item.
                        let ended = ended && !this.sync.holds_player() && !this.advance_at_end(cx);
                        if ended {
                            this.player_open = false;
                            if let Some(err) = this.player_status.error.clone() {
                                this.toast("Playback stopped", err, cx);
                            }
                            this.player.acknowledge_end();
                            this.player_status = this.player.status();
                            this.load_page(cx);
                            this.news_player_closed(cx);
                        }
                        if changed || ended {
                            this.macos_player_changed(cx);
                        }
                        let idle = this.last_pointer_activity.elapsed() > CONTROLS_HIDE_AFTER;
                        let should_show = !this.player_open || !idle || this.controls_pinned(cx);
                        let controls_changed = should_show != this.controls_visible;
                        this.controls_visible = should_show;
                        // The pause screen comes up after a rest with no input.
                        let pause_screen = this.player_open
                            && this.pip.is_none()
                            && this.player_status.paused
                            && this.player_status.state == PlayState::Playing
                            && this.last_pointer_activity.elapsed() > PAUSE_SCREEN_AFTER;
                        let pause_changed = pause_screen != this.pause_screen;
                        this.pause_screen = pause_screen;
                        if changed || ended || controls_changed || pause_changed {
                            cx.notify();
                        }
                        crate::pacing::trace::main_job("player poll", poll_started, crate::player::clock_ns());
                        !ended
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }
}

pub fn library_query(data: &LibraryData) -> ItemQuery {
    let view = &data.view;
    let favorites = view.collection_type.as_deref() == Some(FAVORITES_ID);
    let person = view.collection_type.as_deref() == Some(PERSON_VIEW);
    let include_types = match view.collection_type.as_deref() {
        Some("movies") => vec!["Movie".to_string()],
        Some("tvshows") => vec!["Series".to_string()],
        Some(FAVORITES_ID) | Some(PERSON_VIEW) => {
            vec!["Movie".to_string(), "Series".to_string()]
        }
        Some(crate::lists::PLAYLISTS_ID) => vec!["Playlist".to_string()],
        _ => Vec::new(),
    };
    let include_types = match data.show {
        LibraryShow::Collections => vec!["BoxSet".to_string()],
        LibraryShow::Episodes => vec!["Episode".to_string()],
        _ => include_types,
    };
    // The collections are not inside the movie library.
    let collections = data.show == LibraryShow::Collections;
    let playlists = view.collection_type.as_deref() == Some(crate::lists::PLAYLISTS_ID);
    let recursive = !include_types.is_empty();
    let filters: Vec<&str> = (favorites || data.show == LibraryShow::Favorites)
        .then_some("IsFavorite")
        .into_iter()
        .chain(data.filter.filter(|f| *f != "IsFavorite" || data.show != LibraryShow::Favorites))
        .collect();
    ItemQuery {
        parent_id: (!favorites && !person && !collections && !playlists).then(|| view.id.clone()),
        person_id: person.then(|| view.id.clone()),
        filters: (!filters.is_empty()).then(|| filters.join(",")),
        include_types,
        recursive: Some(recursive),
        sort_by: Some(data.sort.to_string()),
        sort_order: Some(
            if data.descending {
                "Descending"
            } else {
                "Ascending"
            }
            .to_string(),
        ),
        name_starts_with: data.letter.filter(|c| *c != '#').map(|c| c.to_string()),
        name_less_than: (data.letter == Some('#')).then(|| "A".to_string()),
        limit: Some(PAGE_SIZE),
        start: Some(data.start),
        ..Default::default()
    }
}

/// The user's views that the app shows as libraries.
fn library_views(views: Vec<Item>) -> Vec<Item> {
    views
        .into_iter()
        .filter(|v| {
            !matches!(
                v.collection_type.as_deref(),
                Some("playlists") | Some("livetv") | Some("books") | Some("music")
            )
        })
        .collect()
}

/// The libraries with a "Recently Added" row on the home page.
fn home_libraries(views: &[Item]) -> Vec<Item> {
    views
        .iter()
        .filter(|v| {
            matches!(
                v.collection_type.as_deref(),
                Some("movies") | Some("tvshows") | Some("homevideos") | None
            )
        })
        .cloned()
        .collect()
}

/// Whether the window leaves full screen as the player closes: only when
/// it is full screen now and was not before the item started.
fn leaves_fullscreen_with_the_player(fullscreen_before: bool, fullscreen_now: bool) -> bool {
    fullscreen_now && !fullscreen_before
}

#[cfg(test)]
mod fullscreen_tests {
    use super::leaves_fullscreen_with_the_player as leaves;

    #[test]
    fn full_screen_that_began_with_the_player_ends_with_it() {
        assert!(leaves(false, true), "entered during the item: back to the window");
        assert!(!leaves(true, true), "the window was full screen before: it stays");
        assert!(!leaves(false, false), "left during the item: nothing to do");
        assert!(!leaves(true, false), "left during the item: it is not put back");
    }
}

impl Render for Bloom {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        let started_cpu = crate::perf::thread_cpu_ns();
        self.hero_only_frame = !std::mem::take(&mut self.content_dirty);
        self.sync_frame(window);
        self.viewport_w = f32::from(window.viewport_size().width);
        self.viewport_h = f32::from(window.viewport_size().height);
        // Picture in picture ends with the player.
        if !self.player_open {
            self.leave_pip(window);
        }
        self.follow_player_fullscreen(window);
        // The window buttons go away with the controls of the player.
        // Picture in picture hides them by itself.
        let buttons_hidden = self.player_open && self.pip.is_none() && !self.controls_visible;
        if buttons_hidden != self.window_buttons_hidden {
            self.window_buttons_hidden = buttons_hidden;
            crate::pip::set_window_buttons_hidden(window, buttons_hidden);
        }
        // The pointer goes away with the controls, also in picture in
        // picture. macOS shows it again when the mouse moves, and a move
        // brings the controls back. A key can bring the controls back
        // with no move, so the pointer is given back then.
        let pointer_hidden =
            self.player_open && !self.controls_visible && window.is_window_active();
        if pointer_hidden != self.pointer_hidden {
            self.pointer_hidden = pointer_hidden;
            crate::macos::hide_pointer_until_it_moves(pointer_hidden);
        }
        // The main screen holds the focus when nothing else does, so its
        // keyboard shortcuts work without a click first.
        if self.screen == Screen::Main
            && !self.player_open
            && self.connect.authorize.is_none()
            && (window.focused(cx).is_none() || self.player_focus.is_focused(window))
        {
            window.focus(&self.app_focus, cx);
        }
        if std::mem::take(&mut self.connect.clear_password) {
            self.connect
                .password
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if std::mem::take(&mut self.sync.focus_player) && self.player_open {
            window.focus(&self.player_focus, cx);
        }
        self.prepare_admin_prompt(window, cx);
        self.prepare_metadata(window, cx);
        self.prepare_lists(window, cx);
        self.prepare_cast(window, cx);
        let theme = UiTheme::read(cx).clone();
        let content = match self.screen {
            Screen::Connect => self.render_connect(window, cx).into_any_element(),
            Screen::Main => self.render_shell(window, cx).into_any_element(),
        };
        crate::perf::record_render(started.elapsed());
        use crate::menus;
        use gpui_kit::{Focusable as _, InteractiveElement as _};
        div()
            .relative()
            .size_full()
            // Menu bar actions for this window.
            .on_action(cx.listener(|_, _: &menus::Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &menus::Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &menus::ToggleFullScreen, window, _| {
                window.toggle_fullscreen()
            }))
            .on_action(cx.listener(|this, _: &menus::Back, _, cx| {
                if !this.player_open {
                    this.back(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::About, _, cx| {
                if !this.player_open && this.screen == Screen::Main {
                    this.open_settings(crate::settings::Section::About, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::CheckForUpdates, _, cx| {
                this.check_for_updates(cx)
            }))
            .on_action(cx.listener(|this, _: &menus::Home, _, cx| {
                if !this.player_open && this.screen == Screen::Main {
                    this.open_home(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::Random, _, cx| {
                if !this.player_open && this.screen == Screen::Main {
                    this.open_random(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::Reload, _, cx| {
                if this.screen == Screen::Main {
                    this.load_page(cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &menus::Search, window, cx| {
                if !this.player_open && this.screen == Screen::Main {
                    let query = this.search_input.read(cx).value().to_string();
                    this.open_search(query, cx);
                    let focus = this.search_input.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &menus::PlayPause, _, cx| {
                if this.player_open {
                    this.request_toggle_pause(cx);
                } else {
                    this.toggle_hero_pause();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &menus::PictureInPicture, window, cx| {
                if this.player_open {
                    this.toggle_pip(window, cx)
                }
            }))
            .bg(theme.colors.background)
            .text_color(theme.colors.foreground)
            .font_family(theme.fonts.body.clone())
            .text_size(px(14.))
            .line_height(px(20.))
            .child(content)
            .child(self.card_menu.clone())
            .children(self.render_metadata(window, cx))
            .children(self.render_lists_dialog(cx))
            .children(self.render_authorize(cx))
            .children(
                (!self.player_open)
                    .then(|| self.render_sync_panel(Some(crate::views::shell::TOPBAR_H + 4.), cx))
                    .flatten(),
            )
            .children(
                (!self.player_open)
                    .then(|| self.render_cast_panel(Some(crate::views::shell::TOPBAR_H + 4.), cx))
                    .flatten(),
            )
            .children(
                (!self.player_open)
                    .then(|| self.render_subs_panel(Some(crate::views::shell::TOPBAR_H + 4.), cx))
                    .flatten(),
            )
            .children(self.render_enhanced_help(cx))
            .children(self.render_seasons_dialog(cx))
            // A test instance says so in its window, so nobody takes it for
            // the app in use (`dev/run-test` sets the name).
            .children(std::env::var("BLOOM_TEST_NAME").ok().map(|name| {
                div()
                    .absolute()
                    // Under the top bar, clear of its buttons and of the title
                    // of the player.
                    .top(px(52.))
                    .left(px(20.))
                    .px(px(10.))
                    .py(px(3.))
                    .rounded(px(8.))
                    .bg(gpui_kit::rgba(0xd9480fe6))
                    .text_size(px(12.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(gpui_kit::rgb(0xffffff))
                    .child(format!("TEST INSTANCE · {name}"))
            }))
            .child(self.toasts.clone())
            // Painted last, so its paint marks the end of the frame's work.
            .child(gpui_kit::canvas(
                |_, _, _| {},
                move |_, _, _, _| crate::perf::record_draw(started, started_cpu),
            ))
    }
}

/// The real `Bloom` against a small HTTP server, for the races between a
/// request on its way and what the user does meanwhile.
#[cfg(test)]
pub(crate) mod race_harness;
#[cfg(test)]
mod race_tests;
