// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Root view: session, navigation, background fetching, player polling.

use crate::quality_tags::QualityApi as _;
use std::{cell::RefCell, collections::HashMap, time::Duration};

use anyhow::Result;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, Styled, Task, Window, div, px, rgb, rgba,
};

use crate::{
    config::{Config, Profile},
    jellyfin::{Client, Item, ItemKind, ItemQuery, SeerrItem, MediaSegment},
    player::{PlayState, Player, PlayerStatus},
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

// The data of the pages and their loading have no window: `bloom_core::pages`.
pub use crate::pages::{
    DetailData, FAVORITES_ID, HomeData, LIBRARY_FILTERS, LIBRARY_SORTS, LibraryData,
    LibraryShow, PAGE_SIZE, PERSON_VIEW, home_libraries, library_query,
};

/// Time one hero slide stays on screen.
pub const HERO_SLIDE: Duration = Duration::from_millis(7000);
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
    let accent = crate::ui::tone::INK;
    let ink = crate::ui::tone::ON_ACCENT;
    if dark {
        t.colors.background = rgb(0x101010);
        t.colors.foreground = rgba(0xffffffde);
        t.colors.popover = rgb(0x2a2a2a);
        t.colors.popover_foreground = accent;
        t.colors.muted = rgb(0x1c1c1c);
        t.colors.muted_foreground = rgba(0xffffff99);
        t.colors.border = rgba(0xf5f5f729);
        t.colors.input = rgba(0xffffff17);
        t.colors.primary = accent;
        t.colors.primary_foreground = ink;
        t.colors.ring = crate::ui::tone::INK_60;
        t.colors.destructive = rgb(0xf92672);
    } else {
        t.colors.background = rgb(0xfafafa);
        // Liquid glass prototype: a dimmer ground for the screenshots.
        if let Some(ground) = crate::ui::glass::ground() {
            t.colors.background = ground;
        }
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

/// The home row a card sits in, for the "remove from row" menu entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardRow {
    None,
    Resume,
    NextUp,
}

/// What `Bloom::set_user_data` sets for the user on an item.
#[derive(Clone, Copy)]
enum Mark {
    Played,
    Favorite,
}


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

/// A page and its data. The two large ones are boxed: each entry of the
/// Back history holds a page, and a detail page's data is about 2 KB.
pub enum Page {
    Home(HomeData),
    Library(LibraryData),
    Detail(Box<DetailData>),
    Search(SearchData),
    /// Server dashboard for administrators.
    Admin(Box<crate::admin::AdminData>),
    /// Settings of the signed-in user.
    Settings(crate::settings::Section),
    /// One playlist, with its entries in order.
    Playlist(crate::lists::PlaylistData),
    /// The files downloaded for offline use.
    Downloads,
}

/// A popup that is not a menu (it has no `MenuState`). `Bloom::panel` holds
/// the open one, so two panels are never open next to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// Episode list of the player.
    Episodes,
    /// The list of keys of Jellyfin Enhanced.
    Help,
    SyncPlay,
    Cast,
    /// Search for subtitles.
    Subtitles,
    Bookmarks,
    /// The dialog of "Request more seasons" (src/seasons.rs).
    Seasons,
}

