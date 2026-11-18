//! The server's socket for the iOS app: the news that makes a page load
//! again, the commands of other devices (this phone as a target), and the
//! sessions the Play on sheet shows. The socket itself is core's
//! `realtime`; the reading of a command is core's `cast::protocol`.

use std::sync::{Arc, Mutex};

use bloom_core::{
    cast::protocol::{self, Action, Local, RemoteCommand},
    news,
    realtime::{self, Realtime, SocketEvent},
};

use crate::{BloomError, PlayCommand, Server, guarded, lock, run, syncplay::SyncSession};

/// The general commands the iOS player carries out. The volume is the
/// system's on a phone, so the volume commands are not here (parity gap 4);
/// the stream commands wait for the server-to-mpv track map.
const IOS_COMMANDS: &[&str] = &["Mute", "Unmute", "ToggleMute", "DisplayMessage"];

/// What came over the socket, in the app's terms.
#[derive(uniffi::Enum)]
pub enum ServerEvent {
    /// The socket is open; `again` when it was open before. Capabilities
    /// and subscriptions go out again on each open.
    Opened { again: bool },
    Closed,
    /// The library (`library`) or only this user's data changed.
    News { library: bool },
    /// A command another device sent to this one; `plan` says what to do.
    Command { request: Arc<RemoteRequest> },
    /// Another device asks this one to play items.
    Play {
        item_ids: Vec<String>,
        play_command: PlayCommand,
        start_secs: Option<f64>,
        start_index: Option<u32>,
    },
    /// The sessions changed (after `subscribe_sessions`).
    Sessions,
}

/// The state of the player a command may depend on.
#[derive(uniffi::Record)]
pub struct PlayerState {
    pub player_open: bool,
    pub paused: bool,
    pub volume: f32,
    pub muted: bool,
}

/// What the player does for a command (core's `protocol::Action`).
#[derive(uniffi::Enum, Debug, PartialEq)]
pub enum RemoteAction {
    TogglePause,
    SeekTo { secs: f64 },
    SeekBy { secs: f64 },
    Next,
    Previous,
    Stop,
    SetVolume { volume: f32 },
    SetMuted { muted: bool },
    ToggleMute,
    SetAudioStream { index: i64 },
    SetSubtitleStream { index: i64 },
    Toast { header: String, text: String },
    ToggleFullscreen,
    Nothing,
}

impl From<Action> for RemoteAction {
    fn from(action: Action) -> Self {
        match action {
            Action::TogglePause => Self::TogglePause,
            Action::SeekTo(secs) => Self::SeekTo { secs },
            Action::SeekBy(secs) => Self::SeekBy { secs },
            Action::Next => Self::Next,
            Action::Previous => Self::Previous,
            Action::Stop => Self::Stop,
            Action::SetVolume(volume) => Self::SetVolume { volume },
            Action::SetMuted(muted) => Self::SetMuted { muted },
            Action::ToggleMute => Self::ToggleMute,
            Action::SetAudioStream(index) => Self::SetAudioStream { index },
            Action::SetSubtitleStream(index) => Self::SetSubtitleStream { index },
            Action::Toast { header, text } => Self::Toast { header, text },
            Action::ToggleFullscreen => Self::ToggleFullscreen,
            Action::Nothing => Self::Nothing,
        }
    }
}

/// A command of another device, as core read it.
#[derive(uniffi::Object)]
pub struct RemoteRequest {
    command: RemoteCommand,
}

#[uniffi::export]
impl RemoteRequest {
    /// What to do with the player in `state`.
    pub fn plan(&self, state: PlayerState) -> RemoteAction {
        let local = Local { player_open: state.player_open, paused: state.paused, volume: state.volume, muted: state.muted };
        guarded("remote plan", RemoteAction::Nothing, || protocol::plan(&self.command, local).into())
    }

    /// For the log.
    pub fn describe(&self) -> String {
        format!("{:?}", self.command)
    }
}

/// The open socket. `next()` until it answers nothing, or `close()`.
#[derive(uniffi::Object)]
pub struct ServerSocket {
    realtime: Mutex<Option<Realtime>>,
    rx: async_channel::Receiver<SocketEvent>,
    /// The SyncPlay session that reads the socket too.
    syncplay: Mutex<Option<Arc<SyncSession>>>,
}

#[uniffi::export]
impl ServerSocket {
    /// The next event the app reads; nil once closed. Messages the app
    /// does not read are skipped here.
    pub async fn next(&self) -> Option<ServerEvent> {
        loop {
            let event = self.rx.recv().await.ok()?;
            let session = lock(&self.syncplay).clone();
            if let Some(session) = session {
                session.socket(&event);
            }
            if let Some(event) = translate(event) {
                return Some(event);
            }
        }
    }

