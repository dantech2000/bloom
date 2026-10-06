// SPDX-License-Identifier: AGPL-3.0-or-later
//! One connection to one cast device. A thread holds the TLS socket open,
//! answers the heartbeat, matches answers to requests by their id, and
//! connects again when the socket is lost. The app talks to it through
//! [`Session`] and hears from it on a channel, the way the socket of the
//! server (`realtime.rs`) and the player do.
//!
//! The devices use a certificate of their own, signed by nobody the system
//! knows. The connection takes the certificate the device shows, and from
//! then on only that one: a connect-again with another certificate fails.
//!
//! A command does not open a connection: the socket stays open, so a pause
//! or a seek is one message on a warm connection. Seeks are coalesced: a
//! seek sent while one waits for its answer is held, and only the last one
//! held goes out when the answer comes.

use std::{
    collections::HashMap,
    io::{ErrorKind, Write},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use rustls::{
    ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde_json::Value;

use super::{
    jellyfin::{self, Identity, ItemStub},
    mdns::Device,
    messages::{self, App, LoadMedia, MediaStatus, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, RECEIVER, ReceiverStatus, SENDER},
    proto::{CastMessage, Inbox},
};

/// The times of the connection; a test makes them short.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Time between two PINGs of ours.
    pub ping: Duration,
    /// With no word of the device for this long the socket counts as dead.
    pub silence: Duration,
    /// How long one read waits; the loop looks at its work between reads.
    pub read_wait: Duration,
    /// An answer that does not come in this time is given up.
    pub answer: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(5),
            silence: Duration::from_secs(15),
            read_wait: Duration::from_millis(100),
            answer: Duration::from_secs(10),
        }
    }
}

/// What the thread tells the app, at the moment it happens.
#[derive(Clone, Debug)]
pub enum Event {
    /// The socket is open; `again` when it was open before in this run.
    Connected { again: bool },
    /// Something in the status changed.
    Status(Status),
    /// A message of the receiver app, such as a playback report of the
    /// Jellyfin app, or one of a namespace this module does not know.
    Message { namespace: String, payload: Value },
    Error(String),
    Closed,
}

/// The picture of the device, as of the last word of it.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub connected: bool,
    /// The app that runs on the device and that we are connected to.
    pub app: Option<App>,
    /// 0 to 1.
    pub volume: f64,
    pub muted: bool,
    /// IDLE, BUFFERING, PLAYING, PAUSED; empty with nothing loaded.
    pub player_state: String,
    /// Seconds into the item, at `position_at`.
    pub position: f64,
    pub position_at: Option<Instant>,
    pub duration: Option<f64>,
    pub idle_reason: Option<String>,
    pub media_session_id: Option<i64>,
    pub content_id: Option<String>,
    /// The Jellyfin item that plays, from the report of the Jellyfin app.
    pub item_id: Option<String>,
    pub active_tracks: Vec<i64>,
    /// Round trip of the last answered request.
    pub rtt_ms: Option<f64>,
    /// App id to APP_AVAILABLE or APP_UNAVAILABLE, from the last question.
    pub availability: HashMap<String, String>,
    /// The last error the device sent.
    pub error: Option<String>,
}

impl Status {
    /// The position now: the last one, plus the time since when playing.
    pub fn position_now(&self) -> f64 {
        match self.position_at {
            Some(at) if self.player_state == "PLAYING" => self.position + at.elapsed().as_secs_f64(),
            _ => self.position,
        }
    }

    pub fn playing_jellyfin_app(&self) -> bool {
        self.app.as_ref().is_some_and(|app| jellyfin::is_jellyfin_app(&app.app_id))
    }
}

/// What to play. The Jellyfin receiver app is tried first when `jellyfin`
/// is given; the Default Media Receiver with `media` when that app does
/// not launch, or when only `media` is given.
#[derive(Clone, Debug, Default)]
pub struct LoadRequest {
    pub jellyfin: Option<JellyfinLoad>,
    pub media: Option<LoadMedia>,
}

#[derive(Clone, Debug)]
pub struct JellyfinLoad {
    pub identity: Identity,
    pub items: Vec<ItemStub>,
    pub start_secs: f64,
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
}

type Reply = mpsc::Sender<Value>;

#[derive(Clone)]
enum Command {
    GetStatus(Option<Reply>),
    Availability(Vec<String>, Option<Reply>),
    Launch(Vec<String>),
    /// Connects to the app that runs, whatever it is.
    Join,
    StopApp,
    SetVolume(f64),
    SetMuted(bool),
    Load(LoadRequest),
    Play,
    Pause,
    Seek(f64),
    StopMedia,
    MediaStatus,
    /// An audio stream index of Jellyfin.
    SetAudio(i64),
    /// A subtitle stream index of Jellyfin, or a track id of the media
    /// receiver; `None` turns subtitles off.
    SetSubtitle(Option<i64>),
    /// Any message, to the app when one is connected, else to the device.
    Raw { namespace: String, payload: Value, reply: Option<Reply> },
    Close,
}

struct Shared {
    status: Mutex<Status>,
    stop: AtomicBool,
}

/// The handle of the app. Dropping it closes the connection.
pub struct Session {
    device: Device,
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let _ = self.commands.send(Command::Close);
    }
}

impl Session {
    /// Opens the connection on its own thread. The events come on the
    /// returned channel.
    pub fn connect(device: Device) -> (Self, async_channel::Receiver<Event>) {
        Self::connect_with(device, Timing::default())
    }

    pub fn connect_with(device: Device, timing: Timing) -> (Self, async_channel::Receiver<Event>) {
        let (tx, rx) = async_channel::unbounded();
        let (commands, inbox) = mpsc::channel();
        let shared = Arc::new(Shared { status: Mutex::new(Status::default()), stop: AtomicBool::new(false) });
        let worker = Worker::new(device.clone(), timing, tx, shared.clone(), inbox);
        let _ = thread::Builder::new()
            .name(format!("cast-{}", device.name))
            .spawn(move || worker.run());
        (Self { device, commands, shared }, rx)
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Asks the device for its status; the answer comes on the channel as
    /// well as into the status.
    pub fn get_status(&self) -> mpsc::Receiver<Value> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::GetStatus(Some(reply)));
        answer
    }

    pub fn app_availability(&self, app_ids: Vec<String>) -> mpsc::Receiver<Value> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Availability(app_ids, Some(reply)));
        answer
    }

    /// Launches the first of the apps that the device has.
    pub fn launch(&self, app_ids: Vec<String>) {
        self.send(Command::Launch(app_ids));
    }

    pub fn join(&self) {
        self.send(Command::Join);
    }

    /// Stops the app that runs; the device shows its idle screen.
    pub fn stop_app(&self) {
        self.send(Command::StopApp);
    }

    /// `level` from 0 to 1.
    pub fn set_volume(&self, level: f64) {
        self.send(Command::SetVolume(level));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted(muted));
    }

    /// Launches the right app when it does not run, then loads.
    pub fn load(&self, request: LoadRequest) {
        self.send(Command::Load(request));
    }

    pub fn play(&self) {
        self.send(Command::Play);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    /// The last seek wins: one sent while another waits replaces it.
    pub fn seek(&self, secs: f64) {
        self.send(Command::Seek(secs));
    }

    pub fn stop_media(&self) {
        self.send(Command::StopMedia);
    }

    pub fn media_status(&self) {
        self.send(Command::MediaStatus);
    }

    pub fn set_subtitle(&self, track: Option<i64>) {
        self.send(Command::SetSubtitle(track));
    }

    pub fn set_audio(&self, index: i64) {
        self.send(Command::SetAudio(index));
    }

    /// Sends any message and gives back the answer with the same request
    /// id, for tests and the debug channel.
    pub fn request(&self, namespace: &str, payload: Value) -> mpsc::Receiver<Value> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Raw { namespace: namespace.into(), payload, reply: Some(reply) });
        answer
    }
}

// ----- the thread ------------------------------------------------------------

enum Waiting {
    Reply(Reply),
    /// A LAUNCH; the apps still to try when it fails.
    Launch { rest: Vec<String> },
    Seek,
    Quiet,
}

struct Pending {
    sent: Instant,
    waiting: Waiting,
}

type Stream = StreamOwned<ClientConnection, TcpStream>;