/// The player as the window holds it.
pub struct PlayerUi {
    pub(crate) status: PlayerStatus,
    pub(crate) poll: Option<Task<()>>,
    /// Whether the embedded video view covers the content area.
    pub(crate) open: bool,
    /// `open` at the frame before, and whether the window was full
    /// screen when the player opened: full screen that began with the
    /// player ends with it (`follow_player_fullscreen`).
    pub(crate) was_open: bool,
    pub(crate) fullscreen_before: bool,
    pub(crate) frame: Option<VideoFrame>,
    pub(crate) frame_seq: u64,
    /// When the frames task last woke, and when the last draw ran (pacing
    /// trace).
    pub(crate) frame_wake_ns: u64,
    pub(crate) last_draw_ns: u64,
    /// The position the seek slider was last set to by the frames task.
    pub(crate) slider_pos: f64,
    /// The newest frame whose wait was measured (`dev/jctl pacing`).
    pub(crate) noticed_frame_seq: u64,
    pub(crate) seek_slider: Entity<SliderState>,
    pub(crate) scrubbing: bool,
    pub(crate) audio_menu: Entity<MenuState>,
    /// Gear menu of the player: audio track and playback speed.
    pub(crate) settings_menu: Entity<MenuState>,
    pub(crate) subtitle_menu: Entity<MenuState>,
    pub(crate) volume_slider: Entity<SliderState>,
    /// Player volume from 0 to 100.
    pub(crate) volume: f32,
    pub(crate) muted: bool,
    pub(crate) speed: f32,
    /// The item in the player, for its rating, end time and favourite state.
    pub(crate) playing: Option<Item>,
    /// Intro and credits ranges of the item in the player.
    pub(crate) segments: Vec<MediaSegment>,
    pub(crate) tracks_version: u64,
    pub(crate) focus: FocusHandle,
    /// The player shows its data about the video: codecs, bitrates,
    /// dropped frames, cache.
    pub(crate) info: bool,
    /// Whether the player overlay controls are shown; hidden after pointer idle.
    pub(crate) controls_visible: bool,
    /// The close, minimise and zoom buttons are hidden (see the root render).
    pub(crate) window_buttons_hidden: bool,
    /// The pointer is hidden with the controls of the player.
    pub(crate) pointer_hidden: bool,
    /// The paused player shows the item's details in place of the controls.
    pub(crate) pause_screen: bool,
    /// Frame of the normal window while the player is in picture in
    /// picture; `None` when it is not.
    pub(crate) pip: Option<crate::pip::Frame>,
    pub(crate) last_activity: std::time::Instant,
}

/// The page on show, the pages Back and Forward return to, and the scroll places.
pub struct Navigation {
    pub(crate) page: Page,
    /// The pages Back returns to, nearest last, each with its scroll place.
    pub(crate) history: Vec<(Page, gpui_kit::Point<gpui_kit::Pixels>)>,
    /// The pages Back left, nearest last, for Forward; a new page empties it.
    pub(crate) forward: Vec<(Page, gpui_kit::Point<gpui_kit::Pixels>)>,
    /// True when the page came from Back: it fades in on a phone instead
    /// of sliding in from the right (`ui::motion`, The Quiet Return Rule).
    pub(crate) back: bool,
    /// Scroll position of the current page.
    pub(crate) scroll: ScrollHandle,
    pub(crate) generation: u64,
    /// Scroll positions of the horizontal rows, by row id.
    pub(crate) rows: RefCell<HashMap<SharedString, ScrollHandle>>,
    /// The glide of each row its arrows started (`cards::page_row`).
    pub(crate) glides: crate::views::cards::RowGlides,
    /// Key of the card under the pointer (`cards::card`); a change of the
    /// page clears it (`leave_page`).
    pub(crate) hovered_card: Option<SharedString>,
}

impl Navigation {
    /// The page on show and every page Back and Forward return to.
    pub(crate) fn pages_mut(&mut self) -> impl Iterator<Item = &mut Page> {
        std::iter::once(&mut self.page)
            .chain(self.history.iter_mut().map(|(page, _)| page))
            .chain(self.forward.iter_mut().map(|(page, _)| page))
    }
}

/// The search field and the title index that answers it with no request.
pub struct TitleSearch {
    pub(crate) input: Entity<InputState>,
    /// Pending search for the text in the search field.
    pub(crate) debounce: Option<Task<()>>,
    /// Titles of the movies and shows of the server, for a search with no
    /// request, and the time they were loaded.
    pub(crate) catalog: crate::search_index::Index,
    pub(crate) loaded: Option<std::time::Instant>,
    /// The request of the title index whose answer is wanted.
    pub(crate) request: Revision,
}

/// The menus of the pages and the one open panel: one popup at a time.
pub struct Popups {
    pub(crate) profile_menu: Entity<MenuState>,
    pub(crate) sort_menu: Entity<MenuState>,
    /// Views of the library, under its title.
    pub(crate) show_menu: Entity<MenuState>,
    pub(crate) filter_menu: Entity<MenuState>,
    pub(crate) detail_audio_menu: Entity<MenuState>,
    pub(crate) detail_subtitle_menu: Entity<MenuState>,
    pub(crate) more_menu: Entity<MenuState>,
    /// Context menu of the card under the pointer.
    pub(crate) card_menu: Entity<MenuState>,
    /// The "…" menu of a row or a card of the dashboard (`open_admin_menu`).
    pub(crate) admin_menu: Entity<MenuState>,
    /// The open panel that is not a menu; see [`Panel`]. `open_panel` sets
    /// it and nothing else does, so a panel never opens next to a menu.
    pub(crate) panel: Option<Panel>,
    /// The panel a click outside of it closed, and when (`toggle_panel`).
    pub(crate) panel_dismissed: Option<(Panel, std::time::Instant)>,
}