    /// Asks the server to push the sessions (for the Play on sheet), every
    /// 1.5 s at most. False while the socket is down.
    pub fn subscribe_sessions(&self, on: bool) -> bool {
        let held = lock(&self.realtime);
        let Some(realtime) = held.as_ref() else { return false };
        if on {
            realtime.send("SessionsStart", Some(serde_json::Value::String("0,1500".into())));
        } else {
            realtime.send("SessionsStop", None);
        }
        true
    }

    /// The SyncPlay session reads every event of the socket from now on
    /// (nil detaches it).
    pub fn attach_syncplay(&self, session: Option<Arc<SyncSession>>) {
        *lock(&self.syncplay) = session;
    }

    /// Sends a message to the server (a SyncPlay subscription, a ping).
    /// False when the socket is closed or `data_json` is not JSON (nothing
    /// is sent then).
    pub fn send(&self, kind: String, data_json: Option<String>) -> bool {
        let data = match data_json.map(|json| serde_json::from_str(&json)) {
            Some(Ok(data)) => Some(data),
            Some(Err(err)) => {
                log::warn!("socket: {kind} not sent, its data is not JSON: {err}");
                return false;
            }
            None => None,
        };
        let held = lock(&self.realtime);
        let Some(realtime) = held.as_ref() else { return false };
        realtime.send(&kind, data);
        true
    }

    pub fn close(&self) {
        lock(&self.syncplay).take();
        lock(&self.realtime).take();
        self.rx.close();
    }
}

fn translate(event: SocketEvent) -> Option<ServerEvent> {
    match event {
        SocketEvent::Open { again } => Some(ServerEvent::Opened { again }),
        SocketEvent::Closed => Some(ServerEvent::Closed),
        SocketEvent::Message { kind, data } => {
            if let Some(library) = news::kind_of(&kind) {
                return Some(ServerEvent::News { library });
            }
            if kind == "Sessions" {
                return Some(ServerEvent::Sessions);
            }
            match protocol::parse(&kind, &data)? {
                RemoteCommand::Play { item_ids, command, start_secs, start_index, .. } => Some(ServerEvent::Play {
                    item_ids,
                    play_command: command.into(),
                    start_secs,
                    start_index: start_index.and_then(|n| u32::try_from(n).ok()),
                }),
                command => Some(ServerEvent::Command { request: Arc::new(RemoteRequest { command }) }),
            }
        }
    }
}

/// How long the news waits before the page loads again, `since_first_ms`
/// after the first message no reload has handled (core's `news::delay`:
/// 3 s of quiet, 15 s at most).
#[uniffi::export]
pub fn news_delay_ms(since_first_ms: u64) -> u64 {
    news::delay(std::time::Duration::from_millis(since_first_ms)).as_millis() as u64
}

#[uniffi::export]
impl Server {
    /// Opens the server's socket for this session; it reconnects by itself
    /// until closed.
    pub fn socket(&self) -> Result<Arc<ServerSocket>, BloomError> {
        let client = self.signed_in()?;
        let (realtime, rx) = realtime::start(client);
        Ok(Arc::new(ServerSocket { realtime: Mutex::new(Some(realtime)), rx, syncplay: Mutex::new(None) }))
    }

    /// Tells the server what this session can do, so other devices list it
    /// as a target (`control`) or not.
    pub async fn post_capabilities(&self, control: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let mut body = protocol::capabilities(control);
            if control {
                body["SupportedCommands"] = serde_json::json!(IOS_COMMANDS);
            }
            client.post_capabilities(&body).map_err(Into::into)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_news_commands_and_plays() {
        let message = |kind: &str, data| translate(SocketEvent::Message { kind: kind.into(), data });
        assert!(matches!(message("LibraryChanged", json!({})), Some(ServerEvent::News { library: true })));
        assert!(matches!(message("UserDataChanged", json!({})), Some(ServerEvent::News { library: false })));
        assert!(matches!(message("Sessions", json!([])), Some(ServerEvent::Sessions)));
        assert!(message("ScheduledTasksInfo", json!({})).is_none());
        let play = message("Play", json!({ "ItemIds": ["a", "b"], "PlayCommand": "PlayNow", "StartPositionTicks": 50_000_000 }));
        let Some(ServerEvent::Play { item_ids, play_command, start_secs, .. }) = play else { panic!("not a play") };
        assert_eq!(item_ids, ["a", "b"]);
        assert_eq!(play_command, PlayCommand::PlayNow);
        assert_eq!(start_secs, Some(5.));
        let Some(ServerEvent::Command { request }) = message("Playstate", json!({ "Command": "Pause" })) else {
            panic!("not a command")
        };
        let state = |paused| PlayerState { player_open: true, paused, volume: 100., muted: false };
        assert_eq!(request.plan(state(false)), RemoteAction::TogglePause);
        assert_eq!(request.plan(state(true)), RemoteAction::Nothing);
    }

    #[test]
    fn the_news_waits_for_quiet_and_not_forever() {
        assert_eq!(news_delay_ms(0), 3_000);
        assert_eq!(news_delay_ms(14_000), 1_000);
        assert_eq!(news_delay_ms(20_000), 0);
    }
}
