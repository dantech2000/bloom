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

    // ----- infrastructure ---------------------------------------------------

    pub fn toast(&self, title: impl Into<String>, description: impl Into<String>, cx: &mut App) {
        let title = title.into();
        let description = description.into();
        let id = format!("toast-{}", uuid::Uuid::new_v4());
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(id, title, description, Some(Duration::from_secs(6)), cx);
        });
    }

    /// A notice that replaces the one before it of the same `key`, where
    /// several in a row say one thing (three downloads queued one after the
    /// other): one card with the latest word, not a stack.
    pub fn toast_as(&self, key: &str, title: impl Into<String>, description: impl Into<String>, cx: &mut App) {
        let (title, description) = (title.into(), description.into());
        let id = format!("toast-{key}");
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(id, title, description, Some(Duration::from_secs(6)), cx);
        });
    }

    pub fn save_config(&self, cx: &mut App) {
        self.config_saves.set(self.config_saves.get() + 1);
        if sandboxed() {
            return;
        }
        if let Err(err) = self.config.save() {
            log::error!("saving config failed: {err:#}");
            self.toast("Could not save settings", format!("{err:#}"), cx);
        }
    }

    /// Saves the config once the changes stop, for a value that changes
    /// many times in a row (a slider that is dragged): each save is a write
    /// with a full sync of the disk, some milliseconds on the UI thread.
    /// The window place waits the same way; one write takes both. The quit
    /// of the app writes what still waits.
    pub fn save_config_soon(&mut self, cx: &mut Context<Self>) {
        self.window_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            this.update(cx, |this, cx| {
                this.window_save = None;
                this.save_config(cx);
            })
            .ok();
        }));
    }

    /// Writes a save of the config that still waits, at once: every way
    /// out of the window calls this (the red button, Close Window, the
    /// quit), as the task that waits ends with the window. True when a
    /// save waited.
    pub(crate) fn flush_config_save(&mut self, cx: &mut Context<Self>) -> bool {
        let waited = self.window_save.take().is_some();
        if waited {
            self.save_config(cx);
        }
        waited
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

    /// As [`Bloom::fetch`], for an answer that only the session that asked
    /// may take: it is dropped when another profile or server is open by
    /// the time it comes (a picture, a password, a page to open).
    pub fn fetch_in_session<T, W, D>(&self, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        let epoch = self.session_epoch;
        self.fetch(cx, work, move |this, result, cx| {
            if this.session_epoch == epoch {
                done(this, result, cx);
            }
        });
    }

    /// As [`Bloom::fetch`], for an answer that belongs to the page on show:
    /// it is dropped when the user has gone to another page, or the page
    /// has asked again, by the time it comes.
    pub fn fetch_for_page<T, W, D>(&self, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        let generation = self.nav.generation;
        self.fetch(cx, work, move |this, result, cx| {
            if this.nav.generation == generation {
                done(this, result, cx);
            }
        });
    }

    fn next_generation(&mut self) -> u64 {
        self.nav.generation += 1;
        self.nav.generation
    }

    /// A new revision, larger than every one before it.
    pub(crate) fn next_revision(&mut self) -> Revision {
        self.revisions.0 += 1;
        self.revisions
    }

    /// The session changes: every answer still on its way is for the old one.
    fn next_session_epoch(&mut self) {
        self.session_epoch.0 += 1;
        // The page of the session before is gone too: an answer for it
        // must not land in the page of the same kind that opens next.
        self.next_generation();
        // The posters of the session before are of no use to the next one.
        crate::images::cancel_prewarm();
        // Nor are its hero slides, and a question it left open must not
        // run against the next one.
        self.clear_hero();
        self.forget_admin_dialogs();
        // A live load of the dashboard of the session before has no answer
        // to wait for here.
        self.admin_live.busy = false;
        self.admin_live.load += 1;
        // The devices of the server before, and an editor open on one of
        // its items, are not for the next one.
        self.cast.sessions.clear();
        self.metadata.open = None;
    }

    /// Ends the session in the app (the server and the config are the
    /// caller's business), so that what the old session asked for is
    /// dropped when it answers.
    pub fn drop_session(&mut self, cx: &mut Context<Self>) {
        // What plays on another device for this session stops, while the
        // session can still say so.
        self.cast_off(cx);
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
        let url = crate::options::options().server_url.as_deref().unwrap_or(&server.url);
        let client = Client::new(url, &self.config.device_id)
            .with_server(&server.id)
            .with_session(&profile.token, &profile.user_id)
            .opened();
        let (id, server_name, profile) = (server.id.clone(), server.name.clone(), profile.clone());
        // What plays on another device for the session before stops, while
        // that session can still say so.
        self.cast_off(cx);
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
        self.search.catalog = Default::default();
        self.search.loaded = None;
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
        self.fetch_in_session(
            cx,
            |client| client.user_settings(),
            |this, result, cx| {
                if let (Ok((is_admin, audio_language, sync_access)), Some(session)) =
                    (result, this.session.as_mut())
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
        self.fetch_in_session(
            cx,
            |client| Ok(client.quality_prefs()),
            |this, result, cx| {
                if let Ok(Some((enabled, prefs))) = result {
                    use crate::quality_tags::Category;
                    crate::quality_tags::set_prefs(prefs);
                    this.je_quality_tags =
                        (enabled, prefs.shows(Category::Resolution), prefs.shows(Category::DynamicRange));
                    this.apply_quality_tags(cx);
                }
            },
        );
        self.load_enhanced(cx);
        // The file is written when the profile changed, not at each start:
        // a save is a write with a full sync of the disk on the UI thread.
        let active = Some((server_id.to_string(), user_id.to_string()));
        if self.config.active != active {
            self.config.active = active;
            self.save_config(cx);
        }
        self.screen = Screen::Main;
        self.clear_history();
        self.views.clear();
        self.nav.page = Page::Home(HomeData::default());
        // The home page loads the libraries with its own data.
        self.load_page(cx);
        self.events_page(cx);
        self.rebuild_menu(cx);
        cx.notify();
        true
    }

    /// Ends the session on the server, removes its saved profile and shows
    /// the sign-in page of that server.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.stop_sync();
        self.config.active = None;
        match self.session.as_ref().map(|s| (s.server_id.clone(), s.user_id.clone())) {
            // `forget_profile` drops the session, and with it what it asked
            // for, and saves the config.
            Some((server_id, user_id)) => {
                self.forget_profile(&server_id, &user_id, cx);
                self.select_server(server_id, cx);
            }
            None => self.save_config(cx),
        }
        // The pages of the user who left must not show again.
        self.clear_history();
        self.views.clear();
        self.nav.page = Page::Home(HomeData::default());
        self.events_page(cx);
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
        let active = self.config.active.clone();
        self.config.upsert_profile(server_id, profile.clone());
        // `open_session` saves when the active profile changes; a sign-in
        // to the one that is active already is saved here.
        let opened = self.open_session(server_id, &profile.user_id, cx);
        if !opened || self.config.active == active {
            self.save_config(cx);
        }
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let dark = !self.config.dark.unwrap_or(true);
        self.config.dark = Some(dark);
        self.save_config(cx);
        UiTheme::set(cx, theme(dark));
        // The parts AppKit draws follow the theme (see `main.rs`).
        crate::macos::set_app_appearance(Some(dark));
        self.rebuild_menu(cx);
        cx.notify();
    }

    /// The View menu says "Exit Full Screen" while the window is full
    /// screen. The menu bar is built again only when the state changes.
    fn follow_fullscreen_menu(&mut self, window: &Window, cx: &mut App) {
        let fullscreen = window.is_fullscreen();
        if fullscreen != self.menus_fullscreen {
            self.menus_fullscreen = fullscreen;
            crate::menus::set_menus(cx, fullscreen);
        }
    }

    /// Whether the window is full screen, as the bounds observer last saw
    /// it; for the events, which have no window at hand.
    pub(crate) fn events_fullscreen(&self) -> bool {
        self.menus_fullscreen
    }

    /// Card and padding sizes for the current window width.
    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.viewport_w)
    }

    /// Scroll state of every row, for the debug channel.
    pub fn debug_rows(&self) -> String {
        self.nav.rows
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
        self.nav.rows
            .borrow_mut()
            .entry(id.clone())
            .or_default()
            .clone()
    }

    /// Sets the watched or the favourite state of an item on the server and
    /// hands the state the server answers to `done`. With `for_page` the
    /// answer is for the page on show, and is dropped when the user has
    /// left it by then.
    fn set_user_data(
        &mut self,
        item_id: &str,
        mark: Mark,
        on: bool,
        for_page: bool,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, crate::jellyfin::UserData, &mut Context<Self>) + 'static,
    ) {
        let (item_id, generation) = (item_id.to_string(), self.nav.generation);
        self.fetch(
            cx,
            move |client| match mark {
                Mark::Played => client.set_played(&item_id, on),
                Mark::Favorite => client.set_favorite(&item_id, on),
            },
            move |this, result, cx| {
                if for_page && this.nav.generation != generation {
                    return;
                }
                match result {
                    Ok(user_data) => done(this, user_data, cx),
                    Err(err) => {
                        let title = match mark {
                            Mark::Played => "Could not update watched state",
                            Mark::Favorite => "Could not update favorites",
                        };
                        this.toast(title, format!("{err:#}"), cx)
                    }
                }
                cx.notify();
            },
        );
    }

    /// Marks an item watched or unwatched, then reloads the page data.
    pub fn set_item_played(&mut self, item: &Item, played: bool, cx: &mut Context<Self>) {
        self.set_user_data(&item.id, Mark::Played, played, false, cx, |this, _, cx| this.load_page(cx));
    }

    /// Adds an item to the favourites or removes it, then reloads the page data.
    pub fn set_item_favorite(&mut self, item: &Item, favorite: bool, cx: &mut Context<Self>) {
        self.set_user_data(&item.id, Mark::Favorite, favorite, false, cx, |this, _, cx| this.load_page(cx));
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
        self.forget_admin_dialogs();
        self.leave_page();
        // The rows of a detail page have one name on every detail page:
        // the next one starts at the left, not where the last one was.
        self.nav.rows.borrow_mut().retain(|id, _| !id.starts_with("detail."));
        // Keep the place in the page, so Back returns to it.
        let place = self.nav.scroll.offset();
        self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        let previous = std::mem::replace(&mut self.nav.page, page);
        self.nav.history.push((previous, place));
        self.nav.back = false;
        if self.nav.history.len() > 40 {
            self.nav.history.remove(0);
        }
        self.nav.forward.clear();
        self.load_page(cx);
        self.events_page(cx);
        cx.notify();
    }

    /// What goes with the page that leaves: the card under the pointer is
    /// a card of that page, and the pointer may never leave it (the page
    /// changed under it), so its hover layer would show on the way back.
    fn leave_page(&mut self) {
        self.nav.hovered_card = None;
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if let Some((page, offset)) = self.nav.history.pop() {
            self.nav.back = true;
            let left = self.swap_page(page, offset, cx);
            self.nav.forward.push(left);
        }
    }

    /// Returns to the page Back left, as cmd-] and the forward button do.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if let Some((page, offset)) = self.nav.forward.pop() {
            self.nav.back = false;
            let left = self.swap_page(page, offset, cx);
            self.nav.history.push(left);
        }
    }

    /// Shows `page` at `offset` and gives back the page it replaced with
    /// its place.
    fn swap_page(
        &mut self,
        page: Page,
        offset: gpui_kit::Point<gpui_kit::Pixels>,
        cx: &mut Context<Self>,
    ) -> (Page, gpui_kit::Point<gpui_kit::Pixels>) {
        self.forget_admin_dialogs();
        self.leave_page();
        let left_offset = self.nav.scroll.offset();
        self.nav.scroll.set_offset(offset);
        let left = std::mem::replace(&mut self.nav.page, page);
        if matches!(self.nav.page, Page::Library(_)) {
            self.rebuild_library_menus(cx);
        }
        if matches!(self.nav.page, Page::Admin(_)) {
            self.start_admin_tick(cx);
        }
        // The menus of the detail page are those of the item shown last;
        // a page that comes back with its data is not loaded again.
        if matches!(self.nav.page, Page::Detail(_)) {
            self.rebuild_detail_menus(cx);
        }
        if self.page_loading() {
            // The user left before the answer came, and the answer was
            // thrown away (`generation`); the page asks again.
            self.load_page(cx);
        } else {
            self.next_generation();
        }
        self.events_page(cx);
        cx.notify();
        (left, left_offset)
    }

    /// True while the current page waits for its data.
    fn page_loading(&self) -> bool {
        match &self.nav.page {
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
        self.open_root(Page::Home(HomeData::default()), cx);
    }

    /// Shows a page of the top bar: the page starts at its top, and Back
    /// and Forward have nothing to return to.
    fn open_root(&mut self, page: Page, cx: &mut Context<Self>) {
        self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.forget_admin_dialogs();
        self.clear_history();
        self.nav.page = page;
        self.events_page(cx);
        self.rebuild_library_menus(cx);
        self.load_page(cx);
        cx.notify();
    }

    /// Forgets the pages Back and Forward would show.
    fn clear_history(&mut self) {
        self.nav.history.clear();
        self.nav.forward.clear();
    }

    pub fn open_library(&mut self, view: Item, cx: &mut Context<Self>) {
        self.open_root(Page::Library(LibraryData::new(view)), cx);
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
        self.fetch_in_session(
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

    /// Opens the library item of an id (used by Seerr cards in the library).
    pub fn open_item_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.fetch_in_session(
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

    pub fn open_folder(&mut self, folder: Item, cx: &mut Context<Self>) {
        self.navigate(Page::Library(LibraryData::new(folder)), cx);
        self.rebuild_library_menus(cx);
    }

    pub fn open_item(&mut self, item: Item, cx: &mut Context<Self>) {
        match item.kind {
            ItemKind::Playlist => self.open_playlist(item, cx),
            ItemKind::CollectionFolder | ItemKind::UserView | ItemKind::Folder | ItemKind::BoxSet => {
                self.open_folder(item, cx)
            }
            _ => self.navigate(
                Page::Detail(Box::new(DetailData {
                    item,
                    loading: true,
                    ..Default::default()
                })),
                cx,
            ),
        }
    }

    pub fn open_search(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if let Page::Search(data) = &mut self.nav.page {
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
        let results = self.search.catalog.search(&typed);
        match &mut self.nav.page {
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
                self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
                if let Some((items, seerr)) = answered {
                    data.results = items;
                    data.seerr = seerr;
                    data.loading = false;
                    cx.notify();
                    return;
                }
                data.loading = !typed.is_empty();
                // Without an index the old results stay until the server answers.
                if !self.search.catalog.is_empty() || typed.is_empty() {
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

    /// Every menu of the app. A new menu goes into this list, and nowhere
    /// else: the observers of `Bloom::new` and `close_popups` read it.
    fn menus(&self) -> [&Entity<MenuState>; 12] {
        [
            &self.popups.admin_menu,
            &self.popups.profile_menu,
            &self.playback.audio_menu,
            &self.playback.settings_menu,
            &self.popups.sort_menu,
            &self.popups.show_menu,
            &self.popups.filter_menu,
            &self.popups.detail_audio_menu,
            &self.popups.detail_subtitle_menu,
            &self.popups.more_menu,
            &self.popups.card_menu,
            &self.playback.subtitle_menu,
        ]
    }

    /// Closes every popup but `keep`: the menus and the open panel. The app
    /// shows one popup at a time; whatever opens one calls this first.
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
        if self.close_panels() {
            cx.notify();
        }
    }

    /// True while a panel that is not a menu is open.
    pub fn panel_open(&self) -> bool {
        self.popups.panel.is_some()
    }

    /// True while this panel is the open one.
    pub fn panel_is(&self, panel: Panel) -> bool {
        self.popups.panel == Some(panel)
    }

    /// Opens a panel in the place of whatever popup is open.
    pub fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popups(None, window, cx);
        self.popups.panel = Some(panel);
        cx.notify();
    }

    /// Shows a panel in the place of the open one, for an opener with no
    /// window at hand. The menu whose entry opened it has closed itself.
    pub(crate) fn replace_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.popups.panel = Some(panel);
        cx.notify();
    }

    /// The button of a panel: closes the panel when it is open and opens it
    /// in the place of every other popup when it is not. True when the
    /// panel is open afterwards.
    pub fn toggle_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let was_open = self.panel_is(panel);
        self.close_popups(None, window, cx);
        // A click on the button while the panel is open closed it a moment
        // ago, as a click outside of it; that click must not open it again.
        let dismissed = self
            .popups.panel_dismissed
            .take()
            .is_some_and(|(which, at)| which == panel && at.elapsed() < Duration::from_millis(250));
        if was_open || dismissed {
            cx.notify();
            return false;
        }
        self.open_panel(panel, window, cx);
        true
    }

    /// A click outside of a panel: closes it when it is the open one.
    pub fn dismiss_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.close_panel(panel) {
            self.popups.panel_dismissed = Some((panel, std::time::Instant::now()));
            cx.notify();
        }
    }

    /// Closes the open panel, whichever it is; true when one was open.
    pub fn close_panels(&mut self) -> bool {
        self.popups.panel.take().is_some()
    }

    /// Closes this panel; true when it was the open one.
    pub fn close_panel(&mut self, panel: Panel) -> bool {
        self.panel_is(panel) && self.close_panels()
    }

    /// Loads the title index again now: the server said the library changed.
    pub fn reload_catalog(&mut self, cx: &mut Context<Self>) {
        self.search.loaded = None;
        self.refresh_catalog(cx);
    }

    /// Loads the title index, when there is none or it is older than ten
    /// minutes, and fetches the posters of its titles to the disk.
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        const MAX_AGE: Duration = Duration::from_secs(600);
        if self.search.loaded.is_some_and(|at| at.elapsed() < MAX_AGE) {
            return;
        }
        if self.session.is_none() {
            return;
        }
        self.search.loaded = Some(std::time::Instant::now());
        // The answer is for the session that asked and for the newest
        // request: another profile or server may be open by then with an
        // index of its own, or the library changed and a newer request is
        // on its way. An older answer is dropped, posters and all.
        let epoch = self.session_epoch;
        let request = self.next_revision();
        self.search.request = request;
        let query = crate::pages::catalog_query();
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
                if this.session_epoch != epoch || this.search.request != request {
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
                        // The titles are folded once here, not at each key.
                        this.search.catalog = crate::search_index::Index::new(items);
                    }
                    // The next search tries again.
                    Err(err) => {
                        log::warn!("title index failed: {err:#}");
                        this.search.loaded = None;
                    }
                }
            },
        );
    }

    /// (Re)loads the data for the current page.
    pub fn load_page(&mut self, cx: &mut Context<Self>) {
        self.next_generation();
        // Without the server, the pages show what is on this Mac.
        if self.offline_load_page(cx) {
            return;
        }
        match &self.nav.page {
            Page::Home(_) => self.load_home(cx),
            Page::Library(_) => self.load_library(cx),
            Page::Detail(_) => self.load_detail(cx),
            Page::Admin(_) => self.load_admin(cx),
            Page::Playlist(_) => self.load_playlist(cx),
            // The downloads page reads the index of this Mac; nothing loads.
            Page::Downloads => {}
            // The settings load with the session and stay in the app.
            Page::Settings(_) => {
                if !self.prefs.loaded {
                    self.load_prefs(cx);
                }
            }
            Page::Search(_) => self.load_search(cx),
        }
    }

    /// Loads the rows of the home page and, at sign-in, the libraries. `load_page` calls this.
    fn load_home(&mut self, cx: &mut Context<Self>) {
        let Page::Home(data) = &mut self.nav.page else {
            return;
        };
        data.loading = true;
        let libraries = home_libraries(&self.views);
        let need_hero = self.hero.slides.is_empty();
        // At sign-in the libraries are not known yet. They load in
        // the same pass as the page, so the page does not wait for
        // them first.
        let need_views = self.views.is_empty();
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let loaded = crate::pages::load_home(&client, libraries, need_views, need_hero)?;
                log::info!(
                    "home data loaded in {} ms ({} ms after start)",
                    started.elapsed().as_millis(),
                    crate::perf::since_start_ms()
                );
                Ok(loaded)
            },
            move |this, result, cx| {
                if let Page::Home(data) = &mut this.nav.page {
                    match result {
                        Ok((views, mut fresh)) => {
                            if !views.is_empty() {
                                this.views = views;
                            }
                            let hero = std::mem::take(&mut fresh.hero);
                            *data = fresh;
                            if this.hero.slides.is_empty() && !hero.is_empty() {
                                this.hero.slides = hero;
                                this.show_hero_slide(0);
                            }
                        }
                        Err(err) => {
                            data.loading = false;
                            // No server at all: the offline state takes over.
                            if !this.request_failed(&err, cx) {
                                this.toast("Could not load home", format!("{err:#}"), cx);
                            }
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    /// Loads the first page of the library on show. `load_page` calls this.
    fn load_library(&mut self, cx: &mut Context<Self>) {
        let Page::Library(data) = &mut self.nav.page else {
            return;
        };
        data.loading = true;
        let query = library_query(data);
        self.fetch_for_page(
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
                if let Page::Library(data) = &mut this.nav.page {
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

    /// Loads the item of the detail page and the rows under it. `load_page` calls this.
    fn load_detail(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &mut self.nav.page else {
            return;
        };
        data.loading = true;
        // The card already says what kind of item this is, so the
        // requests for its rows start with the item request.
        let known = Item {
            id: data.item.id.clone(),
            kind: data.item.kind.clone(),
            series_id: data.item.series_id.clone(),
            season_id: data.item.season_id.clone(),
            ..Default::default()
        };
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let fresh = crate::pages::load_detail(&client, &known)?;
                log::info!("detail data loaded in {} ms", started.elapsed().as_millis());
                Ok(fresh)
            },
            move |this, result, cx| {
                if let Page::Detail(data) = &mut this.nav.page {
                    match result {
                        Ok(mut fresh) => {
                            // A reload of the same item keeps the
                            // tracks the user picked on the page.
                            if fresh.item.id == data.item.id {
                                (fresh.audio, fresh.subtitle) = (data.audio, data.subtitle);
                            }
                            **data = fresh;
                            // The row of the season starts at the
                            // episode of the page; the ones before
                            // it are a scroll to the left away.
                            if data.item.kind == ItemKind::Episode {
                                let at = data
                                    .episodes
                                    .iter()
                                    .position(|ep| ep.id == data.item.id)
                                    .unwrap_or(0);
                                let offset = at as f32 * this.detail_episode_step();
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

    /// Runs the search of the page on the server. `load_page` calls this.
    fn load_search(&mut self, cx: &mut Context<Self>) {
        let Page::Search(data) = &mut self.nav.page else {
            return;
        };
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
        let text = data.query.clone();
        let asked = text.clone();
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let found = crate::pages::load_search(&client, &text)?;
                log::info!("search data loaded in {} ms", started.elapsed().as_millis());
                Ok(found)
            },
            move |this, result, cx| {
                if let Page::Search(data) = &mut this.nav.page {
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

    // ----- user data --------------------------------------------------------

    /// Flips the watched state of the item shown on the detail page.
    pub fn toggle_played(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.nav.page else {
            return;
        };
        let (item_id, played) = (data.item.id.clone(), !data.item.user_data.played);
        self.set_user_data(&item_id, Mark::Played, played, true, cx, |this, user_data, cx| {
            if let Page::Detail(data) = &mut this.nav.page {
                data.item.user_data = user_data;
                if data.item.is_series() {
                    // Episodes and season counts changed server-side too.
                    this.load_page(cx);
                }
            }
        });
    }

    /// Flips the favorite state of the item shown on the detail page.
    pub fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.nav.page else {
            return;
        };
        let (item_id, favorite) = (data.item.id.clone(), !data.item.user_data.is_favorite);
        self.set_user_data(&item_id, Mark::Favorite, favorite, true, cx, |this, user_data, _| {
            if let Page::Detail(data) = &mut this.nav.page {
                data.item.user_data = user_data;
            }
        });
    }

    // ----- playback ---------------------------------------------------------

    /// Plays the next unwatched episode of a series, or the first episode
    /// when everything has been watched.
    pub fn play_series(&mut self, series: &Item, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        let series_id = series.id.clone();
        let generation = self.nav.generation;
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
                if this.nav.generation != generation {
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
        window.focus(&self.playback.focus, cx);
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
        // A downloaded item plays from its file on this Mac; otherwise the
        // server says how the item plays (the file, or a transcode).
        self.stream_begin(item, start_secs as f64);
        self.player.set_volume(self.playback.volume as f64);
        self.player.set_muted(self.playback.muted);
        self.player.set_speed(self.playback.speed as f64);
        self.apply_subtitle_style();
        self.playback.playing = Some(item.clone());
        self.close_panel(Panel::Episodes);
        self.playback.segments.clear();
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
                    && this.playback.playing.as_ref().is_some_and(|i| i.id == item_id)
                {
                    this.playback.segments = segments;
                    cx.notify();
                }
            },
        );
        self.load_timeline(item.id.clone(), cx);
        self.playback.status = self.player.status();
        self.playback.open = true;
        // A phone turns for the video; an iPad keeps the orientation it has.
        if self.phone() {
            crate::set_landscape(true);
        }
        self.show_controls();
        self.playback.tracks_version = 0;
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// When the last local input happened.
    pub fn last_activity(&self) -> std::time::Instant {
        self.playback.last_activity
    }

    /// Called on pointer activity over the video; controls hide again after a delay.
    pub fn show_controls(&mut self) {
        self.playback.last_activity = std::time::Instant::now();
        self.playback.controls_visible = true;
    }

    /// Controls stay while paused, buffering, scrubbing, or when a track menu is open.
    fn controls_pinned(&self, cx: &App) -> bool {
        self.playback.status.paused
            || self.playback.status.state != PlayState::Playing
            || self.playback.scrubbing
            || self.playback.audio_menu.read(cx).is_open()
            || self.playback.subtitle_menu.read(cx).is_open()
            || self.playback.settings_menu.read(cx).is_open()
            || self.panel_open()
    }

    /// Switches the player between the full window and picture in picture.
    pub fn toggle_pip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.playback.pip.take() {
            Some(normal) => crate::pip::leave(window, normal),
            None => {
                if window.is_fullscreen() {
                    window.toggle_fullscreen();
                }
                self.playback.pip = crate::pip::enter(window, self.config.pip_window);
            }
        }
        self.show_controls();
        cx.notify();
    }

    /// Ends picture in picture when the player goes away.
    pub fn leave_pip(&mut self, window: &mut Window) {
        if let Some(normal) = self.playback.pip.take() {
            crate::pip::leave(window, normal);
        }
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.playback.volume = volume.clamp(0., 100.);
        self.player.set_volume(self.playback.volume as f64);
        if self.playback.muted && self.playback.volume > 0. {
            self.playback.muted = false;
            self.player.set_muted(false);
        }
        cx.notify();
    }

    /// Changes the volume by a step and moves the slider with it.
    pub fn nudge_volume(&mut self, step: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = (self.playback.volume + step).clamp(0., 100.);
        self.playback.volume_slider
            .update(cx, |slider, cx| slider.set_value(volume, window, cx));
        self.set_volume(volume, cx);
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.playback.muted = !self.playback.muted;
        self.player.set_muted(self.playback.muted);
        cx.notify();
    }

    pub fn set_speed(&mut self, speed: f32, cx: &mut Context<Self>) {
        // A group plays at one speed.
        if self.sync.following() {
            self.toast("SyncPlay", "The speed stays at 1x while you play with a group.", cx);
            return;
        }
        self.playback.speed = speed;
        self.player.set_speed(speed as f64);
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// Favourite toggle for the item in the player.
    pub fn toggle_playing_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(item) = &self.playback.playing else {
            return;
        };
        let (item_id, favorite) = (item.id.clone(), !item.user_data.is_favorite);
        self.set_user_data(&item_id.clone(), Mark::Favorite, favorite, false, cx, move |this, user_data, _| {
            if let Some(item) = this.playback.playing.as_mut().filter(|i| i.id == item_id) {
                item.user_data = user_data;
            }
        });
    }

    /// The marked range the playback position is in, if any.
    pub fn current_segment(&self) -> Option<&MediaSegment> {
        let position = self.playback.status.position;
        self.playback.segments.iter().find(|s| {
            // A range under three seconds is not worth a prompt.
            s.end_secs() - s.start_secs() >= 3.
                && position >= s.start_secs()
                && position < s.end_secs() - 1.
        })
    }

    /// Shows or hides the data of the player about the video that plays.
    pub fn toggle_playback_info(&mut self, cx: &mut Context<Self>) {
        self.playback.info = !self.playback.info;
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
        if self.playback.open == self.playback.was_open {
            return;
        }
        self.playback.was_open = self.playback.open;
        if self.playback.open {
            self.playback.fullscreen_before = window.is_fullscreen();
        } else if leaves_fullscreen_with_the_player(self.playback.fullscreen_before, window.is_fullscreen()) {
            window.toggle_fullscreen();
        }
    }

    pub fn close_player_view(&mut self, cx: &mut Context<Self>) {
        self.playback.open = false;
        if self.phone() {
            crate::set_landscape(false);
        }
        // The end of the playback is an event, at once.
        self.events_tick(cx);
        // The preview sheets of the timeline are not needed any more.
        crate::images::preload(Vec::new(), cx);
        self.news_player_closed(cx);
        cx.notify();
    }

    /// The seek slider and the time label follow the position, by whole
    /// quarter seconds, in the draw of a frame (see the frames task): while
    /// frames flow, the poll asks for no draw of its own for the position.
    fn follow_position(&mut self, cx: &mut Context<Self>) {
        let status = &self.playback.status;
        if self.playback.scrubbing || status.duration <= 0. {
            return;
        }
        let pos = (status.position * 4.).floor() / 4.;
        if pos == self.playback.slider_pos {
            return;
        }
        self.playback.slider_pos = pos;
        self.move_seek_slider(cx);
    }

    /// Puts the seek slider at the position of the player.
    fn move_seek_slider(&mut self, cx: &mut Context<Self>) {
        let (pos, dur) = (self.playback.status.position as f32, self.playback.status.duration as f32);
        self.playback.seek_slider.update(cx, |s, cx| {
            crate::ui::slider::set_range_and_value(s, dur, pos);
            cx.notify();
        });
    }

    /// Measures how long a new frame waited for the UI, once for a frame.
    fn note_frame_wait(&mut self) {
        let seq = self.player.frame_seq();
        if seq != self.playback.noticed_frame_seq {
            self.playback.noticed_frame_seq = seq;
            crate::perf::video_frame_noticed(self.player.frame_wait_ms());
        }
    }

    /// Swaps in the newest video frame. With display pacing (`pacing.rs`)
    /// a frame is shown one tick of the display after its own.
    fn sync_frame(&mut self, window: &mut Window) {
        self.player.ui_seen();
        let display = self.player.display_pacing();
        if display && self.playback.open {
            crate::pacing::note_window(window, &self.player);
        }
        let prev_draw = std::mem::replace(&mut self.playback.last_draw_ns, crate::player::clock_ns());
        // A closed player draws no frame: the surface goes back to the pool
        // at once, not when the next item plays.
        if !self.playback.open {
            self.playback.frame = None;
        }
        if self.player.frame_seq() == self.playback.frame_seq {
            return;
        }
        // One read of the frame, its number and its tick: a frame that is
        // published between two reads cannot be shown with the tick of the
        // one before.
        let published = self.player.frame();
        let seq = published.seq;
        if seq == self.playback.frame_seq {
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
        self.playback.frame_seq = seq;
        crate::pacing::trace::adopted(seq, self.playback.frame_wake_ns, self.playback.last_draw_ns, prev_draw, draw_id);
        crate::perf::video_frame_shown();
        self.playback.frame = if self.playback.open { published.frame } else { None };
    }

    pub(crate) fn start_player_poll(&mut self, cx: &mut Context<Self>) {
        self.playback.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let fast = this
                    .read_with(cx, |this, _| this.playback.open)
                    .unwrap_or(false);
                // The frames have their own signal. Everything here moves
                // by quarter seconds or slower, or is a change the eye sees
                // within 50 ms (a pause, the end, a seek's new position; the
                // frame of a seek comes through the frames task).
                let interval = if fast { 50 } else { 500 };
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        let poll_started = crate::player::clock_ns();
                        let status = this.player.status();
                        let ended = status.state == PlayState::Ended;
                        // The seek slider and the time label move by whole
                        // seconds; a redraw for each 8 ms change of the
                        // position cost the whole UI about 48 draws a
                        // second. `playback.status` itself stays current for
                        // everything that reads it.
                        let position_moved = (status.position * 4.).floor()
                            != (this.playback.status.position * 4.).floor();
                        // While frames come, each of them draws the UI and
                        // moves the slider (`follow_position`), so a draw
                        // for the position alone is not needed: it lands at
                        // the tick, misses the compositor and pushes the
                        // next frame a refresh late (see pacing.rs).
                        let frames_flow = this.player.display_pacing()
                            && status.state == PlayState::Playing
                            && !status.paused
                            && this.player.frame_wait_ms() < 100.;
                        let backdrop = crate::ui::glass::video_backdrop_version();
                        let backdrop_moved = backdrop != this.backdrop_seen;
                        this.backdrop_seen = backdrop;
                        let changed = (position_moved && !frames_flow)
                            || backdrop_moved
                            || status.paused != this.playback.status.paused
                            || status.buffering != this.playback.status.buffering
                            || status.state != this.playback.status.state
                            || status.tracks_version != this.playback.status.tracks_version;
                        this.playback.status = status;
                        this.subs_status_changed(cx);
                        for index in this.player.take_failed_subtitles() {
                            this.subtitle_file_failed(index);
                        }
                        if this.playback.status.tracks_version != this.playback.tracks_version {
                            this.playback.tracks_version = this.playback.status.tracks_version;
                            this.rebuild_track_menus(cx);
                        }
                        // The slider follows the position; an update with no
                        // change would only cause a redraw.
                        if changed && !this.playback.scrubbing && this.playback.status.duration > 0. {
                            this.move_seek_slider(cx);
                        }
                        this.enhanced_tick(cx);
                        this.adaptive_tick(cx);
                        this.events_tick(cx);
                        // At the end of a file the queue goes on.
                        // In a group the group decides what comes after an item.
                        let ended = ended
                            && !this.sync.holds_player()
                            && !this.advance_at_end(cx)
                            && !this.refind_lost_episode(cx);
                        if ended {
                            if let Some(err) = this.playback.status.error.clone() {
                                this.toast("Playback stopped", err, cx);
                            }
                            // Nothing may play behind the closed view: an
                            // item that still plays after a load failed,
                            // or a load whose answer is on its way.
                            this.player.stop();
                            this.player.acknowledge_end();
                            this.playback.status = this.player.status();
                            // The one way out of the player view, as Stop
                            // takes it: the preview sheets go as well.
                            this.close_player_view(cx);
                            this.load_page(cx);
                        }
                        if changed || ended {
                            this.macos_player_changed(cx);
                        }
                        let idle = this.playback.last_activity.elapsed() > CONTROLS_HIDE_AFTER;
                        let should_show = !this.playback.open || !idle || this.controls_pinned(cx);
                        let controls_changed = should_show != this.playback.controls_visible;
                        this.playback.controls_visible = should_show;
                        // The pause screen comes up after a rest with no input.
                        let pause_screen = this.playback.open
                            && this.playback.pip.is_none()
                            && this.playback.status.paused
                            && this.playback.status.state == PlayState::Playing
                            && this.playback.last_activity.elapsed() > PAUSE_SCREEN_AFTER;
                        let pause_changed = pause_screen != this.playback.pause_screen;
                        this.playback.pause_screen = pause_screen;
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