pub struct Bloom {
    pub(crate) config: Config,
    pub(crate) screen: Screen,
    pub(crate) session: Option<Session>,
    pub(crate) toasts: Entity<ToastState>,
    pub(crate) player: Player,
    /// The display-sleep assertion while a video plays (src/awake.rs).
    pub(crate) awake: crate::awake::Awake,
    /// The Now Playing tile and the media keys (src/nowplaying.rs).
    pub(crate) nowplaying: crate::nowplaying::State,
    /// The AirPlay sender; idle until an item is sent.
    pub(crate) airplay: crate::airplay::AirPlay,
    pub(crate) connect: ConnectState,
    /// The splash at the start (src/views/splash.rs).
    pub(crate) splash: crate::views::splash::Splash,
    pub(crate) views: Vec<Item>,
    /// Whether the menu bar reads "Exit Full Screen" now.
    menus_fullscreen: bool,
    /// Focus of the main screen, so its keyboard shortcuts work.
    pub(crate) app_focus: FocusHandle,
    /// The field that narrows the Users section of the dashboard by name.
    pub(crate) admin_users_filter: Entity<InputState>,
    /// The fields that find a task and a log file on their sections.
    pub(crate) admin_tasks_filter: Entity<InputState>,
    pub(crate) admin_logs_filter: Entity<InputState>,
    /// A phone shows the Settings page as a list of its sections first;
    /// a row opens the section, and the back arrow returns to the list.
    pub(crate) settings_list: bool,
    /// The copy of the video frame the glass last drew (`video_backdrop_version`).
    pub(crate) backdrop_seen: u64,
    /// Window width at the last render; card sizes follow it.
    pub(crate) viewport_w: f32,
    pub(crate) viewport_h: f32,
    /// The hero of the home page: its slides, the one on show and its
    /// trailer. Kept for the session, so it survives navigation.
    pub(crate) hero: crate::views::hero::Hero,
    /// How often a save of the config was asked for; a test reads it (a
    /// test writes no config file).
    pub(crate) config_saves: std::cell::Cell<u64>,
    /// Pending write of the window place to the config file.
    window_save: Option<Task<()>>,
    /// The home rows as a view GPUI can cache between frames.
    pub(crate) rows_view: Entity<crate::views::home::HomeRows>,
    /// True when something other than the hero asked for a redraw since the
    /// last render.
    content_dirty: bool,
    /// Items that follow in the player, and its chapters and previews.
    pub(crate) queue: crate::queue::PlayQueue,
    /// Quality tag settings of the user in Jellyfin Enhanced: on, resolution
    /// tag, dynamic range tag.
    pub(crate) je_quality_tags: (bool, bool, bool),
    /// The question, the form and the value shown once that wait for the
    /// user. The dashboard, the settings and the downloads open them.
    pub(crate) dialogs: crate::admin::dialogs::Dialogs,
    /// The loads of the live part of the dashboard, and their timer.
    pub(crate) admin_live: crate::admin::Live,
    /// SyncPlay: the session with the server and the panel of the groups.
    pub(crate) sync: crate::syncplay::ui::SyncState,
    /// The socket of the server and its news (src/socket.rs).
    pub(crate) socket: crate::socket::ServerSocket,
    /// Remote control of and by other devices ("Play On").
    pub(crate) cast: crate::cast::CastState,
    /// Metadata manager: its dialog and the editing state of its fields.
    pub(crate) metadata: crate::metadata::State,
    /// Playlists and collections: the dialog and the playlists of the user.
    pub(crate) lists: crate::lists::State,
    /// Features of the Jellyfin Enhanced plugin.
    pub(crate) enhanced: crate::enhanced::State,
    /// Playback negotiation: the tracks chosen for the next item.
    pub(crate) stream: crate::stream::State,
    /// Google Cast: the devices and the one connection.
    pub(crate) chromecast: crate::chromecast::State,
    /// Downloads for offline use and the offline mode.
    pub(crate) downloads: crate::downloads::DownloadsState,
    /// Events for other programs (src/events.rs).
    pub(crate) events: crate::events::State,
    /// Whether the server answers (src/connection.rs).
    pub(crate) connection: crate::connection::ConnectionState,
    /// Search for subtitles and the timing offsets (`subtitles.rs`).
    pub(crate) subs: crate::subtitles::State,
    /// What the server keeps for the user: configuration and display
    /// preferences. Playback follows some of them.
    pub(crate) prefs: crate::settings::Prefs,
    /// The saves of settings on their way; they outlive a session.
    pub(crate) pref_lanes: crate::settings::SaveLanes,
    /// Episode list of the player.
    pub(crate) episode_picker: crate::views::episodes::EpisodePicker,
    /// The version the "update is ready" toast was shown for.
    pub(crate) update_said: Option<String>,
    /// See [`SessionEpoch`].
    pub(crate) session_epoch: SessionEpoch,
    /// The last revision given out by `next_revision`.
    revisions: Revision,
    _subscriptions: Vec<gpui_kit::Subscription>,
    /// The player as the window holds it.
    pub(crate) playback: PlayerUi,
    /// The page on show, the pages Back and Forward return to, and the scroll places.
    pub(crate) nav: Navigation,
    /// The search field and the title index that answers it with no request.
    pub(crate) search: TitleSearch,
    /// The menus of the pages and the one open panel: one popup at a time.
    pub(crate) popups: Popups,
    /// The debug channel (src/debug.rs).
    pub(crate) debug: crate::debug::Channel,
}

