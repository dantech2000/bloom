// SPDX-License-Identifier: AGPL-3.0-or-later
//! How an item gets to the player. The server is asked how to play it
//! (`PlaybackInfo`): it gets what mpv can play and the bitrate the user
//! allows, and answers with the file itself (direct play) or with a
//! transcode over HLS. This module keeps the quality choice, maps the
//! tracks of the server to the tracks of mpv while a transcode plays, ends
//! a transcode that is not needed any more, and shows what plays.

use crate::player::TrackKind;
use crate::jellyfin::{StreamKind, SubtitleDelivery};
use crate::jellyfin::Method;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result, anyhow};
use gpui_kit::{
    Context, Div, ParentElement as _, SharedString, Styled, Window, div, px, rgba,
};

use crate::{
    app::Bloom,
    jellyfin::{Client, Item, MediaStream},
    player::{PlayRequest, PlayState, Player, PlayerStatus, SubtitleFile},
    ui::{glass::glass, menu::MenuItem},
};

pub use bloom_core::bitrate::{LADDER, bitrate_label, ladder};
pub use bloom_core::playback::{
    Load, PlayMethod, PlaybackInfo, Target, mpv_id, stream_title,
};
use bloom_core::playback::{next_track, query_value, request_body, without_key};

// ----- what the app holds ----------------------------------------------------

/// The bitrate the user allows, in bits per second; 0 is no limit ("Auto").
/// A global, because the SyncPlay session loads items without the app.
static MAX_BITRATE: AtomicU64 = AtomicU64::new(0);

pub fn max_bitrate() -> Option<u64> {
    match MAX_BITRATE.load(Ordering::Relaxed) {
        0 => None,
        bitrate => Some(bitrate),
    }
}

/// The last answer of the server, for the player and for the end of the
/// app (a transcode left on the server would run on).
static CURRENT: Mutex<Option<Arc<Resolved>>> = Mutex::new(None);

/// The last answer of the server, whatever its item; none before the
/// first load. While a load is on its way this is still the answer for
/// the item before: ask with [`current_of`] for what belongs to an item.
pub fn current() -> Option<Arc<Resolved>> {
    CURRENT.lock().unwrap().clone()
}

/// The answer of the server for this item; none while its load is on its
/// way, and for a downloaded item, which plays with no answer.
pub fn current_of(item_id: &str) -> Option<Arc<Resolved>> {
    of_item(current(), item_id)
}

fn of_item(held: Option<Arc<Resolved>>, item_id: &str) -> Option<Arc<Resolved>> {
    held.filter(|r| r.item_id == item_id)
}

/// True while this item is in the player as a transcode: the server chose
/// its tracks, so the player must not choose them again.
pub fn transcoding(item_id: &str) -> bool {
    current_of(item_id).is_some_and(|r| r.play_method == PlayMethod::Transcode)
}

/// What the detail page chose for the next item.
#[derive(Default)]
pub struct State {
    /// Place of the audio track among the audio streams, from 0.
    pub wanted_audio: Option<usize>,
    /// The same for the subtitle; `Some(None)` is none.
    pub wanted_subtitle: Option<Option<usize>>,
    /// Subtitle files of the item that did not come (server indexes): a
    /// choice of one of them burns it in at once.
    pub failed_files: Vec<i64>,
    /// A reload asked for while a load was on its way: the audio, the
    /// subtitle and the burn-in of `stream_reload_with`. It runs when the
    /// item plays (`stream_poll`).
    pending_reload: Option<(Option<i64>, Option<i64>, bool)>,
    /// The user chose the audio track of the item in the player; the
    /// choice of the server does not replace it (`load_timeline`).
    pub explicit_audio: bool,
    /// The same for the subtitle.
    pub explicit_subtitle: bool,
}

/// A track the user chose, for [`State::chose`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chosen {
    Audio,
    Subtitle,
}

impl State {
    /// Notes a choice of a track for the item in the player: by the user
    /// on a page or in a menu, by a remote control, or by a group that
    /// keeps the track the player started with. The next item starts
    /// with no choice (`stream_begin`).
    pub fn chose(&mut self, track: Chosen) {
        match track {
            Chosen::Audio => self.explicit_audio = true,
            Chosen::Subtitle => self.explicit_subtitle = true,
        }
    }
}

/// Takes the settings of the config and ends a transcode when the app
/// quits. The subscription must live as long as the app.
pub fn install(config: &crate::config::Config, cx: &mut gpui_kit::App) -> gpui_kit::Subscription {
    MAX_BITRATE.store(config.max_bitrate.unwrap_or(0), Ordering::Relaxed);
    crate::adaptive::install(config);
    cx.on_app_quit(|_| {
        end_current();
        async {}
    })
}

/// Ends the transcode of the item that plays, on the server. Blocks for
/// the request; it is for the end of the app.
pub fn end_current() {
    let Some(resolved) = current() else { return };
    if resolved.play_method != PlayMethod::Transcode {
        return;
    }
    let id = &resolved.play_session_id;
    match resolved.client.stop_encoding(id) {
        Ok(()) => log::info!("ended the transcode of session {} at quit", &id[..id.len().min(8)]),
        Err(err) => log::warn!("stop encoding at quit: {err:#}"),
    }
}

// ----- the answer of the server ------------------------------------------------

/// The decision of the server for one load.
#[derive(Clone)]
pub struct Resolved {
    pub client: Client,
    /// The item this answer is for.
    pub item_id: String,
    pub url: String,
    pub play_method: PlayMethod,
    /// The server repacks the file and keeps its video as it is.
    pub remux: bool,
    pub play_session_id: String,
    pub media_source_id: String,
    /// Why the server transcodes, as it names them
    /// ("ContainerBitrateExceedsLimit").
    pub reasons: Vec<String>,
    /// Bits per second of the file.
    pub source_bitrate: Option<u64>,
    pub target: Option<Target>,
    pub streams: Vec<MediaStream>,
    /// The audio stream in a transcode (server index); none for direct play.
    pub audio_index: Option<i64>,
    /// The subtitle burned into a transcode (server index); none otherwise.
    pub burned_subtitle: Option<i64>,
    /// The subtitles in files of their own that mpv adds to the item; the
    /// one that starts shown has `select`.
    pub external_subs: Vec<SubtitleFile>,
    /// The external subtitle that starts selected, by server index.
    pub selected_external: Option<i64>,
    /// The load asked for the subtitle burned in (see `Load::burn_in`).
    pub burn_in: bool,
}

