// SPDX-License-Identifier: AGPL-3.0-or-later
//! Development control channel. When `BLOOM_DEBUG_SOCK` names a path, the
//! app listens there on a Unix socket and runs one text command per
//! connection, so a script can drive navigation and read state without input
//! events. See `dev/jctl`.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use gpui_kit::{App, Context, SharedString, Window, px, size};

use crate::app::{Bloom, Page, Panel};

pub struct Request {
    line: String,
    reply: mpsc::Sender<String>,
}

/// A listener of the debug channel on one socket. Its drop ends the thread
/// and takes the socket away, so a listener goes with the window that
/// made it: a window that opens again starts its own on the same path.
pub struct Listener {
    path: String,
    stop: Arc<AtomicBool>,
}

impl Listener {
    /// Listens on `path`; each line that comes in goes to `requests`, and
    /// the answer goes back. The socket is for the owner only.
    fn start(path: String, requests: async_channel::Sender<Request>) -> Option<Self> {
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(err) => {
                log::warn!("debug channel: cannot bind {path}: {err}");
                return None;
            }
        };
        // For the owner only, or not at all: the channel drives the app.
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(err) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
                log::warn!("debug channel: cannot make {path} private ({err}); not listening");
                let _ = std::fs::remove_file(&path);
                return None;
            }
        }
        log::info!("debug channel listening on {path}");
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        thread::Builder::new()
            .name("debug".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    // An error that stays (no file handles left) would turn
                    // this loop into a spin.
                    let Ok(stream) = stream else {
                        thread::sleep(Duration::from_millis(200));
                        continue;
                    };
                    // A caller that connects and sends no line must not
                    // hold the one thread of the channel for good.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut line = String::new();
                    if BufReader::new(&stream).read_line(&mut line).is_err() {
                        continue;
                    }
                    let (reply, answer) = mpsc::channel();
                    let request = Request {
                        line: line.trim().to_string(),
                        reply,
                    };
                    // This thread is not async; the UI task awaits the receiver.
                    if requests.send_blocking(request).is_err() {
                        return;
                    }
                    let text = answer
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap_or_else(|_| "error: timed out".into());
                    let mut stream = stream;
                    let _ = writeln!(stream, "{text}");
                }
            })
            .expect("spawn debug thread");
        Some(Self { path, stop })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The thread waits in `accept`; a connection wakes it.
        let _ = std::os::unix::net::UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
        log::info!("debug channel on {} closed", self.path);
    }
}

/// The channel as the window holds it.
pub struct Channel {
    /// Where the listeners send the commands, and the listener that the
    /// setting turned on.
    pub(crate) requests: Option<async_channel::Sender<Request>>,
    pub(crate) listener: Option<Listener>,
    /// The listener on the socket of `BLOOM_DEBUG_SOCK`, kept so it ends
    /// with the window.
    pub(crate) env_listener: Option<Listener>,
    /// The question and the form a debug command opened, by the address
    /// of their action; 0 for none. `write confirm` and `write submit`
    /// run these only, never a dialog a person opened.
    pub(crate) dialogs: (
        Option<std::rc::Weak<dyn Fn(&mut Bloom, &mut Context<Bloom>)>>,
        Option<std::rc::Weak<dyn Fn(&mut Bloom, Vec<String>, &mut Context<Bloom>)>>,
    ),
}

/// The socket of the debug channel that the setting turns on, in the folder
/// of the app.
pub fn setting_socket() -> String {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(crate::brand::FOLDER)
        // A test instance must not take the socket of the app the user runs.
        .join(match &crate::options::options().test_name {
            Some(name) => format!("debug-{name}.sock"),
            None => "debug.sock".into(),
        })
        .display()
        .to_string()
}

/// The verbs of the channel, in the order of `run_debug_command`, for the
/// answer to an unknown one. A new verb goes here too.
const COMMANDS: &[&str] = &[
    "home", "back", "search", "library", "item", "resize", "scroll", "hero", "hover-card", "play",
    "stop", "track", "tracks", "mute", "playall", "next", "libraries", "previous", "enqueue",
    "hover", "queue", "pip", "window-front", "window-back", "pause", "seek", "admin", "meta", "admin-edit", "admin-config",
    "admin-discard", "write", "admin-save", "admin-dialog", "connect", "authorize",
    "authorize-close", "je", "tags", "show", "scrub", "type", "a11y", "menu", "settings",
    "plugin-config", "branding", "airplay", "access", "triggers", "awake", "updates", "nowplaying",
    "syncplay", "lists", "events", "socket-inject", "cast", "downloads", "connection",
    "chromecast", "subs", "quality", "episodes", "rows", "perf", "theme", "glass", "mpv",
    "mpv-set", "stream", "quit", "pacing", "hero-video", "sizes", "prefs", "catalog", "menubar",
    "action", "appearance", "reopen", "state",
];

impl Bloom {
    /// Starts the loop that runs the commands on the UI thread, and the
    /// listeners: one on the socket of `BLOOM_DEBUG_SOCK` (a development
    /// or test instance), one when the setting is on.
    pub fn start_debug_channel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::unbounded::<Request>();
        if let Some(path) = &crate::options::options().debug_sock {
            // Lives as long as the window.
            self.debug.env_listener = Listener::start(path.clone(), tx.clone());
        }
        self.debug.requests = Some(tx);
        if self.config.debug_channel == Some(true) {
            self.listen_for_debug();
        }

