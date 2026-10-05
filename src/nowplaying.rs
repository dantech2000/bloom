// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Now Playing tile of macOS and the media keys, through
//! MediaPlayer.framework. `MPNowPlayingInfoCenter` shows what the player
//! plays (Control Centre, the Touch Bar, AirPods); `MPRemoteCommandCenter`
//! brings the keys of the keyboard and the buttons of the tile to the app.
//!
//! The info is set at a change of state, of item, of speed and at a seek,
//! never at every position: the system counts the time on from the rate.
//!
//! The system calls a command handler on a thread of its own choice. The
//! handler puts the command in a channel, and a task on the UI thread takes
//! it to the player, the way the AirPlay events go (`watch_airplay` in
//! `src/cast/mod.rs`). Every command goes through the playback facade
//! (`src/playback.rs`), so a SyncPlay group and a cast target keep their
//! rules.

use std::{
    ffi::{CStr, c_void},
    sync::OnceLock,
    time::Instant,
};

use block::{ConcreteBlock, RcBlock};
use gpui_kit::{Context, Subscription};

use crate::{
    airplay::av::{Pool, nsstring, string_of},
    app::Bloom,
    cast::target::Kind,
    jellyfin::Item,
    macos::{Id, Sel, class, objc_msgSend, sel, send},
    player::PlayState,
};

// The keys of the info dictionary are constants of the framework; the link
// line makes them and the classes resolve.
#[link(name = "MediaPlayer", kind = "framework")]
unsafe extern "C" {
    static MPMediaItemPropertyTitle: Id;
    static MPMediaItemPropertyArtist: Id;
    static MPMediaItemPropertyAlbumTitle: Id;
    static MPMediaItemPropertyPlaybackDuration: Id;
    static MPMediaItemPropertyArtwork: Id;
    static MPNowPlayingInfoPropertyElapsedPlaybackTime: Id;
    static MPNowPlayingInfoPropertyPlaybackRate: Id;
    static MPNowPlayingInfoPropertyDefaultPlaybackRate: Id;
    static MPNowPlayingInfoPropertyMediaType: Id;
}

/// `MPNowPlayingInfoMediaTypeVideo`.
const MEDIA_TYPE_VIDEO: usize = 2;
/// `MPNowPlayingPlaybackState`.
const STATE_PLAYING: isize = 1;
const STATE_PAUSED: isize = 2;
const STATE_STOPPED: isize = 3;
/// `MPRemoteCommandHandlerStatusSuccess`.
const HANDLED: isize = 0;
/// A jump of the position beyond what the rate explains: a seek.
const SEEK_JUMP_SECS: f64 = 2.;
/// Width of the artwork asked from the server.
const ARTWORK_WIDTH: u32 = 600;

/// `CGSize`, for the artwork handler.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

/// A command of the system, as the handlers and the debug command give it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    /// Seconds forward (negative: back).
    Skip(f64),
    /// Seconds from the start.
    SeekTo(f64),
}

impl Command {
    /// Parses the argument of the debug command `nowplaying command`.
    pub fn parse(text: &str) -> Option<Self> {
        let (word, arg) = text.split_once(' ').unwrap_or((text, ""));
        Some(match word {
            "play" => Self::Play,
            "pause" => Self::Pause,
            "toggle" => Self::Toggle,
            "stop" => Self::Stop,
            "next" => Self::Next,
            "previous" => Self::Previous,
            "seek" => Self::SeekTo(arg.trim().parse().ok()?),
            "skip" => Self::Skip(arg.trim().parse().ok()?),
            _ => return None,
        })
    }
}

/// The commands on their way to the UI thread.
fn channel() -> &'static (async_channel::Sender<Command>, async_channel::Receiver<Command>) {
    static CHANNEL: OnceLock<(async_channel::Sender<Command>, async_channel::Receiver<Command>)> =
        OnceLock::new();
    CHANNEL.get_or_init(|| async_channel::bounded(64))
}