impl Resolved {
    /// "Direct play", "Direct stream" or "Transcoding", for the user.
    pub fn label(&self) -> &'static str {
        match (self.play_method, self.remux) {
            (PlayMethod::DirectPlay, _) => "Direct play",
            (PlayMethod::Transcode, true) => "Direct stream",
            (PlayMethod::Transcode, false) => "Transcoding",
        }
    }

    /// The name of the method in the playback reports: "DirectStream"
    /// for a remux, as the server names it.
    pub fn reported_method(&self) -> &'static str {
        self.play_method.reported(self.remux)
    }

    /// The source video stream.
    pub fn video(&self) -> Option<&MediaStream> {
        self.streams.iter().find(|s| s.kind == StreamKind::Video)
    }

    /// The streams of one kind that are in the file itself, in the order
    /// mpv numbers them (from 1).
    pub fn embedded(&self, kind: StreamKind) -> Vec<&MediaStream> {
        self.streams
            .iter()
            .filter(|s| s.kind == kind && !s.is_external)
            .collect()
    }

    /// The server index of the audio track mpv plays as its track `aid`.
    pub fn audio_of(&self, aid: i64) -> Option<i64> {
        self.embedded(StreamKind::Audio)
            .get((aid - 1).max(0) as usize)
            .map(|s| s.index)
    }

    /// The server index of the subtitle mpv shows as its track `sid`: a
    /// file added to the item, by the map the worker keeps of them
    /// (`PlayerStatus::external_sids`), else one in the file by its place
    /// (a transcode carries none).
    pub fn subtitle_of(&self, sid: i64, status: &PlayerStatus) -> Option<i64> {
        if let Some((index, _)) = status.external_sids.iter().find(|(_, id)| *id == sid) {
            return Some(*index);
        }
        if self.play_method == PlayMethod::Transcode {
            return None;
        }
        self.embedded(StreamKind::Subtitle)
            .get((sid - 1).max(0) as usize)
            .map(|s| s.index)
    }

    /// The id mpv gave an external subtitle, by server index; none for a
    /// file that is not added yet (on its way, chosen later, or lost).
    pub fn sid_of_external(&self, index: i64, status: &PlayerStatus) -> Option<i64> {
        status
            .external_sids
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, sid)| *sid)
    }

    /// The file of an external subtitle, by server index, as the player
    /// fetches one; it starts shown.
    pub fn external_file(&self, index: i64) -> Option<SubtitleFile> {
        let sub = self.external_subs.iter().find(|s| s.index == index)?;
        Some(SubtitleFile { select: true, ..sub.clone() })
    }

    /// True when a subtitle file that did not come is to be burned in
    /// instead: the item is a transcode (the server burns nothing into a
    /// file it sends as it is), the load did not ask for the burn-in
    /// already, and the server offered the track as a file.
    fn burns_in_instead(&self, index: i64) -> bool {
        self.play_method == PlayMethod::Transcode
            && !self.burn_in
            && self.external_subs.iter().any(|s| s.index == index)
    }
}

// ----- the request -------------------------------------------------------------

#[easy_ext::ext(StreamApi)]
pub(crate) impl Client {
    /// Asks the server how to play an item.
    fn playback_info(&self, load: &Load, max_bitrate: Option<u64>) -> Result<PlaybackInfo> {
        let path = format!("/Items/{}/PlaybackInfo", load.item_id);
        let mut body = self.post(&path, &request_body(self, load, max_bitrate))?;
        body.read_json().context("decode PlaybackInfo")
    }

    /// Ends the transcode of a play session on the server.
    fn stop_encoding(&self, play_session_id: &str) -> Result<()> {
        self.call(
            Method::Delete,
            "/Videos/ActiveEncodings",
            &[
                ("deviceId", self.device_id.to_string()),
                ("playSessionId", play_session_id.to_string()),
            ],
        )
    }
}