struct Worker {
    device: Device,
    timing: Timing,
    tx: async_channel::Sender<Event>,
    shared: Arc<Shared>,
    commands: mpsc::Receiver<Command>,
    verifier: Arc<DeviceCertificate>,
    status: Status,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// The transport of the app we are connected to.
    transport: Option<String>,
    /// The session id of that app, to join it again after a reconnect.
    our_session: Option<String>,
    /// What the Jellyfin app is told on every message, once known.
    identity: Option<Identity>,
    /// Waits for the app to launch.
    pending_load: Option<LoadRequest>,
    /// The next receiver status joins the app that runs.
    pending_join: bool,
    /// A seek waits for its answer; the one to send after it.
    seek_in_flight: Option<Instant>,
    seek_queued: Option<f64>,
    /// The commands that came while the socket was down, or after a
    /// connect before the device said what it runs, in order; of the
    /// controls only the latest of a kind (see `hold`). They go out one at
    /// a time once the app and its media session are known again
    /// (`replay_step`); a socket that dies meanwhile keeps the ones not
    /// yet sent for the next one.
    held: Vec<Command>,
    replay: Replay,
    /// When the status the held commands wait for was asked, and how
    /// often; after `REPLAY_ASKS` without an answer they are given up.
    replay_asked: Instant,
    replay_asks: u32,
    /// The held command last sent, with its request; the next one waits
    /// for its answer, so a socket that dies in between takes nothing with
    /// it that was not written. A control that was written and not
    /// answered when the socket died goes again over the next one (`run`).
    replay_wait: Option<(u64, Command)>,
}

/// Where the held commands wait after a connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Replay {
    /// Nothing waits, or everything went out.
    Done,
    /// For the receiver status: which app runs, if any.
    ReceiverStatus,
    /// The app is joined; for its media status: the media session.
    MediaStatus,
    /// The device said what it runs; the held commands go out in turn.
    Sending,
}

/// How often the status the held commands wait for is asked before they
/// are given up: a device that answers the heartbeat but not a status.
const REPLAY_ASKS: u32 = 3;

impl Worker {
    fn new(
        device: Device,
        timing: Timing,
        tx: async_channel::Sender<Event>,
        shared: Arc<Shared>,
        commands: mpsc::Receiver<Command>,
    ) -> Self {
        Self {
            device,
            timing,
            tx,
            shared,
            commands,
            verifier: Arc::new(DeviceCertificate::default()),
            status: Status::default(),
            next_id: 1,
            pending: HashMap::new(),
            transport: None,
            our_session: None,
            identity: None,
            pending_load: None,
            pending_join: false,
            seek_in_flight: None,
            seek_queued: None,
            held: Vec::new(),
            replay: Replay::Done,
            replay_asked: Instant::now(),
            replay_asks: 0,
            replay_wait: None,
        }
    }

    fn stopped(&self) -> bool {
        self.shared.stop.load(Ordering::Acquire)
    }

    fn run(mut self) {
        let (mut again, mut wait) = (false, Duration::from_secs(1));
        while !self.stopped() {
            match self.connect() {
                Ok(mut stream) => {
                    wait = Duration::from_secs(1);
                    match self.serve(&mut stream, again) {
                        Ok(()) => {
                            let _ = stream.write_all(&self.frame(RECEIVER, NS_CONNECTION, messages::close()).frame());
                            return;
                        }
                        Err(err) => log::info!("cast {}: connection lost: {err:#}", self.device.name),
                    }
                    again = true;
                    self.status.connected = false;
                    // A held control written to the socket that died, with
                    // no answer: nobody knows whether the device got it.
                    // It goes again (to pause twice is to pause), unless a
                    // newer one of its kind waits. A load or a launch does
                    // not: twice is not the same as once.
                    if let Some((id, command)) = self.replay_wait.take()
                        && self.pending.contains_key(&id)
                        && same_kind(&command, &command)
                        && !self.held.iter().any(|held| same_kind(held, &command))
                    {
                        self.held.insert(0, command);
                    }
                    self.pending.clear();
                    self.seek_in_flight = None;
                    // The new socket must connect to the app again; the
                    // status that comes first says whether it still runs.
                    self.transport = None;
                    self.publish();
                    self.emit(Event::Closed);
                }
                Err(err) => {
                    log::warn!("cast {}: {err:#}", self.device.name);
                    self.emit(Event::Error(format!("{err:#}")));
                }
            }
            // Wait in small steps, so a drop ends the thread soon.
            let until = Instant::now() + wait;
            while Instant::now() < until && !self.stopped() {
                thread::sleep(Duration::from_millis(50));
                while let Ok(command) = self.commands.try_recv() {
                    if matches!(command, Command::Close) {
                        return;
                    }
                    self.hold(command);
                }
            }
            wait = (wait * 2).min(Duration::from_secs(15));
        }
    }