/// What every handler block does with its command, and what the debug
/// command `nowplaying command` does: the command goes to the UI thread.
pub fn on_remote(command: Command) {
    log::info!("now playing: command {command:?}");
    if channel().0.try_send(command).is_err() {
        log::warn!("now playing: command {command:?} dropped, the queue is full");
    }
}

fn center() -> Id {
    send!(Id, class(c"MPNowPlayingInfoCenter"), c"defaultCenter")
}

fn command_center() -> Id {
    send!(Id, class(c"MPRemoteCommandCenter"), c"sharedCommandCenter")
}

/// A command of the centre by the name of its property (`playCommand`).
fn command_named(name: &CStr) -> Id {
    let function: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { function(command_center(), sel(name)) }
}

/// The keys of the info dictionary.
struct Keys {
    title: Id,
    artist: Id,
    album: Id,
    duration: Id,
    artwork: Id,
    elapsed: Id,
    rate: Id,
    default_rate: Id,
    media_type: Id,
}

fn keys() -> Keys {
    unsafe {
        Keys {
            title: MPMediaItemPropertyTitle,
            artist: MPMediaItemPropertyArtist,
            album: MPMediaItemPropertyAlbumTitle,
            duration: MPMediaItemPropertyPlaybackDuration,
            artwork: MPMediaItemPropertyArtwork,
            elapsed: MPNowPlayingInfoPropertyElapsedPlaybackTime,
            rate: MPNowPlayingInfoPropertyPlaybackRate,
            default_rate: MPNowPlayingInfoPropertyDefaultPlaybackRate,
            media_type: MPNowPlayingInfoPropertyMediaType,
        }
    }
}

fn number(value: f64) -> Id {
    send!(Id, class(c"NSNumber"), c"numberWithDouble:", value => f64)
}

/// Adds a handler to one command of the centre. The centre keeps the block
/// for the life of the process; the closure maps its event to a [`Command`].
fn add_handler(name: &CStr, map: impl Fn(Id) -> Command + 'static) {
    let command = command_named(name);
    if command.is_null() {
        return;
    }
    let block = ConcreteBlock::new(move |event: Id| -> isize {
        on_remote(map(event));
        HANDLED
    })
    .copy();
    send!((), command, c"setEnabled:", true => bool);
    send!((), command, c"addTargetWithHandler:", &*block as *const _ as Id => Id);
    // The centre copied the block; this reference may go.
    drop(block);
}

/// Registers the handlers, once. The system shows the buttons of the
/// commands that have one.
fn register_commands() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let _pool = Pool::new();
        add_handler(c"playCommand", |_| Command::Play);
        add_handler(c"pauseCommand", |_| Command::Pause);
        add_handler(c"togglePlayPauseCommand", |_| Command::Toggle);
        add_handler(c"stopCommand", |_| Command::Stop);
        add_handler(c"nextTrackCommand", |_| Command::Next);
        add_handler(c"previousTrackCommand", |_| Command::Previous);
        // `MPSkipIntervalCommandEvent`: the interval the system shows.
        add_handler(c"skipForwardCommand", |event| Command::Skip(send!(f64, event, c"interval")));
        add_handler(c"skipBackwardCommand", |event| Command::Skip(-send!(f64, event, c"interval")));
        // `MPChangePlaybackPositionCommandEvent`: a scrub of the tile.
        add_handler(c"changePlaybackPositionCommand", |event| {
            Command::SeekTo(send!(f64, event, c"positionTime"))
        });
    });
}

/// Tells the skip buttons how far they go: the lengths of the player's own
/// skip buttons.
fn set_skip_intervals(back: f64, forward: f64) {
    for (name, secs) in [(c"skipBackwardCommand", back), (c"skipForwardCommand", forward)] {
        let command = command_named(name);
        if command.is_null() {
            continue;
        }
        let intervals = send!(Id, class(c"NSArray"), c"arrayWithObject:", number(secs) => Id);
        send!((), command, c"setPreferredIntervals:", intervals => Id);
    }
}

/// An Objective-C object this module retains.
struct Retained(Id);