impl Bloom {
    pub fn new(config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let toasts = cx.new(ToastState::new);
        let profile_menu = cx.new(|cx| MenuState::new([], cx));
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search movies, shows, episodes…"));
        let admin_users_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find a user"));
        let admin_tasks_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find a task"));
        let admin_logs_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find a log file"));
        // The lists follow their fields as they are typed.
        for field in [&admin_users_filter, &admin_tasks_filter, &admin_logs_filter] {
            cx.subscribe_in(field, window, |_, _, _: &crate::ui::input::InputEvent, _, cx| cx.notify())
                .detach();
        }
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
        let admin_menu = cx.new(|cx| MenuState::new([], cx));
        let subtitle_menu = cx.new(|cx| MenuState::new([], cx));

        let mut subscriptions = Vec::new();
        let player = Player::default();
        let player_frames = player.frames();
        subscriptions.push(
            cx.subscribe_in(&search_input, window, |this, _, event, _, cx| {
                match event {
                    crate::ui::input::InputEvent::PressEnter { .. } => {
                        this.search.debounce = None;
                        let query = this.search.input.read(cx).value().to_string();
                        this.open_search(query, cx);
                    }
                    // The title index answers each keystroke; the server
                    // search follows once the typing rests.
                    crate::ui::input::InputEvent::Change => {
                        let typed = this.search.input.read(cx).value().to_string();
                        this.search_titles(typed, cx);
                        this.search.debounce = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(150))
                                .await;
                            this.update(cx, |this, cx| {
                                let query = this.search.input.read(cx).value().to_string();
                                let shown = match &this.nav.page {
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
            this.follow_fullscreen_menu(window, cx);
            if window.is_fullscreen() {
                return;
            }
            if this.playback.pip.is_some() {
                // The small window has its own saved place.
                let place = crate::pip::frame(window);
                if place.is_some() && this.config.pip_window != place {
                    this.config.pip_window = place;
                    this.save_config_soon(cx);
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
            this.save_config_soon(cx);
        }));
        subscriptions.extend(connect.subscribe(window, cx));
        // Every redraw request passes here. One from the hero tick leaves the
        // page content clean, so the next frame can reuse the cached rows.
        subscriptions.push(cx.observe(&cx.entity(), |this, _, _| {
            if !std::mem::take(&mut this.hero.only_redraw) {
                this.content_dirty = true;
            }
        }));
        subscriptions.push(cx.subscribe(
            &seek_slider,
            |this, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => {
                    log::debug!("seek slider change: {}", value.end());
                    this.playback.scrubbing = true;
                    // The preview over the timeline follows the thumb.
                    cx.notify();
                }
                SliderEvent::Release(value) => {
                    log::debug!("seek slider release: {}", value.end());
                    this.playback.scrubbing = false;
                    this.request_seek_to(value.end() as f64, cx);
                    cx.notify();
                }
            },
        ));

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
                        if seq == this.playback.frame_seq {
                            return None;
                        }
                        this.playback.frame_wake_ns = crate::player::clock_ns();
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
                    #[cfg(target_os = "macos")]
                    gpui_macos::wake_frame_sources();
                }
            }
        })
        .detach();
        // The media keys, and the end of the Now Playing tile at quit.
        subscriptions.push(crate::nowplaying::install(cx));
        // A save of the config that still waits goes at the quit.
        subscriptions.push(cx.on_app_quit(|this: &mut Self, cx| {
            this.flush_config_save(cx);
            async {}
        }));
        let app = cx.weak_entity();
        let rows_view = cx.new(|_| crate::views::home::HomeRows { app });

        let mut this = Self {
            config,
            screen: Screen::Connect,
            session: None,
            toasts,
            player,
            awake: Default::default(),
            nowplaying: Default::default(),
            airplay: Default::default(),
            connect,
            splash: Default::default(),
            views: Vec::new(),
            menus_fullscreen: false,
            app_focus: cx.focus_handle(),
            admin_users_filter,
            admin_tasks_filter,
            admin_logs_filter,
            settings_list: false,
            backdrop_seen: 0,
            viewport_w: 1280.,
            viewport_h: 820.,
            hero: crate::views::hero::Hero::new(),
            config_saves: std::cell::Cell::new(0),
            window_save: None,
            rows_view,
            content_dirty: true,
            queue: Default::default(),
            je_quality_tags: (false, true, true),
            dialogs: crate::admin::dialogs::Dialogs::new(admin_inputs),
            admin_live: crate::admin::Live::default(),
            sync: Default::default(),
            socket: Default::default(),
            cast: Default::default(),
            metadata: crate::metadata::State::new(window, cx),
            lists: crate::lists::State::new(window, cx),
            enhanced: Default::default(),
            stream: Default::default(),
            chromecast: Default::default(),
            downloads: Default::default(),
            events: Default::default(),
            connection: Default::default(),
            subs: Default::default(),
            prefs: Default::default(),
            pref_lanes: Default::default(),
            episode_picker: Default::default(),
            update_said: None,
            session_epoch: SessionEpoch::default(),
            revisions: Revision::default(),
            _subscriptions: subscriptions,
            playback: PlayerUi {
                status: PlayerStatus::default(),
                poll: None,
                open: false,
                was_open: false,
                fullscreen_before: false,
                frame: None,
                frame_seq: 0,
                frame_wake_ns: 0,
                last_draw_ns: 0,
                slider_pos: -1.,
                noticed_frame_seq: 0,
                seek_slider,
                scrubbing: false,
                audio_menu,
                settings_menu,
                subtitle_menu,
                volume_slider,
                volume: 100.,
                // A test instance never makes a sound (`dev/run-test` sets this).
                muted: crate::options::options().muted,
                speed: 1.,
                playing: None,
                segments: Vec::new(),
                tracks_version: 0,
                focus: cx.focus_handle(),
                info: false,
                controls_visible: true,
                window_buttons_hidden: false,
                pointer_hidden: false,
                pause_screen: false,
                pip: None,
                last_activity: std::time::Instant::now(),
            },
            nav: Navigation {
                page: Page::Home(HomeData::default()),
                history: Vec::new(),
                forward: Vec::new(),
                back: false,
                scroll: ScrollHandle::new(),
                generation: 0,
                rows: RefCell::new(HashMap::new()),
                glides: Default::default(),
                hovered_card: None,
            },
            search: TitleSearch {
                input: search_input,
                debounce: None,
                catalog: Default::default(),
                loaded: None,
                request: Revision::default(),
            },
            popups: Popups {
                profile_menu,
                sort_menu,
                show_menu,
                filter_menu,
                detail_audio_menu,
                detail_subtitle_menu,
                more_menu,
                card_menu,
                admin_menu,
                panel: None,
                panel_dismissed: None,
            },
            debug: crate::debug::Channel {
                requests: None,
                listener: None,
                env_listener: None,
                dialogs: (None, None),
            },
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
        this.start_events();
        this.start_lights(cx);
        this.start_hero_tick(window, cx);
        // The red button and cmd-w: playback stops the way the Stop button
        // stops it (the position is reported) before the window goes; the
        // cores close with the window (`main.rs`).
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |this, cx| {
                this.request_stop(cx);
                // A save that still waits (`save_config_soon`, the window
                // place) goes now: its task ends with the window.
                this.flush_config_save(cx);
            })
            .ok();
            true
        });
        // One popup at a time, in the whole app: a menu that opens closes
        // every other menu and panel. See `close_popups`.
        for menu in this.menus().map(Clone::clone) {
            this._subscriptions.push(cx.observe_in(&menu, window, |this, opened, window, cx| {
                if opened.read(cx).is_open() {
                    this.close_popups(Some(&opened), window, cx);
                }
            }));
        }
        // A field that had the focus and left the page (the search field
        // after a result opens, a dialog that closed) would keep it: gpui
        // then sends keys and menu actions to a node that is not there, and
        // the shortcuts are dead until a click. The screen takes the focus
        // back the moment the field goes.
        this._subscriptions.push(cx.on_focus_lost(window, |this, window, cx| {
            if this.screen == Screen::Main {
                let target = if this.playback.open { &this.playback.focus } else { &this.app_focus };
                window.focus(target, cx);
            }
        }));
        this
    }
}