    fn connect(&self) -> Result<Stream> {
        let address = (self.device.address, self.device.port);
        let tcp = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5)).context("connect")?;
        tcp.set_nodelay(true).ok();
        tcp.set_read_timeout(Some(Duration::from_secs(5)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(5)))?;
        let config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(self.verifier.clone())
            .with_no_client_auth();
        let name = ServerName::IpAddress(std::net::IpAddr::V4(self.device.address).into());
        let connection = ClientConnection::new(Arc::new(config), name)?;
        let mut stream = StreamOwned::new(connection, tcp);
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).context("TLS handshake")?;
        }
        stream.sock.set_read_timeout(Some(self.timing.read_wait))?;
        Ok(stream)
    }

    /// `Ok` when the session was closed on purpose.
    fn serve(&mut self, stream: &mut Stream, again: bool) -> Result<()> {
        let mut inbox = Inbox::default();
        self.send(stream, RECEIVER, NS_CONNECTION, messages::connect())?;
        self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(0), Waiting::Quiet)?;
        self.status.connected = true;
        self.status.error = None;
        self.emit(Event::Connected { again });
        self.publish();
        // The app transport of before: ask again, and connect to it when
        // the app still runs (see `on_receiver_status`). The commands held
        // meanwhile need that transport, and the media session after it.
        let (mut last_ping, mut last_heard) = (Instant::now(), Instant::now());
        self.replay = if self.held.is_empty() { Replay::Done } else { Replay::ReceiverStatus };
        self.replay_asked = Instant::now();
        self.replay_asks = 1;
        self.replay_wait = None;
        loop {
            match inbox.fill(stream) {
                Ok(true) => {}
                Ok(false) => return Err(anyhow!("the device closed the socket")),
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(err) => return Err(err.into()),
            }
            while let Some(message) = inbox.next()? {
                last_heard = Instant::now();
                self.receive(stream, message)?;
            }
            let mut batch = Vec::new();
            while let Ok(command) = self.commands.try_recv() {
                batch.push(command);
            }
            if self.replay != Replay::Done {
                // Behind the ones that wait, so the order holds.
                for command in batch {
                    self.hold(command);
                }
            } else if !self.batch(stream, batch)? {
                return Ok(());
            }
            if self.stopped() {
                return Ok(());
            }
            if last_ping.elapsed() >= self.timing.ping {
                self.send(stream, RECEIVER, NS_HEARTBEAT, messages::ping())?;
                last_ping = Instant::now();
            }
            if last_heard.elapsed() >= self.timing.silence {
                return Err(anyhow!("no word of the device for {:?}", self.timing.silence));
            }
            self.expire(stream)?;
            if !self.replay_step(stream)? {
                return Ok(());
            }
        }
    }

    /// Runs a batch of commands. Of several seeks only the last matters.
    /// False when the batch closes the session.
    fn batch(&mut self, stream: &mut Stream, batch: Vec<Command>) -> Result<bool> {
        let last_seek = batch.iter().rposition(|c| matches!(c, Command::Seek(_)));
        for (n, command) in batch.into_iter().enumerate() {
            if matches!(command, Command::Seek(_)) && last_seek != Some(n) {
                continue;
            }
            if matches!(command, Command::Close) {
                return Ok(false);
            }
            self.command(stream, command)?;
        }
        Ok(true)
    }

    /// Keeps a command for when the connection can carry it. Of the
    /// controls the latest of a kind is what the user wants: a seek takes
    /// the place of the seek before it, a pause of a play, a volume of a
    /// volume. The rest keeps its order.
    fn hold(&mut self, command: Command) {
        self.held.retain(|held| !same_kind(held, &command));
        self.held.push(command);
    }

    /// Moves the held commands on: asks the status again when it does not
    /// come, gives them up with a word when it never does, and sends them
    /// one at a time once the device said what it runs. A command that
    /// needs an app or a media session that is gone fails for itself, as
    /// it does on a live connection. False when a held close ends the
    /// session.
    fn replay_step(&mut self, stream: &mut Stream) -> Result<bool> {
        match self.replay {
            Replay::Done => {}
            Replay::ReceiverStatus | Replay::MediaStatus => {
                if self.replay_asked.elapsed() <= self.timing.answer {
                    return Ok(true);
                }
                if self.replay_asks >= REPLAY_ASKS {
                    let dropped = std::mem::take(&mut self.held).len();
                    self.replay = Replay::Done;
                    self.fail(format!("the device did not say what it runs; {dropped} commands dropped"));
                    return Ok(true);
                }
                self.replay_asks += 1;
                self.replay_asked = Instant::now();
                log::info!("cast {}: no status yet; asking again", self.device.name);
                match self.app_transport().filter(|_| self.replay == Replay::MediaStatus) {
                    Some(transport) => {
                        self.request(stream, &transport, NS_MEDIA, messages::media_status(0), Waiting::Quiet)?;
                    }
                    None => {
                        self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(0), Waiting::Quiet)?;
                    }
                }
            }
            Replay::Sending => {
                if self.replay_wait.as_ref().is_some_and(|(id, _)| self.pending.contains_key(id)) {
                    return Ok(true);
                }
                self.replay_wait = None;
                if self.held.is_empty() {
                    self.replay = Replay::Done;
                    return Ok(true);
                }
                let command = self.held.remove(0);
                if matches!(command, Command::Close) {
                    return Ok(false);
                }
                let before = self.next_id;
                let again = command.clone();
                if let Err(err) = self.command(stream, command) {
                    // Not written: it goes over the next socket.
                    self.held.insert(0, again);
                    return Err(err);
                }
                if self.next_id > before {
                    self.replay_wait = Some((self.next_id - 1, again));
                }
            }
        }
        Ok(true)
    }

    /// Gives up answers that do not come: a seek that waited lets the
    /// next one go; a launch tries the next app, or fails the load.
    fn expire(&mut self, stream: &mut Stream) -> Result<()> {
        let answer = self.timing.answer;
        let late: Vec<u64> = self.pending.iter().filter(|(_, p)| p.sent.elapsed() > answer).map(|(id, _)| *id).collect();
        for id in late {
            match self.pending.remove(&id).map(|p| p.waiting) {
                Some(Waiting::Seek) => self.seek_in_flight = None,
                Some(Waiting::Launch { rest }) if !rest.is_empty() => {
                    log::info!("cast {}: no answer to the launch; next app", self.device.name);
                    self.launch(stream, rest)?;
                }
                Some(Waiting::Launch { .. }) => {
                    self.pending_load = None;
                    self.fail("the device did not answer the launch");
                }
                Some(Waiting::Reply(_) | Waiting::Quiet) | None => {}
            }
        }
        // The Jellyfin app answers a seek with a report that carries no
        // request id: count a seek as answered after a short time.
        if self.seek_in_flight.is_some_and(|at| at.elapsed() > Duration::from_millis(250))
            && !self.pending.values().any(|p| matches!(p.waiting, Waiting::Seek))
        {
            self.seek_in_flight = None;
        }
        if self.seek_in_flight.is_none()
            && let Some(secs) = self.seek_queued.take()
        {
            self.seek(stream, secs)?;
        }
        Ok(())
    }

    fn emit(&self, event: Event) {
        let _ = self.tx.send_blocking(event);
    }

    /// Copies the status for the app and tells it.
    fn publish(&self) {
        *self.shared.status.lock().unwrap() = self.status.clone();
        self.emit(Event::Status(self.status.clone()));
    }

    fn frame(&self, destination: &str, namespace: &str, payload: Value) -> CastMessage {
        CastMessage::text(SENDER, destination, namespace, payload.to_string())
    }

    fn send(&mut self, stream: &mut Stream, destination: &str, namespace: &str, payload: Value) -> Result<()> {
        log::trace!("cast > {destination} {namespace} {payload}");
        stream.write_all(&self.frame(destination, namespace, payload).frame()).context("send")?;
        Ok(())
    }

    /// Sends with a request id and remembers what waits for the answer.
    fn request(
        &mut self,
        stream: &mut Stream,
        destination: &str,
        namespace: &str,
        mut payload: Value,
        waiting: Waiting,
    ) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        payload["requestId"] = Value::from(id);
        self.pending.insert(id, Pending { sent: Instant::now(), waiting });
        self.send(stream, destination, namespace, payload)?;
        Ok(id)
    }

    /// The destination of a media command: the app, or nobody.
    fn app_transport(&self) -> Option<String> {
        self.transport.clone()
    }

    fn fail(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::warn!("cast {}: {text}", self.device.name);
        self.status.error = Some(text.clone());
        self.publish();
        self.emit(Event::Error(text));
    }

    fn command(&mut self, stream: &mut Stream, command: Command) -> Result<()> {
        match command {
            Command::GetStatus(reply) => {
                let waiting = reply.map_or(Waiting::Quiet, Waiting::Reply);
                self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(0), waiting)?;
            }
            Command::Availability(ids, reply) => {
                let waiting = reply.map_or(Waiting::Quiet, Waiting::Reply);
                self.request(stream, RECEIVER, NS_RECEIVER, messages::app_availability(0, &ids), waiting)?;
            }
            Command::Launch(ids) => self.launch(stream, ids)?,
            Command::Join => {
                self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(0), Waiting::Quiet)?;
                self.pending_join = true;
            }
            Command::StopApp => match self.status.app.clone() {
                Some(app) => {
                    self.request(stream, RECEIVER, NS_RECEIVER, messages::stop_app(0, &app.session_id), Waiting::Quiet)?;
                }
                None => self.fail("no app runs"),
            },
            Command::SetVolume(level) => {
                self.request(stream, RECEIVER, NS_RECEIVER, messages::set_volume(0, level), Waiting::Quiet)?;
            }
            Command::SetMuted(muted) => {
                self.request(stream, RECEIVER, NS_RECEIVER, messages::set_muted(0, muted), Waiting::Quiet)?;
            }
            Command::Load(request) => self.load(stream, request)?,
            Command::Play => self.media_command(stream, jellyfin::unpause, |id| messages::play(0, id))?,
            Command::Pause => self.media_command(stream, jellyfin::pause, |id| messages::pause(0, id))?,
            Command::StopMedia => self.media_command(stream, jellyfin::stop, |id| messages::media_stop(0, id))?,
            Command::Seek(secs) => {
                if self.seek_in_flight.is_some() {
                    self.seek_queued = Some(secs);
                } else {
                    self.seek(stream, secs)?;
                }
            }
            Command::MediaStatus => {
                if let Some(transport) = self.app_transport() {
                    self.request(stream, &transport, NS_MEDIA, messages::media_status(0), Waiting::Quiet)?;
                }
            }
            Command::SetAudio(index) => {
                let Some(transport) = self.app_transport() else {
                    self.fail("no app runs");
                    return Ok(());
                };
                match self.identity.clone().filter(|_| self.status.playing_jellyfin_app()) {
                    Some(identity) => {
                        let payload = jellyfin::set_audio(&identity, &self.device.name, index);
                        self.send(stream, &transport, jellyfin::NAMESPACE, payload)?;
                    }
                    None => self.fail("only the Jellyfin app can change the audio stream"),
                }
            }
            Command::SetSubtitle(track) => {
                let Some(transport) = self.app_transport() else {
                    self.fail("no app runs");
                    return Ok(());
                };
                if self.status.playing_jellyfin_app() {
                    if let Some(identity) = self.identity.clone() {
                        let payload = jellyfin::set_subtitle(&identity, &self.device.name, track.unwrap_or(-1));
                        self.send(stream, &transport, jellyfin::NAMESPACE, payload)?;
                    }
                } else if let Some(session) = self.status.media_session_id {
                    let active: Vec<i64> = track.into_iter().collect();
                    self.request(stream, &transport, NS_MEDIA, messages::edit_tracks(0, session, &active), Waiting::Quiet)?;
                } else {
                    self.fail("nothing is loaded");
                }
            }
            Command::Raw { namespace, payload, reply } => {
                let destination = self.app_transport().unwrap_or_else(|| RECEIVER.to_string());
                let waiting = reply.map_or(Waiting::Quiet, Waiting::Reply);
                self.request(stream, &destination, &namespace, payload, waiting)?;
            }
            Command::Close => {}
        }
        Ok(())
    }

    fn launch(&mut self, stream: &mut Stream, mut ids: Vec<String>) -> Result<()> {
        if ids.is_empty() {
            self.fail("no app to launch");
            return Ok(());
        }
        let first = ids.remove(0);
        log::info!("cast {}: launch {first}", self.device.name);
        self.request(stream, RECEIVER, NS_RECEIVER, messages::launch(0, &first), Waiting::Launch { rest: ids })?;
        Ok(())
    }

    /// Loads now when the right app runs, else launches it first.
    fn load(&mut self, stream: &mut Stream, request: LoadRequest) -> Result<()> {
        let app = self.status.app.clone().filter(|_| self.transport.is_some());
        let jellyfin_runs = app.as_ref().is_some_and(|app| jellyfin::is_jellyfin_app(&app.app_id));
        let default_runs = app.as_ref().is_some_and(|app| app.app_id == messages::APP_DEFAULT_MEDIA);
        if jellyfin_runs && request.jellyfin.is_some() || default_runs && request.media.is_some() {
            return self.load_now(stream, request);
        }
        let mut ids = Vec::new();
        if request.jellyfin.is_some() {
            ids.push(jellyfin::APP_STABLE.to_string());
        }
        if request.media.is_some() {
            ids.push(messages::APP_DEFAULT_MEDIA.to_string());
        }
        self.pending_load = Some(request);
        self.launch(stream, ids)
    }

    /// Sends the load to the app that runs.
    fn load_now(&mut self, stream: &mut Stream, request: LoadRequest) -> Result<()> {
        let Some(transport) = self.app_transport() else {
            self.fail("no app runs");
            return Ok(());
        };
        if self.status.playing_jellyfin_app() {
            let Some(load) = request.jellyfin else {
                self.fail("the Jellyfin app runs, and the item has no Jellyfin form");
                return Ok(());
            };
            self.identity = Some(load.identity.clone());
            let payload = jellyfin::play_now(
                &load.identity,
                &self.device.name,
                &load.items,
                load.start_secs,
                load.audio_index,
                load.subtitle_index,
            );
            self.send(stream, &transport, jellyfin::NAMESPACE, payload)?;
            self.status.item_id = load.items.first().map(|item| item.id.clone());
            self.status.player_state = "BUFFERING".into();
            self.publish();
        } else {
            let Some(media) = request.media else {
                self.fail("the media receiver runs, and the item has no stream URL");
                return Ok(());
            };
            self.request(stream, &transport, NS_MEDIA, messages::load(0, &media), Waiting::Quiet)?;
            self.status.content_id = Some(media.url.clone());
            self.status.player_state = "BUFFERING".into();
            self.publish();
        }
        Ok(())
    }

    /// Play, pause and stop: the Jellyfin app takes its own words, the
    /// media receiver the media namespace with the media session.
    fn media_command(
        &mut self,
        stream: &mut Stream,
        jellyfin_command: impl FnOnce(&Identity, &str) -> Value,
        media: impl FnOnce(i64) -> Value,
    ) -> Result<()> {
        let Some(transport) = self.app_transport() else {
            self.fail("no app runs");
            return Ok(());
        };
        if self.status.playing_jellyfin_app() {
            let Some(identity) = self.identity.clone() else {
                self.fail("the Jellyfin app was started by another sender; load an item first");
                return Ok(());
            };
            let payload = jellyfin_command(&identity, &self.device.name);
            self.send(stream, &transport, jellyfin::NAMESPACE, payload)?;
        } else if let Some(session) = self.status.media_session_id {
            self.request(stream, &transport, NS_MEDIA, media(session), Waiting::Quiet)?;
        } else {
            self.fail("nothing is loaded");
        }
        Ok(())
    }

    fn seek(&mut self, stream: &mut Stream, secs: f64) -> Result<()> {
        let Some(transport) = self.app_transport() else {
            self.fail("no app runs");
            return Ok(());
        };
        if self.status.playing_jellyfin_app() {
            let Some(identity) = self.identity.clone() else {
                self.fail("the Jellyfin app was started by another sender; load an item first");
                return Ok(());
            };
            self.send(stream, &transport, jellyfin::NAMESPACE, jellyfin::seek(&identity, &self.device.name, secs))?;
        } else if let Some(session) = self.status.media_session_id {
            self.request(stream, &transport, NS_MEDIA, messages::seek(0, session, secs), Waiting::Seek)?;
        } else {
            self.fail("nothing is loaded");
            return Ok(());
        }
        self.seek_in_flight = Some(Instant::now());
        // The position moves at once, so the app shows the seek before
        // the device reports it.
        self.status.position = secs.max(0.);
        self.status.position_at = Some(Instant::now());
        self.publish();
        Ok(())
    }

    fn receive(&mut self, stream: &mut Stream, message: CastMessage) -> Result<()> {
        let payload: Value = match serde_json::from_str(message.text_payload()) {
            Ok(payload) => payload,
            Err(_) => {
                log::debug!("cast {}: not JSON on {}", self.device.name, message.namespace);
                return Ok(());
            }
        };
        log::trace!("cast < {} {} {payload}", message.source, message.namespace);
        // The answer to a request of ours.
        let pending = messages::request_id(&payload).and_then(|id| self.pending.remove(&id));
        if let Some(pending) = &pending {
            self.status.rtt_ms = Some(pending.sent.elapsed().as_secs_f64() * 1000.);
        }
        if let Some(text) = messages::error_text(&payload) {
            match pending {
                Some(Pending { waiting: Waiting::Launch { rest }, .. }) if !rest.is_empty() => {
                    log::info!("cast {}: {text}; next app", self.device.name);
                    return self.launch(stream, rest);
                }
                Some(Pending { waiting: Waiting::Reply(reply), .. }) => {
                    let _ = reply.send(payload);
                }
                Some(Pending { waiting: Waiting::Seek, .. }) => self.seek_in_flight = None,
                _ => {}
            }
            self.pending_load = None;
            self.fail(text);
            return Ok(());
        }
        match message.namespace.as_str() {
            NS_HEARTBEAT => {
                if payload["type"] == "PING" {
                    self.send(stream, &message.source, NS_HEARTBEAT, messages::pong())?;
                }
            }
            NS_CONNECTION => {
                // The app went away.
                if payload["type"] == "CLOSE" && self.transport.as_deref() == Some(&message.source) {
                    self.app_gone();
                }
            }
            NS_RECEIVER => {
                if let Some(status) = ReceiverStatus::parse(&payload) {
                    self.on_receiver_status(stream, status, pending.as_ref().map(|p| &p.waiting))?;
                } else if payload["responseType"] == "GET_APP_AVAILABILITY" {
                    if let Some(map) = payload["availability"].as_object() {
                        for (id, state) in map {
                            self.status.availability.insert(id.clone(), state.as_str().unwrap_or("").to_string());
                        }
                    }
                    self.publish();
                }
            }
            NS_MEDIA => {
                if let Some(entries) = MediaStatus::parse(&payload) {
                    self.on_media_status(entries);
                    // The media session is known, or there is none: either
                    // way the held commands know what to do.
                    if self.replay == Replay::MediaStatus {
                        self.replay = Replay::Sending;
                    }
                }
            }
            jellyfin::NAMESPACE => {
                if let Some(report) = jellyfin::parse_report(&payload) {
                    self.status.position = report.position;
                    self.status.position_at = Some(Instant::now());
                    self.status.player_state = match report.kind.as_str() {
                        "playbackstop" => "IDLE",
                        _ if report.paused => "PAUSED",
                        _ => "PLAYING",
                    }
                    .into();
                    if report.kind == "playbackstop" {
                        self.status.idle_reason = Some("FINISHED".into());
                    } else {
                        self.status.idle_reason = None;
                    }
                    if report.duration.is_some() {
                        self.status.duration = report.duration;
                    }
                    if report.item_id.is_some() {
                        self.status.item_id = report.item_id.clone();
                    }
                    self.publish();
                }
                self.emit(Event::Message { namespace: message.namespace, payload: payload.clone() });
            }
            _ => self.emit(Event::Message { namespace: message.namespace, payload: payload.clone() }),
        }
        match pending {
            Some(Pending { waiting: Waiting::Reply(reply), .. }) => {
                let _ = reply.send(payload);
            }
            Some(Pending { waiting: Waiting::Seek, .. }) => {
                self.seek_in_flight = None;
                if let Some(secs) = self.seek_queued.take() {
                    self.seek(stream, secs)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_receiver_status(&mut self, stream: &mut Stream, status: ReceiverStatus, waiting: Option<&Waiting>) -> Result<()> {
        if let Some(volume) = status.volume {
            self.status.volume = volume.level;
            self.status.muted = volume.muted;
        }
        let running = status.running().cloned();
        let launched = matches!(waiting, Some(Waiting::Launch { .. }));
        let join = std::mem::take(&mut self.pending_join);
        match running {
            Some(app) if self.transport.as_deref() == Some(&app.transport_id) => {
                // Still the app we talk to.
                self.status.app = Some(app);
            }
            Some(app) => {
                // Our launch, a join, the app we were in before the
                // socket was lost, or the Jellyfin app that another
                // sender started: connect to its transport. Any other
                // app is left alone.
                let ours = launched
                    || join
                    || self.our_session.as_deref() == Some(&app.session_id)
                    || jellyfin::is_jellyfin_app(&app.app_id);
                if ours {
                    self.send(stream, &app.transport_id, NS_CONNECTION, messages::connect())?;
                    self.transport = Some(app.transport_id.clone());
                    self.our_session = Some(app.session_id.clone());
                    self.status.media_session_id = None;
                    self.status.player_state = String::new();
                    self.status.idle_reason = None;
                    let speaks_media = app.speaks(NS_MEDIA);
                    self.status.app = Some(app.clone());
                    if speaks_media {
                        self.request(stream, &app.transport_id, NS_MEDIA, messages::media_status(0), Waiting::Quiet)?;
                        // The held commands wait for the media session.
                        if self.replay == Replay::ReceiverStatus {
                            self.replay = Replay::MediaStatus;
                            self.replay_asked = Instant::now();
                            self.replay_asks = 1;
                        }
                    }
                    // The Jellyfin app we know reports its state when asked.
                    if let Some(identity) = self.identity.clone().filter(|_| jellyfin::is_jellyfin_app(&app.app_id)) {
                        let payload = jellyfin::identify(&identity, &self.device.name);
                        self.send(stream, &app.transport_id, jellyfin::NAMESPACE, payload)?;
                    }
                    if let Some(request) = self.pending_load.take() {
                        self.load_now(stream, request)?;
                    }
                } else {
                    // Another sender's app: what we knew of ours is gone
                    // with it, also when the socket was lost in between.
                    self.app_gone();
                }
            }
            None => {
                self.app_gone();
                if launched {
                    self.pending_load = None;
                    self.fail("the app did not start");
                    return Ok(());
                }
            }
        }
        self.publish();
        // No media session to wait for (no app of ours, or one with no
        // media): the held commands go now, and each says for itself
        // when nothing runs.
        if self.replay == Replay::ReceiverStatus {
            self.replay = Replay::Sending;
        }
        Ok(())
    }

    fn app_gone(&mut self) {
        self.transport = None;
        self.our_session = None;
        self.identity = None;
        self.status.app = None;
        self.status.media_session_id = None;
        self.status.player_state = String::new();
        self.status.content_id = None;
        self.status.item_id = None;
        self.seek_in_flight = None;
        self.seek_queued = None;
    }

    fn on_media_status(&mut self, entries: Vec<MediaStatus>) {
        let entry = entries
            .iter()
            .find(|e| Some(e.media_session_id) == self.status.media_session_id)
            .or_else(|| entries.first());
        match entry {
            Some(entry) => {
                self.status.media_session_id = Some(entry.media_session_id);
                self.status.player_state = entry.player_state.clone();
                self.status.position = entry.current_time;
                self.status.position_at = Some(Instant::now());
                self.status.idle_reason = entry.idle_reason.clone();
                self.status.active_tracks = entry.active_track_ids.clone();
                if let Some(media) = &entry.media {
                    self.status.content_id = Some(media.content_id.clone());
                    if media.duration.is_some() {
                        self.status.duration = media.duration;
                    }
                }
                if entry.player_state == "IDLE" && entry.media.is_none() {
                    self.status.media_session_id = None;
                }
            }
            None => {
                self.status.media_session_id = None;
                self.status.player_state = "IDLE".into();
            }
        }
        self.publish();
    }
}

/// Two controls of which the later takes the place of the earlier: two
/// seeks, a pause and a play, two volumes, two mutes. A control is of a
/// kind with itself; any other command is of no kind.
fn same_kind(a: &Command, b: &Command) -> bool {
    matches!(
        (a, b),
        (Command::Seek(_), Command::Seek(_))
            | (Command::Play | Command::Pause, Command::Play | Command::Pause)
            | (Command::SetVolume(_), Command::SetVolume(_))
            | (Command::SetMuted(_), Command::SetMuted(_))
    )
}

// ----- TLS -----------------------------------------------------------------

/// The crypto of the app: rustls with ring, as `realtime.rs` and ureq use.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Takes the certificate of the device the first time, and only that one
/// after. The devices sign their own, so no root can check them.
#[derive(Debug, Default)]
struct DeviceCertificate {
    pinned: Mutex<Option<Vec<u8>>>,
}

impl ServerCertVerifier for DeviceCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut pinned = self.pinned.lock().unwrap();
        match &*pinned {
            Some(known) if known.as_slice() != end_entity.as_ref() => {
                Err(rustls::Error::General("the device shows another certificate than before".into()))
            }
            Some(_) => Ok(ServerCertVerified::assertion()),
            None => {
                *pinned = Some(end_entity.as_ref().to_vec());
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    //! The session against the mock device of `mock.rs`: a full launch,
    //! load, pause, seek, volume and stop, both ways, with the order and
    //! the payloads checked; then the heartbeat timeout and the reconnect.

    use serde_json::json;

    use super::*;
    use crate::chromecast::mock::Mock;

    fn timing() -> Timing {
        Timing {
            ping: Duration::from_millis(200),
            silence: Duration::from_secs(3),
            read_wait: Duration::from_millis(10),
            answer: Duration::from_secs(2),
        }
    }

    /// Takes the events that came, and waits until the status is as the
    /// test wants it or the time is up.
    fn until(
        session: &Session,
        events: &async_channel::Receiver<Event>,
        log: &mut Vec<Event>,
        what: &str,
        secs: f64,
        mut done: impl FnMut(&Status) -> bool,
    ) -> Status {
        let deadline = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            while let Ok(event) = events.try_recv() {
                log.push(event);
            }
            let status = session.status();
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what} timed out: {status:?}\n{log:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn identity() -> Identity {
        Identity {
            server_address: "https://jellyfin.example".into(),
            server_id: "srv1".into(),
            server_version: "12.1.0".into(),
            user_id: "u1".into(),
            device_id: "d1".into(),
            access_token: "fake-token".into(),
        }
    }

    fn request() -> LoadRequest {
        LoadRequest {
            jellyfin: Some(JellyfinLoad {
                identity: identity(),
                items: vec![ItemStub {
                    id: "i1".into(),
                    name: "Film".into(),
                    kind: "Movie".into(),
                    media_type: "Video".into(),
                    is_folder: false,
                }],
                start_secs: 30.,
                audio_index: None,
                subtitle_index: Some(-1),
            }),
            media: Some(LoadMedia {
                url: "https://jellyfin.example/Videos/i1/master.m3u8?ApiKey=fake-token".into(),
                content_type: "application/x-mpegURL".into(),
                title: "Film".into(),
                start_secs: 30.,
                duration: Some(600.),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn drives_the_jellyfin_app_then_the_media_receiver() {
        let mock = Mock::start();
        let (session, events) = Session::connect_with(mock.device(), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected && s.volume == 0.5);
        assert!(matches!(log.first(), Some(Event::Connected { again: false })), "{log:?}");

        // Two answers find their requests by id, whichever order they come.
        let availability = session.app_availability(vec![jellyfin::APP_STABLE.into(), "DEADBEEF".into()]);
        let status = session.get_status();
        let status = status.recv_timeout(Duration::from_secs(2)).unwrap();
        let availability = availability.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(status["type"], "RECEIVER_STATUS");
        assert_eq!(availability["responseType"], "GET_APP_AVAILABILITY");
        assert_eq!(availability["availability"]["F007D354"], "APP_AVAILABLE");
        assert_eq!(availability["availability"]["DEADBEEF"], "APP_UNAVAILABLE");
        let status = until(&session, &events, &mut log, "availability", 2., |s| !s.availability.is_empty());
        assert_eq!(status.availability.get("DEADBEEF").map(String::as_str), Some("APP_UNAVAILABLE"));
        assert!(status.rtt_ms.is_some_and(|ms| ms < 500.), "{:?}", status.rtt_ms);
        let before = mock.seen().len();

        // Mode 1: the Jellyfin app launches and gets the item.
        session.load(request());
        let status = until(&session, &events, &mut log, "jellyfin load", 5., |s| {
            s.item_id.as_deref() == Some("i1") && s.player_state == "PLAYING"
        });
        assert_eq!(status.app.as_ref().unwrap().app_id, jellyfin::APP_STABLE);
        assert_eq!(status.app.as_ref().unwrap().transport_id, "transport-1");
        assert_eq!(status.position, 30.);
        assert_eq!(status.duration, Some(600.));
        let seen = mock.seen();
        assert_eq!(
            &seen[before..],
            &[
                (NS_RECEIVER.to_string(), "LAUNCH".to_string()),
                (NS_CONNECTION.to_string(), "CONNECT".to_string()),
                (NS_MEDIA.to_string(), "GET_STATUS".to_string()),
                (jellyfin::NAMESPACE.to_string(), "PlayNow".to_string()),
            ],
            "{seen:?}"
        );
        let play_now = mock.payloads(jellyfin::NAMESPACE).pop().unwrap();
        assert_eq!(play_now.0, "transport-1");
        assert_eq!(play_now.1["accessToken"], "fake-token");
        assert_eq!(play_now.1["serverAddress"], "https://jellyfin.example");
        assert_eq!(play_now.1["receiverName"], "Mock TV");
        assert_eq!(play_now.1["options"]["items"][0]["Id"], "i1");
        assert_eq!(play_now.1["options"]["startPositionTicks"], 300_000_000);
        assert_eq!(play_now.1["options"]["subtitleStreamIndex"], -1);
        let connects = mock.payloads(NS_CONNECTION);
        assert_eq!(connects.last().unwrap().0, "transport-1", "connected to the app's transport");

        // Pause, play, seek: the words of the Jellyfin app.
        session.pause();
        until(&session, &events, &mut log, "pause", 2., |s| s.player_state == "PAUSED");
        session.play();
        until(&session, &events, &mut log, "play", 2., |s| s.player_state == "PLAYING");
        session.seek(45.);
        let status = until(&session, &events, &mut log, "seek", 2., |s| {
            let seeks = mock.payloads(jellyfin::NAMESPACE).iter().filter(|p| p.1["command"] == "Seek").count();
            seeks == 1 && s.position == 45.
        });
        assert_eq!(status.player_state, "PLAYING");
        let seek = mock.payloads(jellyfin::NAMESPACE).into_iter().find(|p| p.1["command"] == "Seek").unwrap();
        assert_eq!(seek.1["options"]["position"], 45.0);
        // The receiver reports its subtitle index; it comes through the
        // message event untouched.
        assert!(log.iter().any(|e| matches!(e, Event::Message { namespace, payload } if namespace == jellyfin::NAMESPACE && payload["type"] == "playbackstart")));

        // Volume goes to the device, not the app.
        session.set_volume(0.4);
        until(&session, &events, &mut log, "volume", 2., |s| s.volume == 0.4);
        let volume = mock.payloads(NS_RECEIVER).into_iter().find(|p| p.1["type"] == "SET_VOLUME").unwrap();
        assert_eq!(volume.0, RECEIVER);
        assert_eq!(volume.1["volume"]["level"], 0.4);
        session.set_muted(true);
        until(&session, &events, &mut log, "mute", 2., |s| s.muted);

        // Stop the app: the device shows its idle screen.
        session.stop_app();
        until(&session, &events, &mut log, "stop app", 2., |s| s.app.is_none() && s.item_id.is_none());
        let stop = mock.payloads(NS_RECEIVER).into_iter().find(|p| p.1["type"] == "STOP").unwrap();
        assert_eq!(stop.1["sessionId"], "session-1");

        // Mode 2: the Jellyfin app does not launch; the media receiver
        // gets the stream URL.
        mock.set(|m| m.fails = vec![jellyfin::APP_STABLE.into()]);
        let before = mock.seen().len();
        session.load(request());
        let status = until(&session, &events, &mut log, "media load", 5., |s| {
            s.player_state == "PLAYING" && s.media_session_id == Some(1)
        });
        assert_eq!(status.app.as_ref().unwrap().app_id, messages::APP_DEFAULT_MEDIA);
        assert_eq!(status.content_id.as_deref(), Some("https://jellyfin.example/Videos/i1/master.m3u8?ApiKey=fake-token"));
        assert_eq!(status.position, 30.);
        let seen = mock.seen();
        assert_eq!(
            &seen[before..],
            &[
                (NS_RECEIVER.to_string(), "LAUNCH".to_string()),
                (NS_RECEIVER.to_string(), "LAUNCH".to_string()),
                (NS_CONNECTION.to_string(), "CONNECT".to_string()),
                (NS_MEDIA.to_string(), "GET_STATUS".to_string()),
                (NS_MEDIA.to_string(), "LOAD".to_string()),
            ],
            "{seen:?}"
        );
        let launches: Vec<Value> = mock.payloads(NS_RECEIVER).into_iter().filter(|p| p.1["type"] == "LAUNCH").map(|p| p.1["appId"].clone()).collect();
        assert_eq!(launches, vec![json!("F007D354"), json!("F007D354"), json!("CC1AD845")]);
        // The fall back is quiet: the refusal of the first app is no error
        // for the user.
        assert!(!log.iter().any(|e| matches!(e, Event::Error(_))), "{log:?}");
        let load = mock.payloads(NS_MEDIA).into_iter().find(|p| p.1["type"] == "LOAD").unwrap();
        assert_eq!(load.0, "transport-3");
        assert_eq!(load.1["media"]["contentType"], "application/x-mpegURL");
        assert_eq!(load.1["currentTime"], 30.0);
        assert_eq!(load.1["autoplay"], true);

        session.pause();
        until(&session, &events, &mut log, "media pause", 2., |s| s.player_state == "PAUSED");
        let pause = mock.payloads(NS_MEDIA).into_iter().find(|p| p.1["type"] == "PAUSE").unwrap();
        assert_eq!(pause.1["mediaSessionId"], 1);
        session.play();
        until(&session, &events, &mut log, "media play", 2., |s| s.player_state == "PLAYING");

        // Seeks coalesce: the second waits for the first, the third takes
        // the place of the second.
        mock.set(|m| m.seek_delay = Duration::from_millis(400));
        session.seek(10.);
        thread::sleep(Duration::from_millis(60));
        session.seek(20.);
        thread::sleep(Duration::from_millis(60));
        session.seek(30.);
        let status = until(&session, &events, &mut log, "seeks", 5., |s| {
            mock.payloads(NS_MEDIA).iter().filter(|p| p.1["type"] == "SEEK").count() == 2 && s.position == 30.
        });
        thread::sleep(Duration::from_millis(600));
        let seeks: Vec<Value> = mock.payloads(NS_MEDIA).into_iter().filter(|p| p.1["type"] == "SEEK").map(|p| p.1["currentTime"].clone()).collect();
        assert_eq!(seeks, vec![json!(10.0), json!(30.0)]);
        assert_eq!(status.player_state, "PLAYING");
        mock.set(|m| m.seek_delay = Duration::ZERO);

        session.set_subtitle(Some(2));
        until(&session, &events, &mut log, "subtitle", 2., |s| s.active_tracks == [2]);
        let tracks = mock.payloads(NS_MEDIA).into_iter().find(|p| p.1["type"] == "EDIT_TRACKS_INFO").unwrap();
        assert_eq!(tracks.1["activeTrackIds"], json!([2]));

        session.stop_media();
        until(&session, &events, &mut log, "media stop", 2., |s| s.player_state == "IDLE" && s.idle_reason.as_deref() == Some("CANCELLED"));

        // The whole run went over one connection.
        assert_eq!(mock.state.lock().unwrap().connections, 1);
        drop(session);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(mock.seen().last().unwrap().1, "CLOSE");
    }

    #[test]
    fn connects_again_when_the_device_goes_silent() {
        let mock = Mock::start();
        let short = Timing {
            ping: Duration::from_millis(50),
            silence: Duration::from_millis(400),
            read_wait: Duration::from_millis(10),
            answer: Duration::from_secs(1),
        };
        let (session, events) = Session::connect_with(mock.device(), short);
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        // PINGs go out and PONGs come back, so the socket stays.
        thread::sleep(Duration::from_millis(600));
        assert_eq!(mock.state.lock().unwrap().connections, 1);
        let pings = mock.seen().iter().filter(|m| m.1 == "PING").count();
        assert!(pings >= 5, "{pings} pings");
        assert!(session.status().connected);
        // An app of ours runs.
        session.launch(vec![messages::APP_DEFAULT_MEDIA.into()]);
        until(&session, &events, &mut log, "launch", 3., |s| s.app.is_some());

        // The device says nothing: the session gives the socket up and
        // opens it again, and joins its app again.
        mock.set(|m| m.silent = true);
        until(&session, &events, &mut log, "closed", 3., |s| !s.connected);
        mock.set(|m| m.silent = false);
        let connects = mock.payloads(NS_CONNECTION).len();
        until(&session, &events, &mut log, "again", 5., |s| s.connected);
        // The mock counts a connection a moment after the session has it.
        until(&session, &events, &mut log, "second counted", 3., |_| mock.state.lock().unwrap().connections >= 2);
        assert_eq!(mock.state.lock().unwrap().connections, 2);
        until(&session, &events, &mut log, "joined again", 3., |_| mock.payloads(NS_CONNECTION).len() >= connects + 2);
        let targets: Vec<String> = mock.payloads(NS_CONNECTION)[connects..].iter().map(|p| p.0.clone()).collect();
        assert_eq!(targets, [RECEIVER, "transport-1"]);
        assert_eq!(session.status().app.as_ref().map(|a| a.transport_id.as_str()), Some("transport-1"));
        let kinds: Vec<&str> = log
            .iter()
            .filter_map(|e| match e {
                Event::Connected { again: false } => Some("open"),
                Event::Connected { again: true } => Some("open-again"),
                Event::Closed => Some("closed"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["open", "closed", "open-again"]);
        let answer = session.get_status().recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(answer["type"], "RECEIVER_STATUS");

        // A dropped socket as well.
        mock.set(|m| m.drop_now = true);
        until(&session, &events, &mut log, "dropped", 3., |s| !s.connected);
        until(&session, &events, &mut log, "third", 5., |s| s.connected);
        until(&session, &events, &mut log, "third counted", 3., |_| mock.state.lock().unwrap().connections >= 3);
        assert_eq!(mock.state.lock().unwrap().connections, 3);
    }

    /// Takes the events that came so far.
    fn drain(events: &async_channel::Receiver<Event>, log: &mut Vec<Event>) {
        while let Ok(event) = events.try_recv() {
            log.push(event);
        }
    }

    fn errors(log: &[Event]) -> Vec<&str> {
        log.iter()
            .filter_map(|e| match e {
                Event::Error(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// How many messages of a type the media receiver got.
    fn media_count(mock: &Mock, kind: &str) -> usize {
        mock.payloads(NS_MEDIA).iter().filter(|p| p.1["type"] == kind).count()
    }

    /// The times of a reconnect test: a drop shows at once (the mock
    /// closes the socket), and a status the device is slow with has time
    /// to come before the silence counts.
    fn reconnect_timing() -> Timing {
        Timing {
            ping: Duration::from_millis(50),
            silence: Duration::from_secs(5),
            read_wait: Duration::from_millis(10),
            answer: Duration::from_secs(1),
        }
    }

    /// The media receiver with an item, over a connection with short times.
    fn media_receiver_playing(mock: &Mock, timing: Timing) -> (Session, async_channel::Receiver<Event>, Vec<Event>) {
        mock.set(|m| m.fails = vec![jellyfin::APP_STABLE.into()]);
        let (session, events) = Session::connect_with(mock.device(), timing);
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        session.load(request());
        until(&session, &events, &mut log, "media load", 5., |s| s.player_state == "PLAYING" && s.media_session_id == Some(1));
        (session, events, log)
    }

    /// The socket is lost, and the user pauses and scrubs meanwhile. The
    /// PAUSE and SEEK messages the device got, and the errors so far.
    fn dropped_with_held_controls(mock: &Mock) -> (Session, async_channel::Receiver<Event>, Vec<Event>, usize, usize) {
        let (session, events, mut log) = media_receiver_playing(mock, reconnect_timing());
        mock.set(|m| m.drop_now = true);
        until(&session, &events, &mut log, "dropped", 3., |s| !s.connected);
        let sent_before = media_count(mock, "PAUSE") + media_count(mock, "SEEK");
        session.pause();
        for n in 0..200 {
            session.seek(100. + f64::from(n));
        }
        let errors_before = errors(&log).len();
        (session, events, log, sent_before, errors_before)
    }

    /// A pause and a burst of seeks sent while the socket is down reach
    /// the app once the connection and the media session are back: one
    /// PAUSE and one SEEK, with no error.
    #[test]
    fn controls_held_during_a_reconnect_reach_the_app() {
        let mock = Mock::start();
        let (session, events, mut log, sent_before, errors_before) = dropped_with_held_controls(&mock);
        until(&session, &events, &mut log, "again", 5., |s| s.connected);
        let status = until(&session, &events, &mut log, "paused at the last seek", 5., |s| {
            s.player_state == "PAUSED" && s.media_session_id == Some(1) && s.position == 299.
        });
        // Nothing more goes out: the status at the end of the replay is
        // the last word.
        thread::sleep(Duration::from_millis(300));
        drain(&events, &mut log);
        let sent = media_count(&mock, "SEEK") + media_count(&mock, "PAUSE") - sent_before;
        eprintln!("after the reconnect: {sent} media messages for a pause and 200 seeks");
        assert_eq!(status.error, None);
        assert_eq!(errors(&log).len(), errors_before, "{log:?}");
        assert_eq!(sent, 2, "{:?}", mock.seen());
        let device = mock.state.lock().unwrap().media.clone().unwrap();
        assert_eq!((device.state.as_str(), device.position), ("PAUSED", 299.));
    }

    /// The socket dies again as the held commands go out, right after the
    /// PAUSE was written and before the device did it: the SEEK that
    /// waited behind it is still held, and the PAUSE, which got no answer,
    /// is held again. Both reach the app over the third connection: the
    /// device ends paused at the last seek, as the user asked.
    #[test]
    fn a_second_drop_during_the_replay_keeps_the_rest() {
        let mock = Mock::start();
        let (session, events, mut log, sent_before, errors_before) = dropped_with_held_controls(&mock);
        mock.set(|m| m.drop_after = Some("PAUSE".into()));
        // The second connection lives a few milliseconds: the third one
        // is the evidence, with the two closes in the events.
        until(&session, &events, &mut log, "third", 8., |_| mock.state.lock().unwrap().connections >= 3);
        let status = until(&session, &events, &mut log, "paused at the last seek", 5., |s| {
            s.connected && s.player_state == "PAUSED" && s.media_session_id == Some(1) && s.position == 299.
        });
        thread::sleep(Duration::from_millis(300));
        drain(&events, &mut log);
        assert_eq!(log.iter().filter(|e| matches!(e, Event::Closed)).count(), 2, "{log:?}");
        assert_eq!(mock.state.lock().unwrap().connections, 3);
        // One PAUSE into the socket that died, one PAUSE and one SEEK after.
        assert_eq!(media_count(&mock, "PAUSE"), 2, "{:?}", mock.seen());
        assert_eq!(media_count(&mock, "SEEK") - sent_before, 1, "{:?}", mock.seen());
        assert_eq!(status.error, None);
        assert_eq!(errors(&log).len(), errors_before, "{log:?}");
        let device = mock.state.lock().unwrap().media.clone().unwrap();
        assert_eq!((device.state.as_str(), device.position), ("PAUSED", 299.));
    }

    /// The media status after the join comes later than an answer is
    /// waited for: the held commands wait for it all the same, and go
    /// out when it comes.
    #[test]
    fn a_late_media_status_still_gets_the_held_controls() {
        let mock = Mock::start();
        let (session, events, mut log, sent_before, errors_before) = dropped_with_held_controls(&mock);
        mock.set(|m| m.status_delay = Duration::from_millis(1500));
        until(&session, &events, &mut log, "again", 5., |s| s.connected);
        let connected = Instant::now();
        let status = until(&session, &events, &mut log, "paused at the last seek", 6., |s| {
            s.player_state == "PAUSED" && s.media_session_id == Some(1) && s.position == 299.
        });
        assert!(connected.elapsed() >= Duration::from_millis(1400), "the controls went before the status");
        thread::sleep(Duration::from_millis(300));
        drain(&events, &mut log);
        assert_eq!(status.error, None);
        assert_eq!(errors(&log).len(), errors_before, "{log:?}");
        assert_eq!(media_count(&mock, "SEEK") + media_count(&mock, "PAUSE") - sent_before, 2, "{:?}", mock.seen());
    }

    /// The app lost its item while the socket was down: the media status
    /// says so, and each held control fails for itself with a word for the
    /// user instead of going out to nothing. The connection goes on.
    #[test]
    fn an_empty_media_status_fails_the_held_controls() {
        let mock = Mock::start();
        let (session, events, mut log, sent_before, errors_before) = dropped_with_held_controls(&mock);
        mock.set(|m| m.media = None);
        until(&session, &events, &mut log, "again", 5., |s| s.connected);
        until(&session, &events, &mut log, "the controls failed", 5., |s| s.error.as_deref() == Some("nothing is loaded"));
        thread::sleep(Duration::from_millis(300));
        drain(&events, &mut log);
        assert_eq!(&errors(&log)[errors_before..], ["nothing is loaded", "nothing is loaded"]);
        assert_eq!(media_count(&mock, "SEEK") + media_count(&mock, "PAUSE") - sent_before, 0, "{:?}", mock.seen());
        // Done with the held ones: a command goes at once again.
        let answer = session.get_status().recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(answer["type"], "RECEIVER_STATUS");
    }

    /// Another sender started another app while the socket was down: the
    /// held controls fail with a word, and a load starts our app again.
    #[test]
    fn an_app_replaced_during_the_outage_fails_the_held_controls() {
        let mock = Mock::start();
        let (session, events, mut log, sent_before, errors_before) = dropped_with_held_controls(&mock);
        mock.set(|m| {
            m.app = Some(("YOUTUBE".into(), "session-other".into(), "transport-other".into()));
            m.media = None;
        });
        until(&session, &events, &mut log, "again", 5., |s| s.connected);
        let status = until(&session, &events, &mut log, "the controls failed", 5., |s| s.error.as_deref() == Some("no app runs"));
        thread::sleep(Duration::from_millis(300));
        drain(&events, &mut log);
        assert_eq!(&errors(&log)[errors_before..], ["no app runs", "no app runs"]);
        // Nothing of the old app lingers in the status.
        assert_eq!((status.app, status.media_session_id, status.player_state.as_str()), (None, None, ""), "{log:?}");
        assert_eq!(media_count(&mock, "SEEK") + media_count(&mock, "PAUSE") - sent_before, 0, "{:?}", mock.seen());
        session.load(request());
        until(&session, &events, &mut log, "our app again", 5., |s| {
            s.app.as_ref().is_some_and(|app| app.app_id == messages::APP_DEFAULT_MEDIA)
                && s.player_state == "PLAYING"
                && s.media_session_id == Some(1)
        });
    }

    /// The device answers the heartbeat but no status after the connect:
    /// the status is asked again, and after the last ask the held
    /// controls are given up with one word, not sent into the dark. The
    /// connection goes on and takes commands again.
    #[test]
    fn held_controls_are_given_up_when_the_device_says_not_what_it_runs() {
        let mock = Mock::start();
        let (session, events, mut log, sent_before, errors_before) = dropped_with_held_controls(&mock);
        mock.set(|m| m.mute_status = true);
        until(&session, &events, &mut log, "again", 5., |s| s.connected);
        let connected = Instant::now();
        until(&session, &events, &mut log, "given up", 8., |s| s.error.as_deref().is_some_and(|e| e.contains("did not say")));
        thread::sleep(Duration::from_millis(300));
        drain(&events, &mut log);
        let asks = reconnect_timing().answer * (REPLAY_ASKS - 1);
        assert!(connected.elapsed() >= asks, "given up after {:?}, before the asks", connected.elapsed());
        let late = &errors(&log)[errors_before..];
        assert_eq!(late.len(), 1, "{late:?}");
        assert!(late[0].contains("did not say what it runs") && late[0].contains("2 commands"), "{late:?}");
        assert_eq!(media_count(&mock, "SEEK") + media_count(&mock, "PAUSE") - sent_before, 0, "{:?}", mock.seen());
        mock.set(|m| m.mute_status = false);
        let answer = session.get_status().recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(answer["type"], "RECEIVER_STATUS");
    }

    /// A LAUNCH the device never answers: the next app is tried when the
    /// answer is given up, and when none is left the load fails for the
    /// user instead of waiting for ever.
    #[test]
    fn a_launch_without_an_answer_falls_back_then_fails() {
        let mock = Mock::start();
        let (session, events) = Session::connect_with(mock.device(), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        // The Jellyfin app hangs; the media receiver takes the item.
        mock.set(|m| m.ignores = vec![jellyfin::APP_STABLE.into()]);
        let started = Instant::now();
        session.load(request());
        let status = until(&session, &events, &mut log, "fallback after the silence", 6., |s| {
            s.player_state == "PLAYING" && s.media_session_id == Some(1)
        });
        assert_eq!(status.app.as_ref().unwrap().app_id, messages::APP_DEFAULT_MEDIA);
        assert!(started.elapsed() >= timing().answer, "the fallback must wait for the answer time");
        assert!(errors(&log).is_empty(), "{log:?}");

        // Both apps hang: an error, and the pending load is gone.
        session.stop_app();
        until(&session, &events, &mut log, "stopped", 3., |s| s.app.is_none());
        mock.set(|m| m.ignores = vec![jellyfin::APP_STABLE.into(), messages::APP_DEFAULT_MEDIA.into()]);
        session.load(request());
        until(&session, &events, &mut log, "error after the silence", 8., |s| s.error.is_some());
        assert_eq!(errors(&log), ["the device did not answer the launch"]);
    }

    /// A seek while the media receiver is paused leaves it paused, as the
    /// Cast protocol does when `resumeState` is not given.
    #[test]
    fn a_seek_while_paused_keeps_the_media_receiver_paused() {
        let mock = Mock::start();
        let (session, events, mut log) = media_receiver_playing(&mock, timing());
        session.pause();
        until(&session, &events, &mut log, "pause", 2., |s| s.player_state == "PAUSED");
        session.seek(100.);
        until(&session, &events, &mut log, "seek", 2., |_| media_count(&mock, "SEEK") == 1);
        thread::sleep(Duration::from_millis(200));
        session.media_status();
        let status = until(&session, &events, &mut log, "status", 2., |s| s.position == 100.);
        let device = mock.state.lock().unwrap().media.clone().unwrap();
        assert_eq!(device.state, "PAUSED", "the device plays after a seek while paused");
        assert_eq!(status.player_state, "PAUSED");
    }
}