impl Retained {
    fn get(&self) -> Id {
        self.0
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        send!((), self.0, c"release");
    }
}

/// `MPMediaItemArtwork` around an `NSImage`, with the block that hands the
/// image out. The framework copies the block and calls it later, on a queue
/// of its own, maybe after this struct is gone. So the block has a retain of
/// the image for itself, given back when the block is destroyed, and it
/// returns the image retained and autoreleased, a live +0 object.
struct Artwork {
    object: Retained,
    image: Retained,
    _handler: RcBlock<(CGSize,), Id>,
}

impl Artwork {
    /// Asks the artwork for its picture, as the framework does, and names
    /// the class of what comes back. For the debug command.
    fn check(&self) -> String {
        let size = send!(CGSize, self.image.0, c"size");
        let image = send!(Id, self.object.0, c"imageWithSize:", size => CGSize);
        let class = string_of(send!(Id, image, c"className")).unwrap_or_else(|| "nil".into());
        format!("class={class} size={}x{} same={}", size.width, size.height, image == self.image.0)
    }
}

/// The artwork made from the bytes of a picture. None when the bytes are
/// not a picture.
fn make_artwork(bytes: &[u8]) -> Option<Artwork> {
    let data = send!(
        Id, class(c"NSData"), c"dataWithBytes:length:",
        bytes.as_ptr() => *const u8, bytes.len() => usize
    );
    let image = send!(Id, class(c"NSImage"), c"alloc");
    let image = send!(Id, image, c"initWithData:", data => Id);
    if image.is_null() {
        return None;
    }
    let size = send!(CGSize, image, c"size");
    // The system asks for the picture at a size; the one image serves
    // every size.
    let owned = Retained(send!(Id, image, c"retain"));
    let handler = ConcreteBlock::new(move |_wanted: CGSize| -> Id {
        // `owned.get()` and not `owned.0`: the closure must capture all of
        // `owned`, so the retain goes with the block.
        let image = send!(Id, owned.get(), c"retain");
        send!(Id, image, c"autorelease")
    })
    .copy();
    let artwork = send!(Id, class(c"MPMediaItemArtwork"), c"alloc");
    let artwork = send!(
        Id, artwork, c"initWithBoundsSize:requestHandler:",
        size => CGSize, &*handler as *const _ as Id => Id
    );
    if artwork.is_null() {
        send!((), image, c"release");
        return None;
    }
    Some(Artwork {
        object: Retained(artwork),
        image: Retained(image),
        _handler: handler,
    })
}