        cx.spawn_in(window, async move |this, cx| {
            // Sleeps until a listener thread sends; no timer.
            while let Ok(request) = rx.recv().await {
                let answer = this
                    .update_in(cx, |this, window, cx| {
                        this.run_debug_command_noting_dialogs(&request.line, window, cx)
                    })
                    .unwrap_or_else(|_| "error: app closed".into());
                let _ = request.reply.send(answer);
            }
        })
        .detach();
    }

    fn listen_for_debug(&mut self) {
        if self.debug.listener.is_none()
            && let Some(requests) = self.debug.requests.clone()
        {
            self.debug.listener = Listener::start(setting_socket(), requests);
        }
    }

    /// Whether the setting has the debug channel on.
    pub fn debug_channel_on(&self) -> bool {
        self.debug.listener.is_some()
    }

    /// The setting "Debug channel": on or off at once, and kept for the
    /// next start.
    pub fn set_debug_channel(&mut self, on: bool, cx: &mut Context<Self>) {
        if on {
            self.listen_for_debug();
        } else {
            // The drop ends the thread.
            self.debug.listener = None;
        }
        self.config.debug_channel = Some(self.debug.listener.is_some());
        self.save_config(cx);
        cx.notify();
    }

    /// The address of the action of the open question and of the open
    /// form; 0 for none.
    fn open_dialogs(&self) -> (usize, usize) {
        (
            self.dialogs.confirm.as_ref().map_or(0, |c| std::rc::Rc::as_ptr(&c.run) as *const () as usize),
            self.dialogs.prompt.as_ref().map_or(0, |p| std::rc::Rc::as_ptr(&p.run) as *const () as usize),
        )
    }

    /// True when the open question is the one a debug command opened.
    pub(crate) fn debug_opened_confirm(&self) -> bool {
        match (&self.dialogs.confirm, &self.debug.dialogs.0) {
            (Some(open), Some(noted)) => noted.upgrade().is_some_and(|run| std::rc::Rc::ptr_eq(&run, &open.run)),
            _ => false,
        }
    }

    /// The same for the open form.
    fn debug_opened_prompt(&self) -> bool {
        match (&self.dialogs.prompt, &self.debug.dialogs.1) {
            (Some(open), Some(noted)) => noted.upgrade().is_some_and(|run| std::rc::Rc::ptr_eq(&run, &open.run)),
            _ => false,
        }
    }

    /// Runs a command, and notes the question or the form it opened.
    fn run_debug_command_noting_dialogs(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let before = self.open_dialogs();
        let answer = self.run_debug_command(line, window, cx);
        let after = self.open_dialogs();
        // A weak hold on the action: it names the dialog for as long as
        // any dialog can be compared with it, and its place in memory is
        // not given to another dialog meanwhile.
        if after.0 != before.0 {
            self.debug.dialogs.0 = self.dialogs.confirm.as_ref().map(|c| std::rc::Rc::downgrade(&c.run));
        }
        if after.1 != before.1 {
            self.debug.dialogs.1 = self.dialogs.prompt.as_ref().map(|p| std::rc::Rc::downgrade(&p.run));
        }
        answer
    }

    fn run_debug_command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        match command {
            "home" => self.open_home(cx),
            "back" => self.back(cx),
            // As the search button of the bar: the page, and the field
            // focused (on a phone that raises the keyboard).
            "search" => {
                self.open_search(rest.to_string(), cx);
                let focus = gpui_kit::Focusable::focus_handle(self.search.input.read(cx), cx);
                window.focus(&focus, cx);
            }
            "library" => {
                let view = self
                    .views
                    .iter()
                    .find(|v| v.name.eq_ignore_ascii_case(rest))
                    .cloned();
                match view {
                    Some(view) => self.open_library(view, cx),
                    None => return format!("error: no library named {rest:?}"),
                }
            }
            "item" => {
                let id = rest.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    |this, result, cx| match result {
                        Ok(item) => this.open_item(item, cx),
                        Err(err) => log::warn!("debug item: {err:#}"),
                    },
                );
            }
            "resize" => {
                let mut parts = rest.split_whitespace().map(str::parse::<f32>);
                match (parts.next(), parts.next()) {
                    (Some(Ok(w)), Some(Ok(h))) => window.resize(size(px(w), px(h))),
                    _ => return "error: usage: resize <width> <height>".into(),
                }
            }
            "scroll" => match rest.parse::<f32>() {
                Ok(y) => self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(-y))),
                Err(_) => return "error: usage: scroll <y>".into(),
            },
            "hero" => self.step_hero(rest != "previous"),
            // Shows the hover layer of a card without the pointer, as
            // `poster.<item id>`; no argument clears it.
            "hover-card" => cx.set_global(crate::views::cards::HoveredCard(
                (!rest.is_empty()).then(|| SharedString::from(rest.to_string())),
            )),
            // Player test hooks: play the item of the open detail page, muted.
            "play" => {
                self.playback.muted = true;
                self.play_detail(true, window, cx);
            }
            "stop" => {
                self.request_stop(cx);
                // `play` muted the player for the test; give the sound back.
                // A test instance (`BLOOM_MUTED`) never makes a sound.
                if !crate::options::options().muted {
                    self.playback.muted = false;
                    self.player.set_muted(false);
                }
            }
            // A track by its server index, as the menus pick one: `track sub
            // <index|off>`, `track audio <index>`. `tracks` prints them.
            "track" => {
                let usage = "error: usage: track sub <index|off> | track audio <index>";
                let (kind, which) = rest.split_once(' ').unwrap_or((rest, ""));
                let index = match which {
                    "off" => -1,
                    n => match n.parse::<i64>() {
                        Ok(n) => n,
                        Err(_) => return usage.into(),
                    },
                };
                let resolved = crate::stream::current();
                let transcode = resolved
                    .as_ref()
                    .is_some_and(|r| r.play_method == crate::stream::PlayMethod::Transcode);
                match kind {
                    "sub" => self.stream_subtitle(index),
                    "audio" if transcode => self.stream_reload(Some(index), None),
                    // Direct play: mpv's own tracks, in the file's order.
                    "audio" => {
                        let aid = resolved.as_ref().and_then(|r| {
                            r.embedded("Audio").iter().position(|s| s.index == index).map(|p| p as i64 + 1)
                        });
                        self.player.set_audio(aid);
                    }
                    _ => return usage.into(),
                }
                return self.debug_tracks();
            }
            "tracks" => return self.debug_tracks(),
            // Mutes this instance, for a test member that only follows a group.
            "mute" => {
                self.playback.muted = true;
                self.player.set_muted(true);
            }
            // Plays the open library, muted: `playall`, `playall shuffle`.
            "playall" => {
                self.playback.muted = true;
                self.play_library(rest == "shuffle", window, cx);
            }
            "next" => {
                if !self.request_next(cx) {
                    return "error: the queue is empty".into();
                }
            }
            // Names of the libraries, one on each line.
            "libraries" => {
                return self
                    .views
                    .iter()
                    .map(|view| view.name.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            "previous" => self.request_previous(cx),
            // Puts an item at the end of the play queue: `enqueue <item id>`.
            "enqueue" => {
                let id = rest.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    |this, result, cx| match result {
                        Ok(item) => {
                            this.queue.upcoming.push(item);
                            cx.notify();
                        }
                        Err(err) => log::warn!("debug enqueue: {err:#}"),
                    },
                );
            }
            // Puts the pointer over the timeline: `hover 0.4`, `hover off`.
            "hover" => {
                self.queue.hover = rest.parse::<f32>().ok().map(|f| f.clamp(0., 1.));
                self.show_controls();
            }
            "queue" => {
                let names = |items: &[crate::jellyfin::Item]| {
                    items
                        .iter()
                        .map(|i| i.display_title())
                        .collect::<Vec<_>>()
                        .join(" | ")
                };
                return format!(
                    "playing={:?} muted={} history=[{}] upcoming=[{}] chapters={} trickplay={} up_next={:?}",
                    self.playback.playing.as_ref().map(|i| i.display_title()),
                    self.playback.muted,
                    names(&self.queue.history),
                    names(&self.queue.upcoming),
                    self.queue.timeline.chapters.len(),
                    self.queue.timeline.trickplay.is_some(),
                    self.up_next().map(|(item, secs)| (item.display_title(), secs)),
                );
            }
            "pip" => self.toggle_pip(window, cx),
            // For a screenshot of a test instance that sits under other
            // windows: brings its window over them without taking the
            // focus, and puts it back.
            "window-front" | "window-back" => {
                use crate::macos::{Id, class, send};
                let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
                let windows = send!(Id, app, c"windows");
                let count = send!(usize, windows, c"count");
                for i in 0..count {
                    let window = send!(Id, windows, c"objectAtIndex:", i => usize);
                    if command == "window-front" {
                        send!((), window, c"orderFront:", std::ptr::null_mut() => Id);
                    } else {
                        send!((), window, c"orderBack:", std::ptr::null_mut() => Id);
                    }
                }
                return command.to_string();
            }
            "pause" => self.request_toggle_pause(cx),
            "seek" => match rest.parse::<f64>() {
                Ok(secs) => self.request_seek_to(secs, cx),
                Err(_) => return "error: usage: seek <seconds>".into(),
            },
            // `admin user <name>` opens the page of that user on the Users
            // section (the list must have loaded).
            "admin" if rest.starts_with("user ") => {
                let name = rest.trim_start_matches("user ").trim();
                self.open_admin(crate::admin::Section::Users, cx);
                if let Page::Admin(data) = &mut self.nav.page {
                    data.list = false;
                }
                if !crate::admin::users::open_user_named(self, name, cx) {
                    return "error: no such user on the Users page (loaded?)".into();
                }
            }
            "admin" => match crate::admin::Section::from_name(if rest.is_empty() {
                "dashboard"
            } else {
                rest
            }) {
                Some(section) => {
                    self.open_admin(section, cx);
                    // The section itself, also on a phone (not its list).
                    if let Page::Admin(data) = &mut self.nav.page {
                        data.list = false;
                    }
                }
                None => return format!("error: no dashboard section named {rest:?}"),
            },
            // Opens the metadata manager for the item of the detail page, to
            // look at it. `browse <type>` reads the provider images (a GET).
            "meta" => return self.debug_meta(rest, window, cx),
            // A configuration page of the dashboard. These change the edited
            // copy only; nothing is sent.
            "admin-edit" => {
                let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
                return self.config_edit(key, value, cx);
            }
            "admin-config" => return self.config_state(),
            "admin-discard" => self.config_discard(cx),
            // Runs a write to the server the way a click does, so a test can
            // check it and then put the data back. The destructive ones take
            // only names a test made ("jellyui-test...").
            "write" => return self.debug_write(rest, window, cx),
            // Presses Save on the open configuration page. A page that asks
            // before it saves still asks.
            "admin-save" => self.config_save(cx),
            // Opens a dialog of the dashboard, to look at it. Nothing is sent.
            "admin-dialog" => match rest {
                "newkey" => crate::admin::api_keys::ask_new(self, cx),
                "newuser" => crate::admin::users::ask_new(self, cx),
                "secret" => {
                    self.dialogs.secret = Some(crate::admin::Secret {
                        title: "API key for Example".to_string(),
                        message: "Copy the key now and keep it in a safe place.".to_string(),
                        value: "0123456789abcdef0123456789abcdef".to_string(),
                    });
                    cx.notify();
                }
                "close" => self.close_admin_dialog(window, cx),
                other => match other.strip_prefix("edit ") {
                    Some(key) if self.config_ask_key(key, cx) => {}
                    _ => {
                        return "error: admin-dialog newkey | newuser | secret | edit <key> | close"
                            .to_string();
                    }
                },
            },
            // The sign-in page. `connect add <address>` asks that server who
            // it is; `connect quick` gets a Quick Connect code.
            "connect" => return self.debug_connect_command(rest, window, cx),
            // Opens the dialog that signs in another device; sends nothing.
            "authorize" => self.open_authorize(window, cx),
            "authorize-close" => self.close_authorize(window, cx),
            // Jellyfin Enhanced: state, the list of keys, a switch, an action.
            "je" => return self.debug_je_command(rest, window, cx),
            "tags" => self.toggle_quality_tags(cx),
            "show" => {
                use crate::app::LibraryShow;
                let show = match rest {
                    "favorites" => LibraryShow::Favorites,
                    "collections" => LibraryShow::Collections,
                    "episodes" => LibraryShow::Episodes,
                    _ => LibraryShow::All,
                };
                self.update_library(|d| d.show = show, cx);
            }
            // Holds the seek slider at a time, as a drag of its thumb does.
            "scrub" => match rest.parse::<f32>() {
                Ok(secs) => {
                    self.playback.scrubbing = true;
                    self.playback.seek_slider
                        .update(cx, |slider, cx| slider.set_value(secs, window, cx));
                    self.show_controls();
                }
                Err(_) => self.playback.scrubbing = false,
            },
            // What a keystroke in the search field does: the title index
            // answers, with no request.
            "type" => self.search_titles(rest.to_string(), cx),
            // The accessibility tree of the last frame as JSON: the roles,
            // labels and bounds of the elements, for a test that taps by
            // label (`dev/ios-ui tap-label`). It needs the tree built: on
            // iOS `BLOOM_A11Y_TREE=1` turns that on (dev/ios does).
            "a11y" => {
                #[cfg(target_os = "ios")]
                {
                    return gpui_ios::a11y_json();
                }
                #[cfg(not(target_os = "ios"))]
                {
                    return window
                        .debug_a11y_tree_json()
                        .unwrap_or_else(|| "error: the accessibility tree is not built".into());
                }
            }
            // Opens a menu of the player, as a click on its button does.
            "menu" => {
                if rest == "close" {
                    self.close_popups(None, window, cx);
                    cx.notify();
                    return self.debug_state(window);
                }
                let menu = match rest {
                    "profile" => self.popups.profile_menu.clone(),
                    "sort" => self.popups.sort_menu.clone(),
                    "filter" => self.popups.filter_menu.clone(),
                    "show" => self.popups.show_menu.clone(),
                    "more" => self.popups.more_menu.clone(),
                    "audio" => self.popups.detail_audio_menu.clone(),
                    "subtitles" => self.playback.subtitle_menu.clone(),
                    "detail-subtitles" => self.popups.detail_subtitle_menu.clone(),
                    _ => {
                        // The entries of the player's menus follow its
                        // tracks; a menu opened before a rebuild was empty.
                        self.rebuild_track_menus(cx);
                        self.playback.settings_menu.clone()
                    }
                };
                if menu.read(cx).is_empty() {
                    return "error: the menu has no entries".into();
                }
                menu.update(cx, |menu, cx| menu.open(window, cx));
            }
            // Opens a page of the settings; "password" opens that form.
            "settings" => match rest {
                "password" => self.ask_new_password(cx),
                _ => match crate::settings::Section::from_name(if rest.is_empty() {
                    "profile"
                } else {
                    rest
                }) {
                    Some(section) => {
                        self.open_settings(section, cx);
                        // The section itself, also on a phone (not its list).
                        self.settings_list = false;
                    }
                    None => return format!("error: no settings section named {rest:?}"),
                },
            },
            // The access editor of a user and the trigger editor of a task.
            "plugin-config" => return self.debug_plugin_config(rest, window, cx),
            "branding" => return self.debug_branding(rest, window, cx),
            // AirPlay: routes, the picker, and the sender (src/airplay/hook.rs).
            "airplay" => return self.debug_airplay(rest, window, cx),
            "access" => return self.debug_access(rest, window, cx),
            "triggers" => return self.debug_triggers(rest, cx),
            // The display-sleep assertion, and the Now Playing tile with the
            // media keys (src/awake.rs, src/nowplaying.rs).
            "awake" => return self.debug_awake(rest, cx),
            // The updater: its state, and a check of the feed now.
            "updates" => return self.debug_updates(rest, cx),
            "nowplaying" => return self.debug_nowplaying(rest, cx),
            // SyncPlay: the state, the list of groups, and the actions of
            // the panel.
            "syncplay" => return self.debug_syncplay(rest, window, cx),
            // Playlists and collections; see `lists.rs`.
            "lists" => return self.debug_lists(rest, window, cx),
            // Counters of the news of the server, and a way to feed a
            // message into the same handler as a real one.
            "events" => return self.news_describe(),
            "socket-inject" => {
                let (kind, json) = rest.split_once(' ').unwrap_or((rest, "{}"));
                if kind.is_empty() {
                    return "error: usage: socket-inject <MessageType> <json>".into();
                }
                let data = match serde_json::from_str(json) {
                    Ok(data) => data,
                    Err(err) => return format!("error: not json: {err}"),
                };
                let kind = kind.to_string();
                self.on_socket_event(crate::realtime::SocketEvent::Message { kind, data }, cx);
                return self.news_describe();
            }
            // Remote control ("Play On"): see `cast::debug_cast`.
            "cast" => return self.debug_cast(rest, window, cx),
            // Downloads for offline use: the list, the queue, the page, the
            // offline mode and a local play.
            "downloads" => return self.debug_downloads(rest, window, cx),
            // Whether the server answers: the state, and a forced offline
            // or a probe (src/connection.rs).
            "connection" => return self.debug_connection(rest, cx),
            // Google Cast: the devices, a connection, and the player on it.
            "chromecast" => return self.debug_chromecast(rest, window, cx),
            // Subtitle timing and the search for subtitles (`subtitles.rs`).
            "subs" => return self.debug_subs(rest, window, cx),
            // Playback quality: the play method, the bitrate limit, the menu.
            "quality" => return self.debug_quality(rest, window, cx),
            // Opens or closes the episode list of the player.
            "episodes" => self.toggle_episode_picker(window, cx),
            "rows" => return self.debug_rows(),
            "perf" => return crate::perf::report(),
            // The light theme, which has no switch in the app any more;
            // a test instance does not save it (`BLOOM_CONFIG_READONLY`).
            "theme" => {
                let dark = self.config.dark.unwrap_or(true);
                match rest {
                    "light" if dark => self.toggle_theme(cx),
                    "dark" if !dark => self.toggle_theme(cx),
                    "toggle" => self.toggle_theme(cx),
                    "light" | "dark" | "" => {}
                    _ => return "error: theme [light|dark|toggle]".into(),
                }
                return format!(
                    "theme={} appearance={}",
                    if self.config.dark.unwrap_or(true) { "dark" } else { "light" },
                    crate::macos::app_appearance()
                );
            }
            // Liquid glass prototype: `glass [frosted|liquid] [key=value ...]`
            // changes the mode and the parameters of the current theme's
            // set live (`ui::glass::apply`); `ground=#rrggbb` dims the
            // light theme's page.
            "glass" => {
                let mode = crate::ui::theme::UiTheme::read(cx).mode;
                let before = crate::ui::glass::settings();
                let settings = match crate::ui::glass::apply(rest, mode) {
                    Ok(settings) => settings,
                    Err(error) => return error,
                };
                if settings.ground != before.ground {
                    crate::ui::theme::UiTheme::set(cx, crate::app::theme(self.config.dark.unwrap_or(true)));
                }
                cx.notify();
                // Without arguments: the values and, after them, the keys
                // and their ranges.
                return crate::ui::glass::describe(&settings, mode, rest.is_empty());
            }
            // An mpv property as the worker sees it (`mpv mute`).
            "mpv" => {
                // The header mpv sends to the server is the access token,
                // and an address can carry it as `ApiKey`.
                if rest.to_ascii_lowercase().contains("header") {
                    return "error: this property holds the access token".into();
                }
                return self
                    .player
                    .probe(rest)
                    .map(|value| crate::airplay::stream::redact(&value))
                    .unwrap_or_else(|| "error: no worker, or no answer".into());
            }
            // `mpv-set <property> <value>`: an mpv option for a test, such
            // as `deinterlace yes` or `hwdec no`; the next read shows it.
            "mpv-set" => {
                let Some((name, value)) = rest.split_once(' ') else {
                    return "error: mpv-set <property> <value>".into();
                };
                // An option that prints a text can print the header mpv
                // sends, and with it the access token (`${http-header-fields}`
                // in a message or a title, a log or a dump to a file); so can
                // a value that names a property. A test needs none of these.
                const PRINTS: [&str; 12] = [
                    "msg", "title", "header", "term", "log", "dump", "record", "script", "include", "config",
                    "status", "input",
                ];
                let lower = name.to_ascii_lowercase();
                if PRINTS.iter().any(|part| lower.contains(part)) || value.contains("${") {
                    return "error: this option can show the access token".into();
                }
                self.player.set_property(name, value);
                return format!("set {name}");
            }
            // The events for other programs (`events.rs`): `stream on`,
            // `stream off`, `stream titles on|off`, and the state.
            "stream" => return self.debug_stream(rest, cx),
            // Quits as the menu does, so the quit path can be tested.
            "quit" => {
                cx.quit();
                return "quit".into();
            }
            // How even the video frames reached the screen since the last
            // call; `pacing clock`, `pacing file <path>` (`pacing.rs`).
            "pacing" => return self.debug_pacing(rest, window, cx),
            // Trailer backdrops on or off, as the menu entry does.
            "hero-video" => {
                let want = rest == "on";
                if self.config.hero_video.unwrap_or(true) != want {
                    self.toggle_hero_video(cx);
                }
            }
            // Sizes of the root and of the pages, and what the history holds.
            "sizes" => {
                use std::mem::size_of;
                return format!(
                    "Bloom={} Page={} HomeData={} LibraryData={} DetailData={} SearchData={} AdminData={} PlaylistData={} Item={} | history={} retained_items={}",
                    size_of::<Bloom>(),
                    size_of::<Page>(),
                    size_of::<crate::app::HomeData>(),
                    size_of::<crate::app::LibraryData>(),
                    size_of::<crate::app::DetailData>(),
                    size_of::<crate::app::SearchData>(),
                    size_of::<crate::admin::AdminData>(),
                    size_of::<crate::lists::PlaylistData>(),
                    size_of::<crate::jellyfin::Item>(),
                    self.nav.history.len(),
                    self.nav.history.iter().map(|(page, _)| page_items(page)).sum::<usize>(),
                );
            }
            // One value of the user's settings, for the save race tests.
            "prefs" => {
                return format!(
                    "configuration[{rest}]={} custom[{rest}]={:?} loaded={}",
                    self.prefs.configuration.get(rest).cloned().unwrap_or(serde_json::Value::Null),
                    self.prefs.custom(rest),
                    self.prefs.loaded
                );
            }
            // The title index: its size and the ids of its first titles.
            "catalog" => {
                return format!(
                    "catalog={} first={:?}",
                    self.search.catalog.items().len(),
                    self.search.catalog.items().iter().take(3).map(|i| i.id.as_str()).collect::<Vec<_>>()
                );
            }
            // The menu bar as AppKit holds it, and whether this window
            // would run each entry (what greys it out when the menu opens).
            // A test instance is never the active app, so its menus are not
            // on screen to look at.
            "menubar" => return self.debug_menubar(window, cx),
            // Runs what a menu entry does: `action Settings`, `action
            // input::SelectAll`. It is dispatched after this command, the
            // way a click on the entry would be.
            "action" => {
                let action = ["", "bloom::", "input::"]
                    .iter()
                    .find_map(|prefix| cx.build_action(&format!("{prefix}{rest}"), None).ok());
                let Some(action) = action else {
                    return format!("error: no action named {rest:?}");
                };
                let name = action.name().to_string();
                window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                return format!("dispatched {name}");
            }
            // The appearance of the app's windows and panels, as a test
            // sets it: `appearance light|dark|system`; see `BLOOM_APPEARANCE`.
            "appearance" => {
                if !rest.is_empty() {
                    crate::macos::set_app_appearance(match rest {
                        "light" => Some(false),
                        "dark" => Some(true),
                        _ => None,
                    });
                }
                return format!("appearance={}", crate::macos::app_appearance());
            }
            // Opens the window again after `reopen <seconds>` when it is
            // closed by then, the way a click on the Dock icon does: the
            // channel goes with the window, so the timer is set before.
            "reopen" => {
                let secs = rest.parse::<f32>().ok().filter(|s| s.is_finite()).unwrap_or(2.).clamp(0., 600.);
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(Duration::from_secs_f32(secs)).await;
                    cx.update(crate::reopen_window);
                })
                .detach();
                return format!("reopen in {secs}s");
            }
            "state" => {}
            // Each verb says its own arguments when they are wrong; see
            // `dev/jctl` and the comment at each arm.
            _ => return format!("error: commands: {}", COMMANDS.join(", ")),
        }
        cx.notify();
        self.debug_state(window)
    }

    /// The verb `meta`: the metadata manager of the item of the detail page.
    fn debug_meta(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        use crate::metadata::Tab;
        let (word, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        if word == "close" {
            self.close_metadata(window, cx);
        } else if word == "state" {
            return self.metadata_describe();
        } else if word == "search" {
            // The Identify form starts with the name, year and ids
            // of the item; this sends that search. A search only
            // reads.
            self.metadata_search(cx);
        } else if word == "browse" {
            self.browse_images(if arg.is_empty() { "Primary" } else { arg }, cx);
        } else if word == "confirm" {
            // Shows the question before an image is deleted; no answer is given.
            let first = self
                .metadata
                .open
                .as_ref()
                .and_then(|m| m.images.as_ref()?.first().cloned());
            if let Some(image) = first {
                self.ask_delete_image(&image, cx);
            }
        } else if word == "scroll" {
            let y = arg.parse::<f32>().unwrap_or(0.);
            self.metadata.scroll.set_offset(gpui_kit::point(px(0.), px(-y)));
        } else if let Page::Detail(data) = &self.nav.page {
            let tab = match word {
                "images" => Tab::Images,
                "identify" => Tab::Identify,
                "refresh" => Tab::Refresh,
                _ => Tab::Details,
            };
            let (id, name, kind) =
                (data.item.id.clone(), data.item.name.clone(), data.item.kind.clone());
            self.open_metadata(&id, &name, kind.name(), tab, cx);
        } else {
            return "error: open an item first".into();
        }
        cx.notify();
        self.debug_state(window)
    }

    /// The verb `write`: a write to the server the way a click does it.
    fn debug_write(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            // "Yes" on the open question of the dashboard.
            // Only a question a debug command opened: one a person
            // opened (restart, shut down, delete a user) is not
            // for this channel to answer.
            "confirm" if self.dialogs.confirm.is_some() && !self.debug_opened_confirm() => {
                return "error: the open question was not opened by a debug command".into();
            }
            "submit" if self.dialogs.prompt.is_some() && !self.debug_opened_prompt() => {
                return "error: the open form was not opened by a debug command".into();
            }
            "confirm" => match self.dialogs.confirm.take() {
                Some(confirm) => (confirm.run)(self, cx),
                None => return "error: no question is open".into(),
            },
            // Submits the open form; the values are separated by "|".
            "submit" => match self.dialogs.prompt.clone() {
                Some(prompt) => {
                    self.close_admin_dialog(window, cx);
                    (prompt.run)(self, arg.split('|').map(str::to_string).collect(), cx);
                }
                None => return "error: no form is open".into(),
            },
            "revoke-key" | "delete-user" | "user-password" if !arg.starts_with("jellyui-test") => {
                return "error: only names that start with jellyui-test".into();
            }
            "revoke-key" => {
                if !crate::admin::api_keys::ask_revoke_named(self, arg, cx) {
                    return "error: no such key on the API Keys page".into();
                }
            }
            "user-password" => {
                if !crate::admin::users::ask_password_named(self, arg, cx) {
                    return "error: no such user on the Users page".into();
                }
            }
            "delete-user" => {
                if !crate::admin::users::ask_delete_named(self, arg, cx) {
                    return "error: no such user on the Users page".into();
                }
            }
            // A yes-or-no field of the user's configuration.
            "pref" => {
                let (key, value) = arg.split_once(' ').unwrap_or((arg, ""));
                self.set_user_config(key, serde_json::Value::Bool(value == "true"), cx);
            }
            // The tagline of the item in the open metadata editor.
            "tagline" => {
                let index = crate::metadata::FIELDS
                    .iter()
                    .position(|field| field.label == "Tagline");
                let Some(index) = index.filter(|_| self.metadata.open.is_some()) else {
                    return "error: the metadata editor is not open".into();
                };
                let text = arg.to_string();
                self.metadata.inputs[index]
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                self.save_metadata(cx);
            }
            "picture" => self.upload_profile_picture(arg.into(), cx),
            "picture-remove" => self.remove_profile_picture(cx),
            _ => {
                return "error: write confirm|submit <a|b>|revoke-key <name>|delete-user <name>|\
                        pref <Key> <true|false>|tagline <text>|picture <path>|picture-remove"
                    .into();
            }
        }
        cx.notify();
        self.debug_state(window)
    }

    /// The verb `connect`: the sign-in page.
    fn debug_connect_command(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (what, value) = rest.split_once(' ').unwrap_or((rest, ""));
        match what {
            "" => self.show_connect(cx),
            "servers" => {
                self.show_connect(cx);
                self.connect.choosing_server = true;
            }
            "add" => {
                self.connect
                    .url
                    .update(cx, |input, cx| input.set_value(value.trim(), window, cx));
                self.add_server(cx);
            }
            // Signs a test account in: `connect signin <name>|<password>`.
            "signin" => {
                let (name, password) = value.split_once('|').unwrap_or((value, ""));
                if !name.starts_with("jellyui-test") {
                    return "error: only names that start with jellyui-test".into();
                }
                self.connect
                    .username
                    .update(cx, |input, cx| input.set_value(name, window, cx));
                self.connect
                    .password
                    .update(cx, |input, cx| input.set_value(password, window, cx));
                self.sign_in(cx);
            }
            "quick" => self.start_quick_connect(cx),
            "cancel" => self.cancel_quick_connect(cx),
            "done" => self.screen = crate::app::Screen::Main,
            _ => return "error: connect [servers | add <address> | quick | cancel | done]".into(),
        }
        cx.notify();
        return self.debug_connect();
    }

    /// The verb `je`: Jellyfin Enhanced.
    fn debug_je_command(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, argument) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "help" => {
                if !self.close_panel(Panel::Help) {
                    self.open_panel(Panel::Help, window, cx);
                }
            }
            "toggle" => match crate::enhanced::Feature::of(argument) {
                Some(feature) => self.toggle_enhanced(feature, cx),
                None => return format!("error: no switch named {argument:?}"),
            },
            "do" => {
                if !self.enhanced_player_action(argument, cx) {
                    return format!("error: {argument:?} did nothing");
                }
            }
            other => {
                if let Some(text) = self.debug_je(other, argument, window, cx) {
                    return text;
                }
            }
        }
        cx.notify();
        return self.enhanced_debug();
    }

    /// The verb `syncplay`: the state, the groups, and the actions of the panel.
    fn debug_syncplay(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        use crate::syncplay::core::Intent;
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" | "state" => return self.sync_describe(),
            "panel" => self.toggle_sync_panel(window, cx),
            "list" => {
                return self
                    .sync
                    .groups
                    .iter()
                    .map(|g| format!("{} {:?} [{}]", g.group_id, g.group_name, g.participants.join(",")))
                    .collect::<Vec<_>>()
                    .join(" | ");
            }
            "new" => {
                let name = if arg.is_empty() { "jellyui test group" } else { arg };
                self.sync_user(Intent::Create { name: name.to_string() }, cx)
            }
            "join" => self.sync_user(Intent::Join { group_id: arg.to_string() }, cx),
            "leave" => self.sync_user(Intent::Leave, cx),
            // The queue of the group; an entry is named by its place, from 1.
            "queue" => {
                let Some(queue) = self.sync.session.as_ref().and_then(|s| s.core.queue()) else {
                    return "no queue".into();
                };
                let current = queue.playing_item_index;
                let entries: Vec<String> = queue
                    .playlist
                    .iter()
                    .enumerate()
                    .map(|(n, item)| {
                        let title = self.sync.titles.get(&item.item_id).cloned().unwrap_or_default();
                        let mark = if n as i64 == current { "*" } else { "" };
                        format!("{}{mark} {title}", n + 1)
                    })
                    .collect();
                return format!(
                    "shuffle={} repeat={} | {}",
                    queue.shuffle_mode,
                    queue.repeat_mode,
                    entries.join(" | ")
                );
            }
            "enqueue" | "enqueue-next" => self.sync_user(
                Intent::Enqueue { item_ids: vec![arg.to_string()], next: verb == "enqueue-next" },
                cx,
            ),
            "remove" | "jump" | "up" | "down" => {
                let place = arg.parse::<usize>().unwrap_or(0);
                let id = self
                    .sync
                    .session
                    .as_ref()
                    .and_then(|s| s.core.queue())
                    .and_then(|q| q.playlist.get(place.wrapping_sub(1)))
                    .map(|item| item.playlist_item_id.clone());
                let Some(playlist_item_id) = id else {
                    return "error: no such entry".into();
                };
                let intent = match verb {
                    "remove" => Intent::Remove { playlist_item_ids: vec![playlist_item_id] },
                    "jump" => Intent::Jump { playlist_item_id },
                    "up" => Intent::Move { playlist_item_id, new_index: place.saturating_sub(2) },
                    _ => Intent::Move { playlist_item_id, new_index: place },
                };
                self.sync_user(intent, cx)
            }
            "shuffle" => match crate::syncplay::protocol::ShuffleMode::parse(arg) {
                Some(mode) => self.sync_user(Intent::Shuffle(mode), cx),
                None => return "error: syncplay shuffle <Sorted|Shuffle>".into(),
            },
            "repeat" => match crate::syncplay::protocol::RepeatMode::parse(arg) {
                Some(mode) => self.sync_user(Intent::Repeat(mode), cx),
                None => return "error: syncplay repeat <RepeatNone|RepeatAll|RepeatOne>".into(),
            },
            "unfollow" => self.sync_user(Intent::StopFollowing, cx),
            "follow" => self.sync_user(Intent::Follow, cx),
            _ => {
                return "error: syncplay state|panel|list|new [name]|join <id>|leave|follow|unfollow|\
                        queue|enqueue <item>|enqueue-next <item>|remove <n>|jump <n>|up <n>|down <n>|\
                        shuffle <Sorted|Shuffle>|repeat <RepeatNone|RepeatAll|RepeatOne>"
                    .into();
            }
        }
        cx.notify();
        self.debug_state(window)
    }

    /// The verb `stream`: the events for other programs, and the lights.
    fn debug_stream(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        match rest {
            "on" => self.set_events(true, cx),
            "off" => self.set_events(false, cx),
            "titles on" => self.set_events_titles(true, cx),
            "titles off" => self.set_events_titles(false, cx),
            "colours on" => self.set_events_colours(true, cx),
            "colours off" => self.set_events_colours(false, cx),
            // The newest look at the picture, top band first.
            "colours" => return format!("stream colours {:?}", self.player.colours()),
            // The lights: the helper, a look for lights, the list,
            // a light chosen or told to blink by its place in it.
            "helper on" => self.set_lights(true, cx),
            "helper off" => self.set_lights(false, cx),
            "scan" => self.lights_scan(cx),
            "lamps" => {}
            // The events of a short film, with no film.
            "test" => return format!("stream test: {}", self.events_test_run(cx)),
            _ => {}
        }
        let row = |text: &str| text.parse::<usize>().ok().and_then(|at| self.lights_rows().into_iter().nth(at));
        if let Some((lamp, _)) = rest.strip_prefix("choose ").and_then(row) {
            self.lights_choose(&lamp, cx);
        } else if let Some((lamp, _)) = rest.strip_prefix("identify ").and_then(row) {
            self.lights_identify(&lamp, cx);
        }
        if ["helper", "scan", "lamps", "choose", "identify"].iter().any(|verb| rest.starts_with(verb)) {
            let rows: Vec<String> = self.lights_rows().iter().map(|(lamp, chosen)| format!("{}{lamp}", if *chosen { "*" } else { " " })).collect();
            return format!("stream lights helper={} lamps=[{}]", self.lights_on(), rows.join(" "));
        }
        // `stream lights film=8 gap=0.5 colour=artwork`: the lights
        // lab, as its sliders set it.
        if let Some(pairs) = rest.strip_prefix("lights") {
            let wrong: Vec<&str> = pairs
                .split_whitespace()
                .filter(|pair| !pair.split_once('=').is_some_and(|(name, value)| self.lights_set(name, value, cx)))
                .collect();
            return format!(
                "stream lights {} colour_seen={:?} file={}{}",
                self.lights_json(cx),
                self.events.colour,
                crate::events::lights_path(),
                if wrong.is_empty() { String::new() } else { format!(" not set: {}", wrong.join(" ")) }
            );
        }
        let hub = crate::events::hub();
        return format!(
            "stream on={} titles={} readers={} closed_slow={} socket={}",
            hub.on(),
            self.config.events_titles == Some(true),
            hub.readers(),
            hub.closed_slow.load(std::sync::atomic::Ordering::Relaxed),
            crate::events::socket_path()
        );
    }

    /// The menu bar with, for each entry that runs an action, whether this
    /// window would run it now (`available=`): the same question AppKit
    /// asks when the menu opens, which greys the entry out.
    fn debug_menubar(&self, window: &Window, cx: &mut App) -> String {
        use gpui_kit::OwnedMenuItem;
        let menus = cx.get_menus().unwrap_or_default();
        let mut available: Vec<(String, bool)> = Vec::new();
        for item in menus.into_iter().flat_map(|menu| menu.items) {
            if let OwnedMenuItem::Action { name, action, .. } = item {
                // The window, or an app-wide listener (`cx.on_action`); a
                // test instance is never the active window, so the second
                // question is asked of the app alone.
                let yes = window.is_action_available(action.as_ref(), cx)
                    || cx.is_action_available(action.as_ref());
                available.push((name, yes));
            }
        }
        let mut out = format!("appearance={}\n", crate::macos::app_appearance());
        for entry in crate::macos::menu_bar() {
            out.push_str(&"  ".repeat(entry.depth));
            out.push_str(&entry.text);
            if let Some(title) = &entry.title
                && let Some((_, available)) = available.iter().find(|(name, _)| name == title)
            {
                out.push_str(&format!("  available={available}"));
            }
            out.push('\n');
        }
        out
    }

    fn debug_connect(&self) -> String {
        format!(
            "connect screen={} server={:?} choosing={} probing={} signing_in={} quick_enabled={} \
             quick_code={} splash={} public_users={} error={:?}",
            if self.screen == crate::app::Screen::Connect { "connect" } else { "main" },
            self.connect.selected_server,
            self.connect.choosing_server,
            self.connect.probing,
            self.connect.signing_in,
            self.connect.quick_enabled,
            self.connect.quick.is_some(),
            self.connect.branding.splashscreen_enabled,
            self.connect.public_users.len(),
            self.connect.error,
        )
    }

    /// The tracks as mpv lists them, each with its server index, and what
    /// the server decided for the subtitles.
    fn debug_tracks(&self) -> String {
        let resolved = crate::stream::current();
        let rows: Vec<String> = self
            .playback.status
            .tracks
            .iter()
            .map(|t| {
                let index = match (t.kind.as_str(), resolved.as_ref()) {
                    ("sub", Some(r)) => r.subtitle_of(t.id, &self.playback.status),
                    ("audio", Some(r)) => r.audio_of(t.id),
                    _ => None,
                };
                format!(
                    "{}{}:{} index={} codec={} title={:?} lang={:?}{}",
                    if t.selected { "*" } else { " " },
                    t.kind,
                    t.id,
                    index.map_or("-".to_string(), |i| i.to_string()),
                    t.codec.as_deref().unwrap_or("-"),
                    t.title,
                    t.lang,
                    if t.external { " external" } else { "" },
                )
            })
            .collect();
        format!(
            "tracks method={} burned={:?} burn_in={} external_subs={:?} selected_external={:?} sids={:?} pending={:?}\n{}",
            resolved.as_ref().map_or("none", |r| r.play_method.as_str()),
            resolved.as_ref().and_then(|r| r.burned_subtitle),
            resolved.as_ref().is_some_and(|r| r.burn_in),
            resolved.as_ref().map(|r| r.external_subs.iter().map(|s| s.index).collect::<Vec<_>>()).unwrap_or_default(),
            resolved.as_ref().and_then(|r| r.selected_external),
            self.playback.status.external_sids,
            self.playback.status.pending_subs,
            rows.join("\n")
        )
    }

    fn debug_state(&self, window: &Window) -> String {
        if self.screen == crate::app::Screen::Connect {
            return self.debug_connect();
        }
        let ids = |items: &[crate::jellyfin::Item]| {
            items
                .iter()
                .take(4)
                .map(|i| format!("{}={}", i.name, i.id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let page = match &self.nav.page {
            Page::Home(d) => format!(
                "home loading={} resume={} next_up={} latest_rows={} [{}]",
                d.loading,
                d.resume.len(),
                d.next_up.len(),
                d.latest.len(),
                d.latest.first().map(|(_, i)| ids(i)).unwrap_or_default(),
            ),
            Page::Library(d) => format!(
                "library {:?} loading={} items={}/{} [{}]",
                d.title,
                d.loading,
                d.items.len(),
                d.total,
                ids(&d.items)
            ),
            Page::Detail(d) => format!(
                "detail {:?} kind={} id={} loading={} seasons={} episodes={}{}",
                d.item.name,
                d.item.kind,
                d.item.id,
                d.loading,
                d.seasons.len(),
                d.episodes.len(),
                d.seasons
                    .first()
                    .map(|season| format!(" season0={}", season.id))
                    .unwrap_or_default()
            ),
            Page::Settings(section) => format!(
                "settings section={section:?} list={} loading={} error=None",
                self.settings_list,
                !self.prefs.loaded
            ),
            Page::Admin(d) => format!(
                "admin section={:?} list={} loading={} error={:?}",
                d.section, d.list, d.loading, d.error
            ),
            Page::Playlist(d) => format!(
                "playlist {:?} loading={} entries={}",
                d.list.name,
                d.loading,
                d.entries.len()
            ),
            Page::Downloads => format!("downloads offline={}", self.downloads.offline),
            Page::Search(d) => format!(
                "search {:?} loading={} results={} [{}]",
                d.query,
                d.loading,
                d.results.len(),
                ids(&d.results)
            ),
        };
        let viewport = window.viewport_size();
        format!(
            "{page} | history={} forward={} player={:?} pos={:.0} playing={:?} window={}x{} scroll={} source={}",
            self.nav.history.len(),
            self.nav.forward.len(),
            self.playback.status.state,
            self.playback.status.position,
            self.playback.playing.as_ref().map(|item| item.name.as_str()).unwrap_or(""),
            f32::from(viewport.width),
            f32::from(viewport.height),
            -f32::from(self.nav.scroll.offset().y),
            self.downloads_source_label(),
        )
    }
}

/// Items a page holds in memory (for `sizes`: what the history retains).
fn page_items(page: &Page) -> usize {
    match page {
        Page::Home(d) => d.resume.len() + d.next_up.len() + d.latest.iter().map(|(_, i)| i.len()).sum::<usize>(),
        Page::Library(d) => d.items.len(),
        Page::Detail(d) => 1 + d.seasons.len() + d.episodes.len() + d.similar.len(),
        Page::Search(d) => d.results.len(),
        Page::Playlist(d) => d.entries.len(),
        Page::Admin(_) | Page::Settings(_) | Page::Downloads => 0,
    }
}

/// The debug command `pacing`.
mod pacing;

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener ends with its owner: the thread stops and the socket goes,
    /// so the window that opens next can listen on the same path. Before,
    /// each window that closed left its thread in `accept` for good.
    #[test]
    fn a_listener_that_is_dropped_ends_its_thread() {
        let path = std::env::temp_dir().join(format!("bloom-debug-test-{}.sock", std::process::id()));
        let path = path.display().to_string();
        let (tx, rx) = async_channel::unbounded::<Request>();
        // The thread holds the one sender.
        let listener = Listener::start(path.clone(), tx).expect("listen");
        assert!(std::path::Path::new(&path).exists());
        drop(listener);
        assert!(!std::path::Path::new(&path).exists(), "the socket stays after the listener");
        for _ in 0..400 {
            if rx.sender_count() == 0 {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("the thread of a dropped listener still runs");
    }
}