mod navigation;
mod playback;
mod sessions;

impl Render for Bloom {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        let started_cpu = crate::perf::thread_cpu_ns();
        self.hero.only_frame = !std::mem::take(&mut self.content_dirty);
        self.sync_frame(window);
        crate::ui::glass::set_over_video(self.playback.open);
        // iOS: the video is under GPUI's layer, so a glass over it needs a
        // copy of the frame while a popup is open over the player, or while
        // the controls show (Daniel, 2026-10-23: the bar is glass over the
        // picture on the phone, not a dark slab; they hide after a few
        // seconds, so the copies stop then).
        if cfg!(target_os = "ios") {
            let popup = self.panel_open() || self.menus().iter().any(|menu| menu.read(cx).is_open());
            self.player.set_backdrop(self.playback.open && (popup || self.playback.controls_visible));
        }
        // The page moves with the software keyboard: a frame per tick
        // while it slides.
        if crate::views::shell::keyboard_sliding() {
            window.request_animation_frame();
        }
        self.viewport_w = f32::from(window.viewport_size().width);
        self.viewport_h = f32::from(window.viewport_size().height);
        // Picture in picture ends with the player.
        if !self.playback.open {
            self.leave_pip(window);
        }
        self.follow_player_fullscreen(window);
        // The window buttons go away with the controls of the player.
        // Picture in picture hides them by itself.
        let buttons_hidden = self.playback.open && self.playback.pip.is_none() && !self.playback.controls_visible;
        if buttons_hidden != self.playback.window_buttons_hidden {
            self.playback.window_buttons_hidden = buttons_hidden;
            crate::pip::set_window_buttons_hidden(window, buttons_hidden);
        }
        // The pointer goes away with the controls, also in picture in
        // picture. macOS shows it again when the mouse moves, and a move
        // brings the controls back. A key can bring the controls back
        // with no move, so the pointer is given back then.
        let pointer_hidden =
            self.playback.open && !self.playback.controls_visible && window.is_window_active();
        if pointer_hidden != self.playback.pointer_hidden {
            self.playback.pointer_hidden = pointer_hidden;
            crate::macos::hide_pointer_until_it_moves(pointer_hidden);
        }
        // The main screen holds the focus when nothing else does, so its
        // keyboard shortcuts work without a click first.
        if self.screen == Screen::Main
            && !self.playback.open
            && self.connect.authorize.is_none()
            && (window.focused(cx).is_none() || self.playback.focus.is_focused(window))
        {
            window.focus(&self.app_focus, cx);
        }
        if std::mem::take(&mut self.connect.clear_password) {
            self.connect
                .password
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if std::mem::take(&mut self.sync.focus_player) && self.playback.open {
            window.focus(&self.playback.focus, cx);
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
        use gpui_kit::{Focusable as _, InteractiveElement as _, prelude::FluentBuilder as _};
        div()
            .relative()
            .size_full()
            // Menu bar actions for this window.
            .on_action(cx.listener(|_, _: &menus::Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &menus::Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &menus::ToggleFullScreen, window, _| {
                window.toggle_fullscreen()
            }))
            // cmd-[ and cmd-] are also outdent and indent in the text
            // editor of the dashboard: there the keys stay with the field
            // (gpui tries the field's binding next when this one passes).
            .on_action(cx.listener(|this, _: &menus::Back, _, cx| {
                if this.text_editor_open() {
                    cx.propagate();
                } else if !this.playback.open {
                    this.back(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::Forward, _, cx| {
                if this.text_editor_open() {
                    cx.propagate();
                } else if !this.playback.open {
                    this.forward(cx)
                }
            }))
            // Settings… is greyed out while no session is open: the entry
            // has no listener then, which is what AppKit asks for.
            .when(self.session.is_some() && self.screen == Screen::Main, |el| {
                el.on_action(cx.listener(|this, _: &menus::Settings, _, cx| {
                    if !this.playback.open {
                        this.open_settings(crate::settings::Section::Profile, cx)
                    }
                }))
            })
            // Close Window does what the red button does: playback stops,
            // the window goes, the app stays. gpui closes the window; AppKit's
            // `performClose:` would not, while the player hides the buttons.
            .on_action(cx.listener(|this, _: &menus::CloseWindow, window, cx| {
                this.request_stop(cx);
                // This way out does not ask `on_window_should_close`: a
                // save of the config that still waits goes here.
                this.flush_config_save(cx);
                window.remove_window();
            }))
            .on_action(cx.listener(|this, _: &menus::About, _, cx| {
                if !this.playback.open && this.screen == Screen::Main {
                    this.open_settings(crate::settings::Section::About, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::CheckForUpdates, _, cx| {
                this.check_for_updates(cx)
            }))
            .on_action(cx.listener(|this, _: &menus::Home, _, cx| {
                if !this.playback.open && this.screen == Screen::Main {
                    this.open_home(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &menus::Random, _, cx| {
                if !this.playback.open && this.screen == Screen::Main {
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
                if !this.playback.open && this.screen == Screen::Main {
                    let query = this.search.input.read(cx).value().to_string();
                    this.open_search(query, cx);
                    let focus = this.search.input.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &menus::PlayPause, _, cx| {
                if this.playback.open {
                    this.request_toggle_pause(cx);
                } else {
                    this.toggle_hero_pause();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &menus::PictureInPicture, window, cx| {
                if this.playback.open {
                    this.toggle_pip(window, cx)
                }
            }))
            // On iOS the player page is a hole over the video layer of
            // the window: nothing under it may paint.
            .when(!(cfg!(target_os = "ios") && self.playback.open), |el| el.bg(theme.colors.background))
            .text_color(theme.colors.foreground)
            .font_family(theme.fonts.body.clone())
            .text_size(px(14.))
            .line_height(px(20.))
            .child(content)
            .child(self.popups.card_menu.clone())
            .child(self.popups.admin_menu.clone())
            .children(self.render_metadata(window, cx))
            .children(self.render_lists_dialog(cx))
            .children(self.render_confirm(cx))
            .children(self.render_authorize(cx))
            // The open panel, under the top bar; the player places its own.
            .children(
                (!self.playback.open)
                    .then(|| {
                        let top = Some(crate::views::shell::TOPBAR_H + 4.);
                        self.render_sync_panel(top, cx)
                            .or_else(|| self.render_cast_panel(top, cx))
                            .or_else(|| self.render_subs_panel(top, cx))
                    })
                    .flatten(),
            )
            .children(self.render_enhanced_help(cx))
            .children(self.render_seasons_dialog(cx))
            // A test instance says so in its window, so nobody takes it for
            // the app in use (`dev/run-test` sets the name).
            .children(crate::options::options().test_name.clone().map(|name| {
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
            // The splash at the start lies over every page, the player too.
            .children(self.render_splash(window, cx))
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
#[cfg(test)]
mod shell_tests;