/// Makes artwork `rounds` times and drops each `Artwork` while the framework
/// object of it is still held, as the system may hold it. The block must keep
/// the image alive on its own, and give it back when the object goes. For the
/// test and the debug command.
fn artwork_lifetime_probe(rounds: usize) -> Result<String, String> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(8, 6, image::Rgba([200, 30, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let count = |image: Id| send!(usize, image, c"retainCount");
    let mut last = String::new();
    for round in 0..rounds {
        let artwork = make_artwork(&png).ok_or("no artwork")?;
        let image = artwork.image.0;
        // The framework's own retain of the artwork object.
        let held = Retained(send!(Id, artwork.object.0, c"retain"));
        let before = count(image);
        drop(artwork);
        let after = count(image);
        // Called after the struct is gone, as the system may do.
        let size = CGSize { width: 4., height: 3. };
        let shown = send!(Id, held.0, c"imageWithSize:", size => CGSize);
        let class = string_of(send!(Id, shown, c"className")).unwrap_or_else(|| "nil".into());
        if after != 1 || shown != image || class != "NSImage" {
            return Err(format!("round {round}: retain {before} -> {after}, same={}, class={class}", shown == image));
        }
        last = format!("retain {before} -> {after} after the struct dropped, class={class}");
    }
    Ok(format!("{rounds} rounds ok; last: {last}"))
}

/// The title, artist and album lines of the tile. An episode shows as its
/// name, under the series, with the season as the album; anything else
/// shows its title alone.
pub fn labels(item: &Item) -> (String, Option<String>, Option<String>) {
    match &item.series_name {
        Some(series) => {
            let season = item
                .season_name
                .clone()
                .or_else(|| item.parent_index_number.map(|n| format!("Season {n}")));
            let album = match (season, item.index_number) {
                (Some(season), Some(n)) => Some(format!("{season} · Episode {n}")),
                (Some(season), None) => Some(season),
                (None, Some(n)) => Some(format!("Episode {n}")),
                (None, None) => None,
            };
            (item.name.clone(), Some(series.clone()), album)
        }
        None => (item.display_title(), None, None),
    }
}

/// What the centre was told last.
#[derive(Clone, Debug, PartialEq)]
pub struct Published {
    pub item_id: String,
    pub paused: bool,
    pub rate: f32,
    pub duration: f64,
    pub elapsed: f64,
    pub at: Instant,
}

impl Published {
    /// Where the system thinks the position is now.
    fn expected(&self) -> f64 {
        self.elapsed + f64::from(self.rate) * self.at.elapsed().as_secs_f64()
    }
}

impl State {
    /// The artwork of the item that was published goes to the back; the one
    /// before it goes.
    fn retire_artwork(&mut self) {
        if let Some((_, artwork)) = self.artwork.take() {
            self.artwork_previous = Some(artwork);
        }
    }
}

#[derive(Default)]
pub struct State {
    pub published: Option<Published>,
    /// The artwork of the published item, and the id of that item.
    artwork: Option<(String, Artwork)>,
    /// The artwork before it. The framework pushes the info on a queue of
    /// its own; the artwork of the item before stays until the next item,
    /// so a late push finds its picture.
    artwork_previous: Option<Artwork>,
    /// The item whose artwork is on its way.
    artwork_loading: Option<String>,
}

/// Sets the info and the state of the centre.
fn publish(item: &Item, published: &Published, artwork: Option<&Artwork>) {
    let _pool = Pool::new();
    let (title, artist, album) = labels(item);
    let info = send!(Id, class(c"NSMutableDictionary"), c"dictionary");
    let put = |key: Id, value: Id| {
        send!((), info, c"setObject:forKey:", value => Id, key => Id);
    };
    let keys = keys();
    put(keys.title, nsstring(&title));
    if let Some(artist) = &artist {
        put(keys.artist, nsstring(artist));
    }
    if let Some(album) = &album {
        put(keys.album, nsstring(album));
    }
    put(keys.duration, number(published.duration));
    put(keys.elapsed, number(published.elapsed));
    put(keys.rate, number(f64::from(published.rate)));
    put(keys.default_rate, number(1.));
    put(
        keys.media_type,
        send!(Id, class(c"NSNumber"), c"numberWithUnsignedInteger:", MEDIA_TYPE_VIDEO => usize),
    );
    if let Some(artwork) = artwork {
        put(keys.artwork, artwork.object.0);
    }
    let center = center();
    send!((), center, c"setNowPlayingInfo:", info => Id);
    let state = if published.paused { STATE_PAUSED } else { STATE_PLAYING };
    send!((), center, c"setPlaybackState:", state => isize);
}

/// Takes the info away: nothing plays.
fn clear() {
    let _pool = Pool::new();
    let center = center();
    send!((), center, c"setNowPlayingInfo:", std::ptr::null_mut::<c_void>() => Id);
    send!((), center, c"setPlaybackState:", STATE_STOPPED => isize);
}

/// Reads the centre back, for the debug command: what the system was told.
pub fn describe() -> String {
    let _pool = Pool::new();
    let center = center();
    let state = match send!(isize, center, c"playbackState") {
        STATE_PLAYING => "playing",
        STATE_PAUSED => "paused",
        STATE_STOPPED => "stopped",
        4 => "interrupted",
        _ => "unknown",
    };
    let info = send!(Id, center, c"nowPlayingInfo");
    if info.is_null() {
        return format!("playbackState={state} info=none");
    }
    let text = |key: Id| string_of(send!(Id, info, c"objectForKey:", key => Id));
    let float = |key: Id| {
        let value = send!(Id, info, c"objectForKey:", key => Id);
        (!value.is_null()).then(|| send!(f64, value, c"doubleValue"))
    };
    let keys = keys();
    let media = send!(Id, info, c"objectForKey:", keys.media_type => Id);
    let media = (!media.is_null()).then(|| send!(usize, media, c"unsignedIntegerValue"));
    let artwork = send!(Id, info, c"objectForKey:", keys.artwork => Id);
    format!(
        "playbackState={state} title={:?} artist={:?} album={:?} duration={:?} elapsed={:?} rate={:?} mediaType={:?} artwork={}",
        text(keys.title),
        text(keys.artist),
        text(keys.album),
        float(keys.duration),
        float(keys.elapsed),
        float(keys.rate),
        media,
        !artwork.is_null(),
    )
}

/// Which commands of the centre have a handler and are on.
fn describe_commands() -> String {
    let _pool = Pool::new();
    [
        c"playCommand",
        c"pauseCommand",
        c"togglePlayPauseCommand",
        c"stopCommand",
        c"nextTrackCommand",
        c"previousTrackCommand",
        c"skipForwardCommand",
        c"skipBackwardCommand",
        c"changePlaybackPositionCommand",
    ]
    .iter()
    .map(|name| {
        let command = command_named(name);
        let enabled = !command.is_null() && send!(bool, command, c"isEnabled");
        let name = name.to_str().unwrap_or_default().trim_end_matches("Command");
        let intervals = if name.starts_with("skip") {
            let intervals = send!(Id, command, c"preferredIntervals");
            let first = send!(Id, intervals, c"firstObject");
            if first.is_null() {
                String::new()
            } else {
                format!("({})", send!(f64, first, c"doubleValue"))
            }
        } else {
            String::new()
        };
        format!("{name}{intervals}={}", if enabled { "on" } else { "off" })
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// Starts the task that brings the commands to the player, and clears the
/// tile and the display assertion when the app quits. Called once, from
/// `Bloom::new`.
pub fn install(cx: &mut Context<Bloom>) -> Subscription {
    let receiver = channel().1.clone();
    cx.spawn(async move |this, cx| {
        while let Ok(command) = receiver.recv().await {
            if this.update(cx, |this, cx| this.nowplaying_command(command, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    cx.on_app_quit(|this, _| {
        this.awake.release();
        if this.nowplaying.published.take().is_some() {
            clear();
        }
        async {}
    })
}

impl Bloom {
    /// Tells the centre about the player after a change of its status.
    /// Nothing is sent while nothing changed that the system cannot count
    /// on from the rate.
    pub fn nowplaying_update(&mut self, cx: &mut Context<Self>) {
        // Scalars only: this runs at every change of the status.
        let status = &self.player_status;
        let (state, position, buffering) = (status.state, status.position, status.buffering);
        let active = matches!(state, PlayState::Starting | PlayState::Playing)
            && status.error.is_none()
            && self.cast.kind() == Kind::Local;
        let Some(item) = self.playing.as_ref().filter(|_| active) else {
            if self.nowplaying.published.take().is_some() {
                clear();
                self.nowplaying.retire_artwork();
            }
            self.nowplaying.artwork_loading = None;
            return;
        };
        let paused = status.paused || state == PlayState::Starting;
        // The user's speed; a SyncPlay correction is not a speed to show.
        let rate = if paused || buffering { 0. } else { self.speed };
        let duration = if status.duration > 0. {
            status.duration
        } else {
            item.run_time_ticks.unwrap_or(0) as f64 / crate::jellyfin::TICKS_PER_SECOND as f64
        };
        let before = self.nowplaying.published.as_ref();
        let same_item = before.is_some_and(|p| p.item_id == item.id);
        let seek = before.is_some_and(|p| (position - p.expected()).abs() > SEEK_JUMP_SECS);
        let same = before.is_some_and(|p| {
            same_item && p.paused == paused && p.rate == rate && p.duration == duration
        });
        if same && !seek {
            return;
        }
        let published = Published {
            item_id: item.id.clone(),
            paused,
            rate,
            duration,
            elapsed: position,
            at: Instant::now(),
        };
        if !same_item {
            register_commands();
            set_skip_intervals(self.prefs.skip_back_secs(), self.prefs.skip_forward_secs());
            let item = item.clone();
            publish(&item, &published, None);
            self.nowplaying.retire_artwork();
            self.nowplaying.published = Some(published);
            self.load_artwork(item, cx);
            return;
        }
        let artwork = self.nowplaying.artwork.as_ref().map(|(_, a)| a);
        publish(item, &published, artwork);
        self.nowplaying.published = Some(published);
    }

    /// Fetches the poster of the item off the UI thread and adds it to the
    /// info when it arrives, if the item still plays.
    fn load_artwork(&mut self, item: Item, cx: &mut Context<Self>) {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        let Some(url) = item
            .poster_url(&client, ARTWORK_WIDTH)
            .or_else(|| item.wide_url(&client, ARTWORK_WIDTH))
        else {
            return;
        };
        if self.nowplaying.artwork_loading.as_deref() == Some(&item.id) {
            return;
        }
        self.nowplaying.artwork_loading = Some(item.id.clone());
        let agent = crate::images::agent(cx);
        let fetch = cx
            .background_executor()
            .spawn(async move { crate::images::fetch_bytes(&agent, &url) });
        cx.spawn(async move |this, cx| {
            let bytes = fetch.await;
            this.update(cx, |this, _| {
                if this.nowplaying.artwork_loading.as_deref() != Some(&item.id) {
                    return;
                }
                this.nowplaying.artwork_loading = None;
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        log::warn!("now playing: no artwork: {err:#}");
                        return;
                    }
                };
                let Some(artwork) = make_artwork(&bytes) else {
                    log::warn!("now playing: the artwork is not a picture");
                    return;
                };
                let still = this.nowplaying.published.as_ref().is_some_and(|p| p.item_id == item.id)
                    && this.playing.as_ref().is_some_and(|i| i.id == item.id);
                if !still {
                    return;
                }
                // The same info again, with the picture and the position now.
                let mut published = this.nowplaying.published.clone().unwrap();
                published.elapsed = this.player_status.position;
                published.at = Instant::now();
                publish(&item, &published, Some(&artwork));
                this.nowplaying.published = Some(published);
                this.nowplaying.artwork = Some((item.id.clone(), artwork));
            })
            .ok();
        })
        .detach();
    }

    /// A command of the system, on the UI thread. With a cast target the
    /// target takes it; otherwise the playback facade does, so a SyncPlay
    /// group hears of it.
    pub fn nowplaying_command(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.cast.kind() != Kind::Local {
            let view = self.target_view();
            match command {
                Command::Play => self.target_set_paused(false, cx),
                Command::Pause => self.target_set_paused(true, cx),
                Command::Toggle => self.target_toggle_pause(cx),
                Command::Stop => self.target_stop(cx),
                Command::Next => self.target_skip(true, cx),
                Command::Previous => self.target_skip(false, cx),
                Command::Skip(secs) => self.target_seek(view.position + secs, cx),
                Command::SeekTo(secs) => self.target_seek(secs, cx),
            }
            return;
        }
        if self.playing.is_none() {
            log::info!("now playing: {command:?} with nothing in the player");
            return;
        }
        match command {
            Command::Play => {
                if self.player_status.paused {
                    self.request_toggle_pause(cx);
                }
            }
            Command::Pause => {
                if !self.player_status.paused {
                    self.request_toggle_pause(cx);
                }
            }
            Command::Toggle => self.request_toggle_pause(cx),
            Command::Stop => self.request_stop(cx),
            Command::Next => {
                self.request_next(cx);
            }
            Command::Previous => self.request_previous(cx),
            Command::Skip(secs) => self.request_seek_by(secs, cx),
            Command::SeekTo(secs) => self.request_seek_to(secs, cx),
        }
        self.show_controls();
        cx.notify();
    }

    /// Both macOS integrations, after a change of the player status: one
    /// call from the poll of the player.
    pub fn macos_player_changed(&mut self, cx: &mut Context<Self>) {
        self.awake_update(cx);
        self.nowplaying_update(cx);
    }

    /// The debug command `nowplaying`.
    pub fn debug_nowplaying(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" | "state" => {
                let ours = match &self.nowplaying.published {
                    Some(p) => format!(
                        "item={} paused={} rate={} duration={:.1} elapsed={:.1} expected={:.1} artwork={}",
                        p.item_id,
                        p.paused,
                        p.rate,
                        p.duration,
                        p.elapsed,
                        p.expected(),
                        self.nowplaying.artwork.is_some()
                    ),
                    None => "none".into(),
                };
                format!(
                    "system: {}\npublished: {ours}\nplayer: state={:?} paused={} pos={:.1}\ncommands: {}",
                    describe(),
                    self.player_status.state,
                    self.player_status.paused,
                    self.player_status.position,
                    describe_commands()
                )
            }
            // Sends a command the way a handler block does.
            "command" => match Command::parse(arg.trim()) {
                Some(command) => {
                    on_remote(command);
                    format!("sent {command:?}")
                }
                None => "error: nowplaying command play|pause|toggle|stop|next|previous|seek <s>|skip <±s>".into(),
            },
            // Runs the update now, without a change of the player.
            "update" => {
                self.nowplaying_update(cx);
                describe()
            }
            // Asks the artwork for its picture, the way the framework does.
            "artwork" => match &self.nowplaying.artwork {
                Some((id, artwork)) => format!("item={id} {}", artwork.check()),
                None => "none".into(),
            },
            // Makes and drops artwork in a loop with the framework object held.
            "artwork-probe" => {
                let rounds = arg.trim().parse().unwrap_or(50);
                artwork_lifetime_probe(rounds).unwrap_or_else(|e| format!("error: {e}"))
            }
            _ => "error: nowplaying state|command <...>|update|artwork|artwork-probe [n]".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> Item {
        serde_json::from_value(serde_json::json!({ "Id": "x", "Name": name, "Type": "Movie" })).unwrap()
    }

    #[test]
    fn artwork_outlives_its_struct() {
        println!("{}", artwork_lifetime_probe(20).unwrap());
    }

    #[test]
    fn an_episode_splits_into_its_lines() {
        let mut episode = item("The One");
        episode.kind = "Episode".into();
        episode.series_name = Some("Friends".into());
        episode.season_name = Some("Season 1".into());
        episode.parent_index_number = Some(1);
        episode.index_number = Some(2);
        assert_eq!(
            labels(&episode),
            ("The One".into(), Some("Friends".into()), Some("Season 1 · Episode 2".into()))
        );
        episode.season_name = None;
        assert_eq!(labels(&episode).2, Some("Season 1 · Episode 2".into()));
    }

    #[test]
    fn a_movie_is_its_title() {
        let mut movie = item("Heat");
        movie.production_year = Some(1995);
        assert_eq!(labels(&movie), ("Heat (1995)".into(), None, None));
    }

    #[test]
    fn commands_parse() {
        assert_eq!(Command::parse("play"), Some(Command::Play));
        assert_eq!(Command::parse("seek 42.5"), Some(Command::SeekTo(42.5)));
        assert_eq!(Command::parse("skip -10"), Some(Command::Skip(-10.)));
        assert_eq!(Command::parse("skip"), None);
        assert_eq!(Command::parse("dance"), None);
    }

    #[test]
    fn the_expected_position_follows_the_rate() {
        let published = Published {
            item_id: "x".into(),
            paused: true,
            rate: 0.,
            duration: 100.,
            elapsed: 10.,
            at: Instant::now() - std::time::Duration::from_secs(5),
        };
        assert_eq!(published.expected(), 10.);
        let playing = Published {
            paused: false,
            rate: 2.,
            ..published
        };
        assert!((playing.expected() - 20.).abs() < 0.5);
    }
}