/// Turns the answer of the server into what the player loads. `wanted`
/// is the subtitle the user chose, as a server index, when there is one.
pub fn resolve(
    client: &Client,
    item_id: &str,
    info: &PlaybackInfo,
    wanted_subtitle: Option<i64>,
) -> Result<Resolved> {
    if let Some(code) = &info.error_code {
        return Err(anyhow!("the server cannot play this item: {code}"));
    }
    let source = info
        .source(item_id)
        .ok_or_else(|| anyhow!("the server has no media source for this item"))?;
    let play_session_id = info
        .play_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (url, play_method) = if source.supports_direct_play {
        (client.stream_url(item_id, &source.id), PlayMethod::DirectPlay)
    } else if let Some(path) = source
        .transcoding_url
        .as_deref()
        .filter(|_| source.supports_transcoding || source.supports_direct_stream)
    {
        // mpv sends the token in a header, so the key leaves the URL and
        // cannot show in the statistics of the player or in a log.
        (format!("{}{}", client.base, without_key(path)), PlayMethod::Transcode)
    } else {
        return Err(anyhow!("the server offers no way to play this item"));
    };

    let transcoding = play_method == PlayMethod::Transcode;
    let path = source.transcoding_url.as_deref().unwrap_or_default();
    let reasons: Vec<String> = query_value(path, "TranscodeReasons")
        .filter(|_| transcoding)
        .map(|list| list.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let number = |name: &str| query_value(path, name).and_then(|v| v.parse::<i64>().ok());
    let target = transcoding.then(|| Target {
        bitrate: (number("VideoBitrate").unwrap_or(0) + number("AudioBitrate").unwrap_or(0))
            .max(0) as u64,
        video_codec: query_value(path, "VideoCodec").unwrap_or("h264").to_string(),
        audio_codec: query_value(path, "AudioCodec")
            .unwrap_or_default()
            .split(',')
            .next()
            .unwrap_or_default()
            .to_string(),
        container: source
            .transcoding_container
            .clone()
            .or_else(|| query_value(path, "SegmentContainer").map(str::to_string))
            .unwrap_or_default(),
        protocol: source.transcoding_sub_protocol.clone().unwrap_or_default(),
    });
    let audio_index = transcoding
        .then(|| number("AudioStreamIndex").or(source.default_audio_stream_index))
        .flatten();
    let burned_subtitle = transcoding.then(|| number("SubtitleStreamIndex")).flatten();

    // The subtitle that starts on: the one the user chose, else the one
    // the server names for the user.
    let chosen = wanted_subtitle
        .or(source.default_subtitle_stream_index)
        .filter(|i| *i >= 0);
    let mut external_subs: Vec<SubtitleFile> = source
        .media_streams
        .iter()
        .filter(|s| s.kind == StreamKind::Subtitle && s.delivery_method == Some(SubtitleDelivery::External))
        .filter_map(|s| {
            let url = s.delivery_url.as_deref()?;
            // The player sends the token in a header; the key in the
            // address would show in a log.
            Some(SubtitleFile {
                index: s.index,
                url: format!("{}{}", client.base, without_key(url)),
                title: stream_title(s),
                lang: s.language.clone().unwrap_or_default(),
                select: false,
                graphical: !s.is_text_subtitle_stream,
            })
        })
        .collect();
    let selected_external = chosen.filter(|i| external_subs.iter().any(|s| s.index == *i));
    for sub in &mut external_subs {
        sub.select = selected_external == Some(sub.index);
    }

    Ok(Resolved {
        client: client.clone(),
        item_id: item_id.to_string(),
        url,
        play_method,
        // The server offers the stream it makes as a "direct stream" when
        // it keeps the video of the file.
        remux: transcoding && source.supports_direct_stream,
        play_session_id,
        media_source_id: source.id.clone(),
        reasons,
        source_bitrate: source.bitrate,
        target,
        streams: source.media_streams.clone(),
        audio_index,
        burned_subtitle,
        external_subs,
        selected_external,
        burn_in: false,
    })
}

/// Asks the server and resolves. The first answer names the file; a
/// second request follows when that answer changes what to ask for:
///
/// - "Auto": when the connection does not carry the bitrate of the file,
///   the rung that fits is the limit (see `adaptive`). A manual limit
///   skips this.
/// - A transcode without a choice of the user: a track in the audio
///   language of the user takes the place of the one the server names,
///   since the server can name the track of an earlier playback instead
///   (the rule of the timeline, see `timeline_info`).
pub fn negotiate(client: &Client, load: &Load) -> Result<Resolved> {
    let mut max_bitrate = max_bitrate();
    let info = client.playback_info(load, max_bitrate)?;
    let first = resolve(client, &load.item_id, &info, load.subtitle)?;
    let mut again = None;
    if max_bitrate.is_none() {
        let codec = first.video().and_then(|v| v.codec.as_deref());
        if let Some(cap) = crate::adaptive::auto_cap(first.source_bitrate, codec) {
            max_bitrate = Some(cap);
            again = Some(load.clone());
        }
    }
    // A limit under the file makes the second answer a transcode.
    let transcode = first.play_method == PlayMethod::Transcode || again.is_some();
    if transcode
        && load.audio.is_none()
        && let Some(language) = client.audio_language()
    {
        let named = first.audio_index.or_else(|| {
            info.source(&load.item_id).and_then(|s| s.default_audio_stream_index)
        });
        let preferred = first
            .embedded(StreamKind::Audio)
            .iter()
            .find(|s| s.language.as_deref().is_some_and(|l| crate::subtitles::same_language(l, &language)))
            .map(|s| s.index);
        if preferred.is_some() && preferred != named {
            again = Some(Load { audio: preferred, ..load.clone() });
        }
    }
    let mut resolved = match again {
        Some(again) => {
            let info = client.playback_info(&again, max_bitrate)?;
            resolve(client, &load.item_id, &info, load.subtitle)?
        }
        None => first,
    };
    resolved.burn_in = load.burn_in;
    // The track the server burns in on request is not wanted as a file
    // as well (the server still names it as one).
    if load.burn_in && let Some(burned) = resolved.burned_subtitle {
        resolved.external_subs.retain(|s| s.index != burned);
        if resolved.selected_external == Some(burned) {
            resolved.selected_external = None;
        }
    }
    Ok(resolved)
}

/// Starts an item in the player: the player shows it as opening at once,
/// the server is asked on a thread of its own, and the player loads what
/// the server answered. A load that comes later wins over this one.
pub fn start(player: &Player, client: Client, load: Load) {
    let generation = player.prepare(&load.title, load.start_secs, load.paused);
    // A downloaded item plays from its file on this Mac, with no question
    // to the server, so it also plays offline.
    if let Some(path) = crate::downloads::local_source(player, client.server_id.as_deref(), &load.item_id) {
        *CURRENT.lock().unwrap() = None;
        player.play_prepared(
            generation,
            PlayRequest {
                client,
                item_id: load.item_id,
                url: path,
                title: load.title,
                start_secs: load.start_secs,
                paused: load.paused,
                token: load.token,
                play_session_id: None,
                play_method: PlayMethod::DirectPlay.as_str().to_string(),
                transcoding: false,
                media_source_id: String::new(),
                subtitles: Vec::new(),
            },
        );
        return;
    }
    let player = player.clone();
    let _ = std::thread::Builder::new()
        .name("playback-info".into())
        .spawn(move || match negotiate(&client, &load) {
            Ok(resolved) => {
                log::info!(
                    "play {}: {} ({})",
                    load.item_id,
                    resolved.play_method.as_str(),
                    resolved.reasons.join(",")
                );
                let resolved = Arc::new(resolved);
                let subtitles = resolved.external_subs.clone();
                let request = PlayRequest {
                    client,
                    item_id: load.item_id,
                    url: resolved.url.clone(),
                    title: load.title,
                    start_secs: load.start_secs,
                    paused: load.paused,
                    token: load.token,
                    play_session_id: Some(resolved.play_session_id.clone()),
                    play_method: resolved.reported_method().to_string(),
                    transcoding: resolved.play_method == PlayMethod::Transcode,
                    media_source_id: resolved.media_source_id.clone(),
                    subtitles,
                };
                hand_over(&player, generation, resolved, request);
            }
            Err(err) => {
                log::warn!("playback info for {}: {err:#}", load.item_id);
                // 404: the server no longer has the item. A file replaced
                // by a new release gets a new id (seen on 2026-10-21: the
                // next episode of a queue, swapped while the one before
                // it played); the app looks for the replacement.
                let gone = crate::jellyfin::status_of(&err) == Some(404);
                let message = match gone {
                    true => "the server no longer has this item".to_string(),
                    false => format!("{err:#}"),
                };
                player.fail_load(generation, load.token, &message, gone);
            }
        });
}

/// Hands the answer of the server to the player and remembers it, or ends
/// a transcode that nobody will ask for because a later load came first.
fn hand_over(player: &Player, generation: u64, resolved: Arc<Resolved>, request: PlayRequest) {
    let accepted = {
        let mut current = CURRENT.lock().unwrap();
        let accepted = player.play_prepared(generation, request);
        if accepted {
            *current = Some(resolved.clone());
        }
        accepted
    };
    // The request to the server goes with `CURRENT` free: the UI reads it
    // at every poll of the player, and a slow server must not hold it.
    if !accepted && resolved.play_method == PlayMethod::Transcode {
        let _ = resolved.client.stop_encoding(&resolved.play_session_id);
    }
}

/// The streams of an item as its lists have them: those of its first
/// source, else its own.
fn item_streams(item: &Item) -> &[MediaStream] {
    item.media_sources
        .first()
        .map_or(&item.media_streams, |source| &source.media_streams)
}

// ----- the app ---------------------------------------------------------------------

impl Bloom {
    /// The answer of the server for the item in the player.
    fn stream_resolved(&self) -> Option<Arc<Resolved>> {
        current_of(&self.playback.playing.as_ref()?.id)
    }

    /// Starts an item for the app: with the tracks the detail page chose,
    /// when it chose some.
    pub fn stream_begin(&mut self, item: &Item, start_secs: f64) {
        let (audio, subtitle) = self.stream_take_choice(item);
        let Some(session) = &self.session else { return };
        self.stream.failed_files.clear();
        self.stream.pending_reload = None;
        self.stream.explicit_audio = false;
        self.stream.explicit_subtitle = false;
        start(
            &self.player,
            session.client.clone(),
            Load {
                item_id: item.id.clone(),
                title: item.display_title(),
                start_secs,
                paused: false,
                token: 0,
                audio,
                subtitle,
                burn_in: false,
            },
        );
        // The server got the choice, for a transcode.
        self.stream_choice_to_player(item, audio, subtitle);
    }

    /// The tracks the detail page chose for the next item, as server
    /// indexes (-1 is no subtitle); taken, so they count once.
    fn stream_take_choice(&mut self, item: &Item) -> (Option<i64>, Option<i64>) {
        let audio = self.stream.wanted_audio.take().and_then(|place| {
            item.streams(StreamKind::Audio).get(place).map(|s| s.index)
        });
        let subtitle = self.stream.wanted_subtitle.take().map(|choice| match choice {
            Some(place) => item.streams(StreamKind::Subtitle).get(place).map_or(-1, |s| s.index),
            None => -1,
        });
        (audio, subtitle)
    }

    /// The choice of the detail page that no load of the app took: the
    /// item went to a SyncPlay group, whose load asks for no track. mpv
    /// keeps the choice for that load.
    pub fn stream_choice_left(&mut self, item: &Item) {
        let (audio, subtitle) = self.stream_take_choice(item);
        self.stream_choice_to_player(item, audio, subtitle);
    }

    /// Gives mpv the tracks chosen for the load that comes, by their
    /// server indexes: the file itself has all its tracks. The load of a
    /// transcode drops the choice (`player.rs`), as the server made it. A
    /// subtitle in a file of its own has no id yet: it starts shown
    /// (`SubtitleFile::select`).
    fn stream_choice_to_player(&mut self, item: &Item, audio: Option<i64>, subtitle: Option<i64>) {
        let streams = item_streams(item);
        if let Some(index) = audio {
            self.stream.chose(Chosen::Audio);
            if let Some(aid) = mpv_id(streams, StreamKind::Audio, index) {
                self.player.set_audio(Some(aid));
            }
        }
        if let Some(index) = subtitle {
            self.stream.chose(Chosen::Subtitle);
            if index < 0 {
                self.player.set_subtitle(None);
            } else if let Some(sid) = mpv_id(streams, StreamKind::Subtitle, index) {
                self.player.set_subtitle(Some(sid));
            }
        }
    }

    /// Loads the item again at its position, with the tracks that play
    /// now, after a change of the quality or of a track the server picks.
    /// `audio` and `subtitle` are server indexes that replace the ones
    /// that play.
    pub fn stream_reload(&mut self, audio: Option<i64>, subtitle: Option<i64>) {
        self.stream_reload_with(audio, subtitle, false);
    }

    /// A subtitle file the user chose did not come (a server before 12.2
    /// cannot serve a VobSub as a file): the item loads again with the
    /// track burned in, once. Not for a file the server sends as it is:
    /// the server burns nothing into that, so a load would change nothing.
    pub fn subtitle_file_failed(&mut self, index: i64) {
        let Some(resolved) = self.stream_resolved() else { return };
        if !resolved.burns_in_instead(index) {
            if resolved.play_method != PlayMethod::Transcode {
                log::warn!("subtitle {index} did not come as a file");
            }
            return;
        }
        log::info!("subtitle {index} did not come as a file: burned in instead");
        if !self.stream.failed_files.contains(&index) {
            self.stream.failed_files.push(index);
        }
        self.stream_reload_with(None, Some(index), true);
    }

    fn stream_reload_with(&mut self, audio: Option<i64>, subtitle: Option<i64>, burn_in: bool) {
        let (Some(item), Some(session)) = (self.playback.playing.as_ref(), self.session.as_ref()) else {
            return;
        };
        match self.playback.status.state {
            PlayState::Playing => {}
            // A load is on its way, and it took the settings as they
            // were: this one follows it (`stream_poll`).
            PlayState::Starting => {
                let before = self.stream.pending_reload.take().unwrap_or_default();
                self.stream.pending_reload =
                    Some((audio.or(before.0), subtitle.or(before.1), burn_in || before.2));
                return;
            }
            _ => return,
        }
        let Some(resolved) = current_of(&item.id) else { return };
        let status = &self.playback.status;
        let selected = |kind: TrackKind| status.tracks.iter().find(|t| t.kind == kind && t.selected);
        let audio = audio.or_else(|| match resolved.play_method {
            PlayMethod::Transcode => resolved.audio_index,
            PlayMethod::DirectPlay => selected(TrackKind::Audio).and_then(|t| resolved.audio_of(t.id)),
        });
        let subtitle = subtitle.or_else(|| {
            resolved.burned_subtitle.or_else(|| {
                Some(selected(TrackKind::Sub).and_then(|t| resolved.subtitle_of(t.id, status)).unwrap_or(-1))
            })
        });
        start(
            &self.player,
            session.client.clone(),
            Load {
                item_id: item.id.clone(),
                title: item.display_title(),
                start_secs: status.position.max(0.),
                paused: status.paused,
                token: 0,
                audio,
                subtitle,
                burn_in,
            },
        );
    }

    /// Called with every poll of the player: a reload that waited for the
    /// load before it runs once the item plays.
    pub fn stream_poll(&mut self) {
        if self.stream.pending_reload.is_none() {
            return;
        }
        match self.playback.status.state {
            PlayState::Starting => {}
            PlayState::Playing => {
                if let Some((audio, subtitle, burn_in)) = self.stream.pending_reload.take() {
                    self.stream_reload_with(audio, subtitle, burn_in);
                }
            }
            _ => self.stream.pending_reload = None,
        }
    }

    /// Sets the bitrate the user allows (none is "Auto"), keeps it in the
    /// config, and reloads the item that plays.
    pub fn set_max_bitrate(&mut self, bitrate: Option<u64>, cx: &mut Context<Self>) {
        let bitrate = bitrate.filter(|b| *b > 0);
        if self.config.max_bitrate == bitrate && max_bitrate() == bitrate {
            return;
        }
        self.config.max_bitrate = bitrate;
        MAX_BITRATE.store(bitrate.unwrap_or(0), Ordering::Relaxed);
        self.save_config(cx);
        if self.playback.open {
            self.stream_reload(None, None);
        }
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// The "Quality" entry of the player settings menu: "Auto" and the
    /// rungs of the ladder under the bitrate of the file.
    pub fn quality_menu_item(&self, cx: &mut Context<Self>) -> MenuItem {
        let this = cx.weak_entity();
        let resolved = self.stream_resolved();
        let cap = max_bitrate();
        let source = resolved.as_ref().and_then(|r| r.source_bitrate);
        let codec = resolved
            .as_ref()
            .and_then(|r| r.video().and_then(|v| v.codec.clone()));
        let mut items = Vec::new();
        let handle = this.clone();
        // What Auto does: the file, or less when the connection is slow.
        let lowered = matches!(crate::adaptive::auto_choice(), crate::adaptive::Choice::Rung(_));
        let auto_detail = match (lowered, source) {
            (true, Some(bitrate)) => format!("Lowered for the connection · the file has {}", bitrate_label(bitrate)),
            (true, None) => "Lowered for the connection".to_string(),
            (false, Some(bitrate)) => format!("The file as it is · {}", bitrate_label(bitrate)),
            (false, None) => "The file as it is".to_string(),
        };
        items.push(
            MenuItem::new(
                "settings.quality.auto",
                SharedString::from(format!("Auto · {}", crate::adaptive::auto_label())),
            )
                .detail(auto_detail)
                .radio(cap.is_none())
                .on_click(move |_, _, cx| {
                    handle.update(cx, |t, cx| t.set_max_bitrate(None, cx)).ok();
                }),
        );
        for rung in ladder(source, codec.as_deref()) {
            let handle = this.clone();
            items.push(
                MenuItem::new(
                    SharedString::from(format!("settings.quality.{}", rung.bitrate)),
                    rung.label,
                )
                .detail(rung.height)
                .radio(cap == Some(rung.bitrate))
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |t, cx| t.set_max_bitrate(Some(rung.bitrate), cx))
                        .ok();
                }),
            );
        }
        let now = match (cap, resolved.as_ref()) {
            (Some(bitrate), _) => bitrate_label(bitrate),
            (None, Some(_)) => format!("Auto · {}", crate::adaptive::auto_label()),
            (None, None) => "Auto".to_string(),
        };
        MenuItem::submenu("settings.quality", "Quality", items).detail(now)
    }

    /// The track entries of a menu while a transcode plays: the server
    /// picks the audio and burns a subtitle in, so these are its streams
    /// and a choice loads the item again. None with direct play, where
    /// mpv switches its own tracks.
    pub fn stream_track_items(&self, kind: TrackKind, cx: &mut Context<Self>) -> Option<Vec<MenuItem>> {
        let resolved = self.stream_resolved().filter(|r| r.play_method == PlayMethod::Transcode)?;
        let this = cx.weak_entity();
        let mut items = Vec::new();
        if kind == TrackKind::Audio {
            for stream in resolved.embedded(StreamKind::Audio) {
                let (index, handle) = (stream.index, this.clone());
                items.push(
                    MenuItem::new(
                        SharedString::from(format!("track.audio.{index}")),
                        stream_title(stream),
                    )
                    .radio(resolved.audio_index == Some(index))
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |t, _| t.stream_audio(index)).ok();
                    }),
                );
            }
        } else {
            let status = &self.playback.status;
            let shown = self.shown_subtitle(&resolved);
            let handle = this.clone();
            items.push(
                MenuItem::new("track.sub.none", "Off")
                    .radio(shown.is_none())
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |t, _| t.stream_subtitle(-1))
                            .ok();
                    }),
            );
            for stream in resolved.streams.iter().filter(|s| s.kind == StreamKind::Subtitle) {
                let (index, handle) = (stream.index, this.clone());
                // A track with no file is burned in by the server; a file
                // on its way says so (an image track comes when chosen).
                let detail = match resolved.external_subs.iter().any(|s| s.index == index) {
                    false => "Burned in",
                    true if status.pending_subs.contains(&index) && resolved.sid_of_external(index, status).is_none() => "Loading…",
                    true => "",
                };
                items.push(
                    MenuItem::new(SharedString::from(format!("track.sub.{index}")), stream_title(stream))
                        .detail(detail)
                        .radio(shown == Some(index))
                        .on_click(move |_, _, cx| {
                            handle.update(cx, |t, _| t.stream_subtitle(index)).ok();
                        }),
                );
            }
        }
        if items.is_empty() {
            items.push(
                MenuItem::new(
                    SharedString::from(format!("track.{kind}.empty")),
                    "No tracks",
                )
                .disabled(true),
            );
        }
        Some(items)
    }

    /// The subtitle on the picture, by server index: the one mpv shows,
    /// else the one burned in.
    fn shown_subtitle(&self, resolved: &Resolved) -> Option<i64> {
        let status = &self.playback.status;
        status
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Sub && t.selected)
            .and_then(|t| resolved.subtitle_of(t.id, status))
            .or(resolved.burned_subtitle)
    }

    /// Plays an audio track by its server index. In a transcode the
    /// server picks the track, so the item loads again; the file itself
    /// has all its tracks, and mpv switches. With `stream_subtitle` this
    /// is the one way to a track by its server index: the menus, a remote
    /// control and the keys come here.
    pub fn stream_audio(&mut self, index: i64) {
        let Some(item) = self.playback.playing.as_ref() else { return };
        let resolved = current_of(&item.id);
        let transcode = resolved.as_ref().is_some_and(|r| r.play_method == PlayMethod::Transcode);
        // With no answer of the server (a downloaded item, or a load on
        // its way) the streams are those of the item.
        let streams = resolved.as_ref().map_or(item_streams(item), |r| r.streams.as_slice());
        let Some(aid) = mpv_id(streams, StreamKind::Audio, index) else {
            log::debug!("no audio track for stream {index}");
            return;
        };
        self.stream.chose(Chosen::Audio);
        if !transcode {
            self.player.set_audio(Some(aid));
        } else if resolved.is_some_and(|r| r.audio_index != Some(index)) {
            self.stream_reload(Some(index), None);
        }
    }

    /// Shows a subtitle by server index (-1 for none): a subtitle the
    /// server gives as a file switches in mpv (an image track is fetched
    /// first, when it is chosen), one in the file of a direct play is
    /// mpv's own track, and any other one is burned in by the server, so
    /// the item loads again.
    pub fn stream_subtitle(&mut self, index: i64) {
        let Some(item) = self.playback.playing.as_ref() else { return };
        let Some(resolved) = current_of(&item.id) else {
            // A downloaded item, or a load on its way: the tracks of the
            // file are mpv's.
            let sid = mpv_id(item_streams(item), StreamKind::Subtitle, index);
            if index < 0 || sid.is_some() {
                self.stream.chose(Chosen::Subtitle);
                self.player.set_subtitle(sid);
            }
            return;
        };
        if index >= 0 && !resolved.streams.iter().any(|s| s.kind == StreamKind::Subtitle && s.index == index) {
            log::debug!("no subtitle for stream {index}");
            return;
        }
        self.stream.chose(Chosen::Subtitle);
        let burned = resolved.burned_subtitle.is_some();
        if index < 0 {
            if burned {
                self.stream_reload(None, Some(-1));
            } else {
                self.player.set_subtitle(None);
            }
            return;
        }
        if self.stream.failed_files.contains(&index) && resolved.play_method == PlayMethod::Transcode {
            self.stream_reload_with(None, Some(index), true);
            return;
        }
        let status = self.player.status();
        if !burned {
            if let Some(sid) = resolved.sid_of_external(index, &status) {
                self.player.set_subtitle(Some(sid));
                return;
            }
            // A file not added yet: an image track, or a text one the
            // server did not give at the start. mpv shows it when it comes.
            if let Some(file) = resolved.external_file(index) {
                self.player.fetch_subtitle_file(file);
                return;
            }
        }
        if resolved.play_method != PlayMethod::Transcode
            && let Some(sid) = mpv_id(&resolved.streams, StreamKind::Subtitle, index)
        {
            self.player.set_subtitle(Some(sid));
            return;
        }
        self.stream_reload(None, Some(index));
    }

    /// The next track of a kind (audio or subtitles), for a key. The file
    /// itself has all its tracks and mpv steps through them; in a
    /// transcode the tracks are the server's, in the order of the menu
    /// (for subtitles "Off" first).
    pub fn stream_cycle(&mut self, kind: TrackKind) {
        let Some(resolved) = self.stream_resolved().filter(|r| r.play_method == PlayMethod::Transcode) else {
            match kind {
                TrackKind::Audio => self.stream.chose(Chosen::Audio),
                _ => self.stream.chose(Chosen::Subtitle),
            }
            // mpv's properties of the tracks have the names of the types.
            self.player.command("cycle", &[&kind.to_string()]);
            return;
        };
        if kind == TrackKind::Audio {
            let tracks: Vec<i64> = resolved.embedded(StreamKind::Audio).iter().map(|s| s.index).collect();
            if let Some(next) = next_track(&tracks, resolved.audio_index) {
                self.stream_audio(next);
            }
        } else {
            let tracks: Vec<i64> = std::iter::once(-1)
                .chain(resolved.streams.iter().filter(|s| s.kind == StreamKind::Subtitle).map(|s| s.index))
                .collect();
            let shown = self.shown_subtitle(&resolved).unwrap_or(-1);
            if let Some(next) = next_track(&tracks, Some(shown)) {
                self.stream_subtitle(next);
            }
        }
    }

    /// The card of the "Playback info" overlay: how the item plays.
    pub fn render_stream_info(&self, _cx: &mut Context<Self>) -> Option<Div> {
        if !self.playback.info || !self.playback.open {
            return None;
        }
        let resolved = self.stream_resolved();
        let ink = crate::ui::tone::INK;
        let quiet = crate::ui::tone::INK_70;
        let line = |text: String| div().text_size(px(12.)).text_color(quiet).child(text);
        let mut lines: Vec<Div> = Vec::new();
        let title = match &resolved {
            Some(r) => r.label().to_string(),
            None => "Opening".to_string(),
        };
        if let Some(r) = &resolved {
            if !r.reasons.is_empty() {
                lines.push(line(r.reasons.join(", ")));
            }
            if let Some(video) = r.video() {
                let mut parts = Vec::new();
                if let (Some(w), Some(h)) = (video.width, video.height) {
                    parts.push(format!("{w}×{h}"));
                }
                if let Some(codec) = &video.codec {
                    parts.push(codec.to_uppercase());
                }
                if let Some(bitrate) = r.source_bitrate {
                    parts.push(bitrate_label(bitrate));
                }
                lines.push(line(format!("Source: {}", parts.join(" · "))));
            }
            if let Some(target) = &r.target {
                lines.push(line(format!(
                    "Target: {} · {}/{} · {} over {}",
                    bitrate_label(target.bitrate),
                    target.video_codec,
                    target.audio_codec,
                    target.container,
                    target.protocol
                )));
            }
        }
        let status = &self.playback.status;
        if status.video_w > 0 && status.video_h > 0 {
            lines.push(line(format!("Decoding: {}×{}", status.video_w, status.video_h)));
        }
        lines.push(line(format!(
            "Limit: {}",
            max_bitrate().map_or_else(
                || format!("Auto · {}", crate::adaptive::auto_label()),
                bitrate_label
            )
        )));
        lines.push(line(format!("Connection: {}", crate::adaptive::link_line())));
        if let Some(r) = &resolved {
            lines.push(line(format!(
                "Session {}",
                &r.play_session_id[..r.play_session_id.len().min(8)]
            )));
        }
        Some(
            div()
                .absolute()
                .top(px(70.))
                .right(px(24.))
                .w(px(300.))
                .p(px(14.))
                .rounded(px(16.))
                .border_1()
                .border_color(crate::ui::tone::EDGE)
                .child(glass(px(16.), rgba(0x1c1c1cd9)))
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(ink)
                        .child(title),
                )
                .children(lines),
        )
    }

    /// The `quality` debug command: `state`, `set <kbps|auto>`, `menu`,
    /// `info`.
    pub fn debug_quality(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" | "state" => {
                let status = &self.playback.status;
                let cap = max_bitrate().map_or("auto".to_string(), |b| (b / 1000).to_string());
                let link = crate::adaptive::describe(self.sync.in_group());
                match current() {
                    Some(r) => format!(
                        "method={} cap={cap} session={} reasons=[{}] source={} target={} decoded={}x{} \
                         audio={:?} burned={:?} external_subs={} url={} pos={:.1} paused={} {link}",
                        r.play_method.as_str(),
                        if r.play_session_id.is_empty() { "no" } else { "yes" },
                        r.reasons.join(","),
                        r.source_bitrate.unwrap_or(0),
                        r.target.as_ref().map_or(0, |t| t.bitrate),
                        status.video_w,
                        status.video_h,
                        r.audio_index,
                        r.burned_subtitle,
                        r.external_subs.len(),
                        if r.play_method == PlayMethod::DirectPlay { "static" } else { "hls" },
                        status.position,
                        status.paused,
                    ),
                    None => format!("method=none cap={cap} session=no {link}"),
                }
            }
            "set" => {
                let bitrate = match arg {
                    "auto" | "" => None,
                    kbps => match kbps.parse::<u64>() {
                        Ok(kbps) => Some(kbps * 1000),
                        Err(_) => return "error: usage: quality set <kbps|auto>".into(),
                    },
                };
                self.set_max_bitrate(bitrate, cx);
                format!("cap={}", arg)
            }
            "menu" => {
                self.rebuild_track_menus(cx);
                self.playback.settings_menu
                    .update(cx, |menu, cx| menu.open_submenu("Quality", window, cx));
                "menu".into()
            }
            "info" => {
                self.toggle_playback_info(cx);
                format!("info={}", self.playback.info)
            }
            // Quits the app as the Quit menu does, for a test of the end
            // of a transcode at quit.
            "quit" => {
                cx.quit();
                "quit".into()
            }
            // The measurement and the stalls: see `adaptive`.
            _ => self.debug_adaptive(verb, arg, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const DIRECT: &str = include_str!("../fixtures/playback-info-direct.json");
    const TRANSCODE: &str = include_str!("../fixtures/playback-info-transcode.json");
    const ITEM: &str = "00112233445566778899aabbccddeeff";

    fn client() -> Client {
        Client::new("https://media.example.com", "device-1").with_session("tok", "user-1")
    }

    #[test]
    fn direct_play_keeps_the_static_url() {
        let info: PlaybackInfo = serde_json::from_str(DIRECT).unwrap();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.play_method, PlayMethod::DirectPlay);
        assert_eq!(
            resolved.url,
            format!("https://media.example.com/Videos/{ITEM}/stream?static=true&mediaSourceId={ITEM}")
        );
        assert_eq!(resolved.play_session_id, "aaaaaaaabbbbccccddddeeeeffff0001");
        assert_eq!(resolved.media_source_id, ITEM);
        assert!(resolved.reasons.is_empty());
        assert!(resolved.target.is_none());
        assert_eq!(resolved.source_bitrate, Some(3_794_694));
        assert!(resolved.external_subs.is_empty());
        // mpv's first audio track is stream 1; its first subtitle is stream 2.
        assert_eq!(resolved.audio_of(1), Some(1));
        assert_eq!(resolved.subtitle_of(1, &PlayerStatus::default()), Some(2));
    }

    #[test]
    fn transcode_takes_the_hls_url_and_the_text_subtitle() {
        let info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.play_method, PlayMethod::Transcode);
        assert!(resolved.url.starts_with("https://media.example.com/videos/"));
        assert!(resolved.url.contains("master.m3u8?"));
        assert_eq!(resolved.play_session_id, "aaaaaaaabbbbccccddddeeeeffff0002");
        assert_eq!(resolved.reasons, vec!["ContainerBitrateExceedsLimit"]);
        let target = resolved.target.as_ref().unwrap();
        assert_eq!(target.bitrate, 1_116_000 + 384_000);
        assert_eq!((target.video_codec.as_str(), target.audio_codec.as_str()), ("h264", "aac"));
        assert_eq!((target.container.as_str(), target.protocol.as_str()), ("ts", "hls"));
        assert_eq!(resolved.audio_index, Some(1));
        assert_eq!(resolved.burned_subtitle, None);
        // The text subtitle comes as a file and starts on: the server
        // names it as the default.
        assert_eq!(resolved.external_subs.len(), 1);
        assert_eq!(resolved.external_subs[0].index, 2);
        assert!(resolved.external_subs[0].url.starts_with("https://media.example.com/Videos/"));
        assert!(!resolved.external_subs[0].url.contains("ApiKey="), "the key stays out of the address");
        assert_eq!(resolved.selected_external, Some(2));
        assert!(!resolved.external_subs[0].graphical);
        // The file is not added yet; once the worker has, its id maps.
        let mut status = PlayerStatus::default();
        assert_eq!(resolved.sid_of_external(2, &status), None);
        status.external_sids.push((2, 1));
        assert_eq!(resolved.sid_of_external(2, &status), Some(1));
        assert_eq!(resolved.subtitle_of(1, &status), Some(2));
        assert_eq!(resolved.subtitle_of(2, &status), None);
        // The user chose no subtitle.
        let none = resolve(&client(), ITEM, &info, Some(-1)).unwrap();
        assert_eq!(none.selected_external, None);
    }

    /// An image track the server hands over as a file (a PGS marked
    /// External, as the profile asks) is a file mpv adds, not a burn-in:
    /// the address is the server's, without the key, and the transcode
    /// carries no subtitle.
    #[test]
    fn transcode_takes_a_pgs_track_as_a_file() {
        let mut json: serde_json::Value = serde_json::from_str(TRANSCODE).unwrap();
        json["MediaSources"][0]["MediaStreams"].as_array_mut().unwrap().push(serde_json::json!({
            "Codec": "PGSSUB", "Language": "eng", "DisplayTitle": "English - PGSSUB", "Type": "Subtitle",
            "Index": 3, "IsExternal": false, "DeliveryMethod": "External",
            "DeliveryUrl": format!("/Videos/{ITEM}/{ITEM}/Subtitles/3/0/Stream.pgssub?ApiKey=REDACTED"),
            "IsTextSubtitleStream": false, "SupportsExternalStream": true
        }));
        let info: PlaybackInfo = serde_json::from_value(json).unwrap();
        let resolved = resolve(&client(), ITEM, &info, Some(3)).unwrap();
        assert_eq!(resolved.play_method, PlayMethod::Transcode);
        assert_eq!(resolved.burned_subtitle, None);
        let indexes: Vec<i64> = resolved.external_subs.iter().map(|s| s.index).collect();
        assert_eq!(indexes, vec![2, 3]);
        let pgs = &resolved.external_subs[1];
        assert_eq!(pgs.url, format!("https://media.example.com/Videos/{ITEM}/{ITEM}/Subtitles/3/0/Stream.pgssub?"));
        assert_eq!(pgs.title, "English - PGSSUB");
        assert!(pgs.graphical);
        assert_eq!(resolved.selected_external, Some(3));
        let file = resolved.external_file(3).unwrap();
        assert!(file.select && file.graphical && file.index == 3);
        // The text file did not come (the server failed it), the PGS did:
        // the PGS is mpv's first file all the same.
        let mut status = PlayerStatus::default();
        status.external_sids.push((3, 1));
        assert_eq!(resolved.sid_of_external(3, &status), Some(1));
        assert_eq!(resolved.sid_of_external(2, &status), None);
        assert_eq!(resolved.subtitle_of(1, &status), Some(3));
    }

    /// Finding 9 of the review: the URL of a direct play names the media
    /// source the server resolved, also when its id is not the item id.
    #[test]
    fn direct_play_names_the_resolved_source() {
        let mut info: PlaybackInfo = serde_json::from_str(DIRECT).unwrap();
        info.media_sources[0].id = "ffeeddccbbaa99887766554433221100".into();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.media_source_id, "ffeeddccbbaa99887766554433221100");
        assert_eq!(
            resolved.url,
            format!("https://media.example.com/Videos/{ITEM}/stream?static=true&mediaSourceId=ffeeddccbbaa99887766554433221100")
        );
    }

    /// Finding 4 of the review: ending the transcode of a load that lost to
    /// a later one must not hold `CURRENT`, which the UI reads at each poll.
    #[test]
    fn a_stale_transcode_ends_without_holding_current() {
        use std::io::{Read as _, Write as _};
        // A server that says when the DELETE reached it, and answers it
        // 1.5 s later.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (arrived_tx, arrived) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).unwrap_or(0);
            let _ = arrived_tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            std::thread::sleep(Duration::from_millis(1500));
            let _ = socket.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), "device-1").with_session("tok", "user-1");
        let info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        let resolved = Arc::new(resolve(&client, ITEM, &info, None).unwrap());
        assert_eq!(resolved.play_method, PlayMethod::Transcode);
        let player = Player::default();
        let stale = player.prepare("first", 0., false);
        let _later = player.prepare("second", 0., false);
        let request = PlayRequest {
            client: client.clone(),
            item_id: ITEM.into(),
            url: resolved.url.clone(),
            title: "first".into(),
            start_secs: 0.,
            paused: false,
            token: 1,
            play_session_id: Some(resolved.play_session_id.clone()),
            play_method: "Transcode".into(),
            transcoding: true,
            media_source_id: ITEM.into(),
            subtitles: Vec::new(),
        };
        let installer = {
            let player = player.clone();
            std::thread::spawn(move || hand_over(&player, stale, resolved, request))
        };
        // The DELETE is in flight (the server has it and waits), and the
        // UI asks what plays.
        let request_line = arrived.recv_timeout(Duration::from_secs(5)).expect("the DELETE reached the server");
        assert!(request_line.starts_with("DELETE "), "not a DELETE: {request_line}");
        let started = Instant::now();
        let _ = current();
        let waited = started.elapsed();
        installer.join().unwrap();
        server.join().unwrap();
        assert!(waited < Duration::from_millis(500), "current() waited {waited:?} for the server");
    }

    /// The stream the server makes of a file is a transcode for the app,
    /// also when the server calls it a direct stream (it keeps the video):
    /// the server chose the audio track, and its job must be ended.
    #[test]
    fn a_direct_stream_over_the_transcoding_url_is_a_transcode() {
        let mut info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        info.media_sources[0].supports_direct_stream = true;
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.play_method, PlayMethod::Transcode);
        assert!(resolved.remux);
        assert_eq!(resolved.label(), "Direct stream");
        assert_eq!(resolved.play_method.as_str(), "Transcode");
        // The reports keep the name of the server for it.
        assert_eq!(resolved.reported_method(), "DirectStream");
        assert!(resolved.target.is_some());
        assert_eq!(resolved.audio_index, Some(1));
        let plain: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        assert_eq!(resolve(&client(), ITEM, &plain, None).unwrap().label(), "Transcoding");
    }

    /// The answer of the server for the item before is not the answer for
    /// the item that loads: after a transcode, the next item is no
    /// transcode until the server said so.
    #[test]
    fn the_answer_for_another_item_does_not_count() {
        let info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.item_id, ITEM);
        let held = Some(Arc::new(resolved));
        assert!(of_item(held.clone(), ITEM).is_some());
        assert!(of_item(held, "ffeeddccbbaa99887766554433221100").is_none());
    }

    /// A subtitle file that did not come is burned in only where the
    /// server burns: in a transcode, once, and for a track it offered as
    /// a file.
    #[test]
    fn a_lost_subtitle_file_is_burned_in_only_in_a_transcode() {
        let info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        let mut resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert!(resolved.burns_in_instead(2));
        assert!(!resolved.burns_in_instead(7), "not offered as a file");
        resolved.burn_in = true;
        assert!(!resolved.burns_in_instead(2), "burned in once already");
        resolved.burn_in = false;
        resolved.play_method = PlayMethod::DirectPlay;
        assert!(!resolved.burns_in_instead(2), "the server burns nothing into the file itself");
    }

    #[test]
    fn errors_of_the_server_are_errors() {
        let info: PlaybackInfo =
            serde_json::from_str(r#"{"MediaSources":[],"ErrorCode":"NoCompatibleStream"}"#).unwrap();
        assert!(resolve(&client(), ITEM, &info, None).is_err());
        let mut info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        info.media_sources[0].supports_transcoding = false;
        assert!(resolve(&client(), ITEM, &info, None).is_err());
    }
}
