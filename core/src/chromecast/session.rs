// SPDX-License-Identifier: AGPL-3.0-or-later
//! One connection to one cast device. A thread holds the TLS socket open,
//! answers the heartbeat, matches answers to requests by their id, and
//! connects again when the socket is lost. The app talks to it through
//! [`Session`] and hears from it on a channel, the way the socket of the
//! server (`realtime.rs`) and the player do.
//!
//! The thread sleeps on one channel: the commands of the app come on it,
//! and so do the bytes of the socket, which a second thread reads. So a
//! command goes out the moment it comes, and nothing wakes the thread
//! between a word of the device, a command and the next thing that is due
//! (a ping, an answer that is late).
//!
//! The devices use a certificate of their own, signed by nobody the system
//! knows. The connection takes the certificate the device shows, and from
//! then on only that one: a connect-again with another certificate fails.
//! The app keeps the certificate with its config, by the id of the device
//! ([`Event::Pinned`]), and gives it back at the next connection
//! ([`Session::connect`]), so the trust of the first use holds across
//! runs of the app.
//!
//! A command does not open a connection: the socket stays open, so a pause
//! or a seek is one message on a warm connection. Seeks are coalesced: a
//! seek sent while one waits for its answer is held, and only the last one
//! held goes out when the answer comes.

use std::{
    collections::HashMap,
    io::{self, ErrorKind, Read as _, Write as _},
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use rustls::{
    ClientConnection, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde_json::Value;

use super::{
    jellyfin::{self, Identity, ItemStub},
    discovery::Device,
    messages::{self, App, LoadMedia, MediaStatus, PlayerState, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, RECEIVER, ReceiverStatus, SENDER},
    proto::{CastMessage, Inbox},
};

/// The times of the connection; a test makes them short.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Time between two PINGs of ours.
    pub ping: Duration,
    /// With no word of the device for this long the socket counts as dead.
    pub silence: Duration,
    /// An answer that does not come in this time is given up.
    pub answer: Duration,
    /// The same for a launch: a receiver app that starts cold takes
    /// longer than a command, and a launch given up too soon is replaced
    /// by the next app while the first still comes up.
    pub launch: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(5),
            silence: Duration::from_secs(15),
            answer: Duration::from_secs(10),
            launch: Duration::from_secs(30),
        }
    }
}

/// The refusal of a certificate that is not the one kept for the device.
const OTHER_CERTIFICATE: &str = "the device shows another certificate than before";

/// What the thread tells the app, at the moment it happens.
#[derive(Clone, Debug)]
pub enum Event {
    /// The socket is open; `again` when it was open before in this run.
    Connected { again: bool },
    /// Something in the status changed.
    Status(Box<Status>),
    /// A message of the receiver app, such as a playback report of the
    /// Jellyfin app, or one of a namespace this module does not know.
    Message { namespace: String, payload: Value },
    Error(String),
    /// The item of a load is gone from the device: the app that had it
    /// ended or gave way to another app, or the load never reached an app.
    /// `load` is the number [`Session::load`] gave for it, so the app can
    /// tell the end of the item before from the item it just sent.
    Ended { load: u64 },
    /// The certificate the connection trusts from now on, for the app to
    /// keep: the one of a first connection, or the new one of a device
    /// that made itself another (see `Worker::run`).
    Pinned(Vec<u8>),
    Closed,
}

/// The Jellyfin app answers a seek with a report that carries no request
/// id: a seek counts as answered after this time.
const SEEK_ANSWER: Duration = Duration::from_millis(250);

/// The picture of the device, as of the last word of it.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub connected: bool,
    /// The app that runs on the device and that we are connected to.
    pub app: Option<App>,
    /// 0 to 1.
    pub volume: f64,
    pub muted: bool,
    /// `None` with nothing loaded.
    pub player_state: Option<PlayerState>,
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
            Some(at) if self.player_state == Some(PlayerState::Playing) => self.position + at.elapsed().as_secs_f64(),
            _ => self.position,
        }
    }

    /// An item is in the player of the app: the transport shows.
    pub fn loaded(&self) -> bool {
        matches!(self.player_state, Some(PlayerState::Playing | PlayerState::Paused | PlayerState::Buffering))
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
    /// With the number of the load (see [`Event::Ended`]).
    Load(Box<LoadRequest>, u64),
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

impl Command {
    /// A control: sent twice it does what it does once, and a later one of
    /// its kind takes its place (see `same_kind`).
    fn is_control(&self) -> bool {
        matches!(self, Command::Seek(_) | Command::Play | Command::Pause | Command::SetVolume(_) | Command::SetMuted(_))
    }
}

/// What the thread wakes for.
enum Input {
    Command(Command),
    /// Bytes of the socket with this number, from the thread that reads it.
    Bytes { link: u64, bytes: Vec<u8> },
    /// The socket with this number is closed or broken.
    Lost { link: u64, why: String },
}

struct Shared {
    status: Mutex<Status>,
    /// The loads the app sent so far; the last one's number.
    loads: AtomicU64,
}

/// The handle of the app. Dropping it closes the connection: the close is
/// a command behind the ones sent before it, so a stop sent right before
/// the drop still goes out.
pub struct Session {
    device: Device,
    commands: mpsc::Sender<Input>,
    shared: Arc<Shared>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.send(Command::Close);
    }
}

impl Session {
    /// Opens the connection on its own thread. The events come on the
    /// returned channel. `pin` is the certificate the device showed at an
    /// earlier connection, when the app kept one: the device must show it
    /// again. With none, the certificate it shows now is taken and told
    /// ([`Event::Pinned`]).
    pub fn connect(device: Device, pin: Option<Vec<u8>>) -> (Self, async_channel::Receiver<Event>) {
        Self::start(device, pin, Timing::default())
    }

    /// With no certificate kept and times of its own, for a test.
    pub fn connect_with(device: Device, timing: Timing) -> (Self, async_channel::Receiver<Event>) {
        Self::start(device, None, timing)
    }

    fn start(device: Device, pin: Option<Vec<u8>>, timing: Timing) -> (Self, async_channel::Receiver<Event>) {
        let (tx, rx) = async_channel::unbounded();
        let (commands, inputs) = mpsc::channel();
        let shared = Arc::new(Shared { status: Mutex::new(Status::default()), loads: AtomicU64::new(0) });
        let mut worker = Worker::new(device.clone(), timing, tx, shared.clone(), commands.clone(), inputs);
        worker.told_pin = pin.clone();
        worker.verifier = Arc::new(DeviceCertificate { pinned: Mutex::new(pin) });
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
        let _ = self.commands.send(Input::Command(command));
    }

    // ----- for the debug channel and the tests: `get_status`,
    // `app_availability`, `launch`, `join`, `stop_app`, `media_status` and
    // `request` have no caller in the panel.

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

    /// Launches the right app when it does not run, then loads. Gives the
    /// number of this load; an [`Event::Ended`] with it says the item is
    /// gone from the device.
    pub fn load(&self, request: LoadRequest) -> u64 {
        let load = self.shared.loads.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(Command::Load(Box::new(request), load));
        load
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

/// The socket after the handshake. This thread writes to it; the bytes
/// that come are read by a thread of their own and handed to `take`.
struct Stream {
    connection: ClientConnection,
    socket: TcpStream,
}

impl Stream {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.connection.writer().write_all(bytes)?;
        self.flush()
    }

    fn flush(&mut self) -> io::Result<()> {
        while self.connection.wants_write() {
            self.connection.write_tls(&mut self.socket)?;
        }
        Ok(())
    }

    /// Takes bytes of the socket and puts what they say into the inbox.
    /// False when the device closed the connection.
    fn take(&mut self, mut bytes: &[u8], inbox: &mut Inbox) -> Result<bool> {
        let mut open = true;
        loop {
            let taken = if bytes.is_empty() { 0 } else { self.connection.read_tls(&mut bytes)? };
            self.connection.process_new_packets().context("TLS")?;
            loop {
                match inbox.fill(&mut self.connection.reader()) {
                    Ok(true) => {}
                    Ok(false) => {
                        open = false;
                        break;
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) => return Err(err.into()),
                }
            }
            if bytes.is_empty() || !open {
                break;
            }
            if taken == 0 {
                return Err(anyhow!("TLS takes no more bytes"));
            }
        }
        self.flush()?;
        Ok(open)
    }
}

struct Worker {
    device: Device,
    timing: Timing,
    tx: async_channel::Sender<Event>,
    shared: Arc<Shared>,
    inputs: mpsc::Receiver<Input>,
    /// The sending side of `inputs`, for the thread that reads a socket.
    feed: mpsc::Sender<Input>,
    /// The number of the socket that is open; bytes of one before it are
    /// not for this one.
    link: u64,
    verifier: Arc<DeviceCertificate>,
    /// The certificate the app knows of: the one it gave, or the last one
    /// told with [`Event::Pinned`].
    told_pin: Option<Vec<u8>>,
    status: Status,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// The transport of the app we are connected to.
    transport: Option<String>,
    /// The session id of that app, to join it again after a reconnect.
    our_session: Option<String>,
    /// What the Jellyfin app is told on every message, once known.
    identity: Option<Identity>,
    /// Waits for the app to launch, with its number.
    pending_load: Option<(u64, LoadRequest)>,
    /// The load whose item the app we are connected to got.
    app_load: Option<u64>,
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
        feed: mpsc::Sender<Input>,
        inputs: mpsc::Receiver<Input>,
    ) -> Self {
        Self {
            device,
            timing,
            tx,
            shared,
            inputs,
            feed,
            link: 0,
            verifier: Arc::new(DeviceCertificate::default()),
            told_pin: None,
            status: Status::default(),
            next_id: 1,
            pending: HashMap::new(),
            transport: None,
            our_session: None,
            identity: None,
            pending_load: None,
            app_load: None,
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

    fn run(mut self) {
        // Connects in a row that the kept certificate refused.
        let mut refused = 0;
        let (mut again, mut wait) = (false, Duration::from_secs(1));
        loop {
            match self.connect() {
                Ok(mut stream) => {
                    wait = Duration::from_secs(1);
                    refused = 0;
                    let served = self.serve(&mut stream, again);
                    if served.is_ok() {
                        let _ = stream.write_all(&self.frame(RECEIVER, NS_CONNECTION, messages::close()).frame());
                    }
                    // Ends the thread that reads the socket.
                    let _ = stream.socket.shutdown(Shutdown::Both);
                    match served {
                        Ok(()) => return,
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
                        && command.is_control()
                        && !self.held.iter().any(|held| same_kind(held, &command))
                    {
                        self.held.insert(0, command);
                    }
                    self.pending.clear();
                    self.seek_in_flight = None;
                    // The launch went with the socket; a load that waited
                    // for it must not play over whatever runs on the
                    // device when the next socket opens.
                    self.drop_pending_load();
                    // The new socket must connect to the app again; the
                    // status that comes first says whether it still runs.
                    self.transport = None;
                    self.publish();
                    self.emit(Event::Closed);
                }
                Err(err) => {
                    let text = format!("{err:#}");
                    // A device makes itself a new certificate now and
                    // then. The one kept from the first connection would
                    // then refuse it at every try, for good: after three
                    // tries in a row the next certificate is taken.
                    // Only for that refusal: a device that is off or out of
                    // reach must not cost the certificate that is kept.
                    if text.contains(OTHER_CERTIFICATE) {
                        refused += 1;
                    } else {
                        refused = 0;
                    }
                    if refused >= 3 {
                        *self.verifier.pinned.lock().unwrap() = None;
                    }
                    // Into the status as well: the panel says why the
                    // device is not connected.
                    self.fail(text);
                }
            }
            // The commands that come meanwhile are held; a close ends the
            // thread at once.
            let until = Instant::now() + wait;
            loop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match self.inputs.recv_timeout(left) {
                    Ok(Input::Command(Command::Close)) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(Input::Command(command)) => self.hold(command),
                    // Of the socket that is gone.
                    Ok(Input::Bytes { .. } | Input::Lost { .. }) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                }
            }
            wait = (wait * 2).min(Duration::from_secs(15));
        }
    }

    /// Opens the socket, and starts the thread that reads it.
    fn connect(&mut self) -> Result<Stream> {
        let address = (self.device.address, self.device.port);
        let mut socket = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5)).context("connect")?;
        socket.set_nodelay(true).ok();
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(self.verifier.clone())
            .with_no_client_auth();
        let name = ServerName::IpAddress(std::net::IpAddr::V4(self.device.address).into());
        let mut connection = ClientConnection::new(Arc::new(config), name)?;
        while connection.is_handshaking() {
            connection.complete_io(&mut socket).context("TLS handshake")?;
        }
        // From here the reads wait for as long as the device is quiet.
        socket.set_read_timeout(None)?;
        self.link += 1;
        let (link, feed, mut reader) = (self.link, self.feed.clone(), socket.try_clone()?);
        thread::Builder::new().name(format!("cast-read-{}", self.device.name)).spawn(move || {
            let mut chunk = [0u8; 16 * 1024];
            loop {
                let input = match reader.read(&mut chunk) {
                    Ok(0) => Input::Lost { link, why: "the device closed the socket".into() },
                    Ok(n) => Input::Bytes { link, bytes: chunk[..n].to_vec() },
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(err) => Input::Lost { link, why: format!("read: {err}") },
                };
                let lost = matches!(input, Input::Lost { .. });
                if feed.send(input).is_err() || lost {
                    return;
                }
            }
        })?;
        Ok(Stream { connection, socket })
    }

    /// `Ok` when the session was closed on purpose.
    fn serve(&mut self, stream: &mut Stream, again: bool) -> Result<()> {
        let mut inbox = Inbox::default();
        self.send(stream, RECEIVER, NS_CONNECTION, messages::connect())?;
        self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(), Waiting::Quiet)?;
        self.status.connected = true;
        self.status.error = None;
        self.emit(Event::Connected { again });
        // The certificate of this connection is the one kept from now on;
        // the app hears of a new one.
        let pinned = self.verifier.pinned.lock().unwrap().clone();
        if let Some(certificate) = pinned.filter(|pinned| self.told_pin.as_ref() != Some(pinned)) {
            self.told_pin = Some(certificate.clone());
            self.emit(Event::Pinned(certificate));
        }
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
            // What is due.
            if last_ping.elapsed() >= self.timing.ping {
                self.send(stream, RECEIVER, NS_HEARTBEAT, messages::ping())?;
                last_ping = Instant::now();
            }
            if last_heard.elapsed() >= self.timing.silence {
                return Err(anyhow!("no word of the device for {:?}", self.timing.silence));
            }
            self.expire(stream)?;
            self.replay_step(stream)?;
            // Then sleep until a command, a word of the device, or the
            // next thing that is due.
            let wait = self.next_due(last_ping, last_heard).saturating_duration_since(Instant::now());
            match self.inputs.recv_timeout(wait) {
                // A close does not wait behind the held commands: they
                // were for a target the user has left.
                Ok(Input::Command(Command::Close)) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                // Behind the ones that wait, so the order holds.
                Ok(Input::Command(command)) if self.replay != Replay::Done => self.hold(command),
                Ok(Input::Command(command)) => self.command(stream, command)?,
                Ok(Input::Bytes { link, bytes }) if link == self.link => {
                    let open = stream.take(&bytes, &mut inbox)?;
                    while let Some(message) = inbox.message()? {
                        last_heard = Instant::now();
                        self.receive(stream, message)?;
                    }
                    if !open {
                        return Err(anyhow!("the device closed the connection"));
                    }
                }
                Ok(Input::Lost { link, why }) if link == self.link => return Err(anyhow!(why)),
                // Of a socket before this one.
                Ok(Input::Bytes { .. } | Input::Lost { .. }) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// When the loop has work with no input: the next ping, the silence
    /// that ends the socket, an answer that is given up, the held commands.
    fn next_due(&self, last_ping: Instant, last_heard: Instant) -> Instant {
        let mut due = (last_ping + self.timing.ping).min(last_heard + self.timing.silence);
        for pending in self.pending.values() {
            due = due.min(pending.sent + self.patience(&pending.waiting));
        }
        if let Some(sent) = self.seek_in_flight
            && !self.seek_awaits_answer()
        {
            due = due.min(sent + SEEK_ANSWER);
        }
        match self.replay {
            Replay::Done => {}
            Replay::ReceiverStatus | Replay::MediaStatus => due = due.min(self.replay_asked + self.timing.answer),
            // The next held command goes when the one before is answered,
            // which is an input; with none to wait for it goes now.
            Replay::Sending if self.replay_wait.is_none() => return Instant::now(),
            Replay::Sending => {}
        }
        // A moment past the time, so what is due is due at the wake.
        due + Duration::from_millis(1)
    }

    /// How long an answer is waited for.
    fn patience(&self, waiting: &Waiting) -> Duration {
        if matches!(waiting, Waiting::Launch { .. }) { self.timing.launch } else { self.timing.answer }
    }

    /// A seek waits for an answer with its request id.
    fn seek_awaits_answer(&self) -> bool {
        self.pending.values().any(|p| matches!(p.waiting, Waiting::Seek))
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
    /// it does on a live connection.
    fn replay_step(&mut self, stream: &mut Stream) -> Result<()> {
        match self.replay {
            Replay::Done => {}
            Replay::ReceiverStatus | Replay::MediaStatus => {
                if self.replay_asked.elapsed() <= self.timing.answer {
                    return Ok(());
                }
                if self.replay_asks >= REPLAY_ASKS {
                    let dropped = std::mem::take(&mut self.held).len();
                    self.replay = Replay::Done;
                    self.fail(format!("the device did not say what it runs; {dropped} commands dropped"));
                    return Ok(());
                }
                self.replay_asks += 1;
                self.replay_asked = Instant::now();
                log::info!("cast {}: no status yet; asking again", self.device.name);
                match self.app_transport().filter(|_| self.replay == Replay::MediaStatus) {
                    Some(transport) => {
                        self.request(stream, &transport, NS_MEDIA, messages::get_status(), Waiting::Quiet)?;
                    }
                    None => {
                        self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(), Waiting::Quiet)?;
                    }
                }
            }
            Replay::Sending => {
                if self.replay_wait.as_ref().is_some_and(|(id, _)| self.pending.contains_key(id)) {
                    return Ok(());
                }
                self.replay_wait = None;
                if self.held.is_empty() {
                    self.replay = Replay::Done;
                    return Ok(());
                }
                let command = self.held.remove(0);
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
        Ok(())
    }

    /// Gives up answers that do not come: a seek that waited lets the
    /// next one go; a launch tries the next app, or fails the load.
    fn expire(&mut self, stream: &mut Stream) -> Result<()> {
        let late: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.sent.elapsed() > self.patience(&p.waiting))
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            match self.pending.remove(&id).map(|p| p.waiting) {
                Some(Waiting::Seek) => self.seek_in_flight = None,
                Some(Waiting::Launch { rest }) if !rest.is_empty() => {
                    log::info!("cast {}: no answer to the launch; next app", self.device.name);
                    self.launch(stream, rest)?;
                }
                Some(Waiting::Launch { .. }) => {
                    self.drop_pending_load();
                    self.fail("the device did not answer the launch");
                }
                Some(Waiting::Reply(_) | Waiting::Quiet) | None => {}
            }
        }
        if self.seek_in_flight.is_some_and(|at| at.elapsed() > SEEK_ANSWER) && !self.seek_awaits_answer() {
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
        self.emit(Event::Status(Box::new(self.status.clone())));
    }

    /// The load that waits for its app is given up; the app hears that
    /// its item is not on the device.
    fn drop_pending_load(&mut self) {
        if let Some((load, _)) = self.pending_load.take() {
            self.emit(Event::Ended { load });
        }
    }

    /// The app that had the item of a load is gone, or another item took
    /// its place there.
    fn end_app_load(&mut self) {
        if let Some(load) = self.app_load.take() {
            self.emit(Event::Ended { load });
        }
    }

    fn frame(&self, destination: &str, namespace: &str, payload: Value) -> CastMessage {
        CastMessage::text(SENDER, destination, namespace, payload.to_string())
    }

    fn send(&mut self, stream: &mut Stream, destination: &str, namespace: &str, payload: Value) -> Result<()> {
        log::trace!("cast > {destination} {namespace} {}", without_secrets(&payload));
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
        // The messages are made without an id; this is where they get it.
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
                self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(), waiting)?;
            }
            Command::Availability(ids, reply) => {
                let waiting = reply.map_or(Waiting::Quiet, Waiting::Reply);
                self.request(stream, RECEIVER, NS_RECEIVER, messages::app_availability(&ids), waiting)?;
            }
            Command::Launch(ids) => self.launch(stream, ids)?,
            Command::Join => {
                self.request(stream, RECEIVER, NS_RECEIVER, messages::get_status(), Waiting::Quiet)?;
                self.pending_join = true;
            }
            // A stop also ends a load that still waits for its app: the
            // item must not start on the device seconds after the stop.
            Command::StopApp => {
                self.drop_pending_load();
                match self.status.app.clone() {
                    Some(app) => {
                        self.request(stream, RECEIVER, NS_RECEIVER, messages::stop_app(&app.session_id), Waiting::Quiet)?;
                    }
                    None => self.fail("no app runs"),
                }
            }
            Command::SetVolume(level) => {
                self.request(stream, RECEIVER, NS_RECEIVER, messages::set_volume(level), Waiting::Quiet)?;
            }
            Command::SetMuted(muted) => {
                self.request(stream, RECEIVER, NS_RECEIVER, messages::set_muted(muted), Waiting::Quiet)?;
            }
            Command::Load(request, load) => self.load(stream, load, *request)?,
            Command::Play => self.media_command(stream, jellyfin::unpause, messages::play)?,
            Command::Pause => self.media_command(stream, jellyfin::pause, messages::pause)?,
            Command::StopMedia => {
                // With a load that waits for its app there is nothing to
                // stop yet, and the stop is the end of that load.
                let waited = self.pending_load.is_some();
                self.drop_pending_load();
                if !waited || self.app_transport().is_some() {
                    self.media_command(stream, jellyfin::stop, messages::media_stop)?
                }
            }
            Command::Seek(secs) => {
                if self.seek_in_flight.is_some() {
                    self.seek_queued = Some(secs);
                } else {
                    self.seek(stream, secs)?;
                }
            }
            Command::MediaStatus => {
                if let Some(transport) = self.app_transport() {
                    self.request(stream, &transport, NS_MEDIA, messages::get_status(), Waiting::Quiet)?;
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
                    self.request(stream, &transport, NS_MEDIA, messages::edit_tracks(session, &active), Waiting::Quiet)?;
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
        self.request(stream, RECEIVER, NS_RECEIVER, messages::launch(&first), Waiting::Launch { rest: ids })?;
        Ok(())
    }

    /// Loads now when the right app runs, else launches it first.
    fn load(&mut self, stream: &mut Stream, load: u64, request: LoadRequest) -> Result<()> {
        // A load that still waits for its app gives way to this one.
        self.drop_pending_load();
        let app = self.status.app.clone().filter(|_| self.transport.is_some());
        let jellyfin_runs = app.as_ref().is_some_and(|app| jellyfin::is_jellyfin_app(&app.app_id));
        let default_runs = app.as_ref().is_some_and(|app| app.app_id == messages::APP_DEFAULT_MEDIA);
        if jellyfin_runs && request.jellyfin.is_some() || default_runs && request.media.is_some() {
            return self.load_now(stream, load, request);
        }
        let mut ids = Vec::new();
        if request.jellyfin.is_some() {
            ids.push(jellyfin::APP_STABLE.to_string());
        }
        if request.media.is_some() {
            ids.push(messages::APP_DEFAULT_MEDIA.to_string());
        }
        self.pending_load = Some((load, request));
        self.launch(stream, ids)
    }

    /// Sends the load to the app that runs. The item the app had before
    /// gives way to it.
    fn load_now(&mut self, stream: &mut Stream, load: u64, request: LoadRequest) -> Result<()> {
        // What stops the load says so, and the app hears its item is not
        // on the device.
        let stopped = if self.app_transport().is_none() {
            Some("no app runs")
        } else if self.status.playing_jellyfin_app() && request.jellyfin.is_none() {
            Some("the Jellyfin app runs, and the item has no Jellyfin form")
        } else if !self.status.playing_jellyfin_app() && request.media.is_none() {
            Some("the media receiver runs, and the item has no stream URL")
        } else {
            None
        };
        if let Some(text) = stopped {
            self.emit(Event::Ended { load });
            self.fail(text);
            return Ok(());
        }
        let transport = self.app_transport().unwrap_or_default();
        if let Some(item) = request.jellyfin.filter(|_| self.status.playing_jellyfin_app()) {
            self.identity = Some(item.identity.clone());
            let payload = jellyfin::play_now(
                &item.identity,
                &self.device.name,
                &item.items,
                item.start_secs,
                item.audio_index,
                item.subtitle_index,
            );
            self.send(stream, &transport, jellyfin::NAMESPACE, payload)?;
            self.status.item_id = item.items.first().map(|item| item.id.clone());
        } else if let Some(media) = request.media {
            self.request(stream, &transport, NS_MEDIA, messages::load(&media), Waiting::Quiet)?;
            self.status.content_id = Some(media.url);
        }
        self.app_load = Some(load);
        self.status.player_state = Some(PlayerState::Buffering);
        self.publish();
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
            self.request(stream, &transport, NS_MEDIA, messages::seek(session, secs), Waiting::Seek)?;
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
        log::trace!("cast < {} {} {}", message.source, message.namespace, without_secrets(&payload));
        // The answer to a request of ours.
        let pending = messages::request_id(&payload).and_then(|id| self.pending.remove(&id));
        if let Some(pending) = &pending {
            self.status.rtt_ms = Some(pending.sent.elapsed().as_secs_f64() * 1000.);
        }
        let error = messages::error_text(&payload);
        // The device did what a request asked: the error of a command
        // before it is old news, and the panel stops showing it.
        if pending.is_some() && error.is_none() && self.status.error.take().is_some() {
            self.publish();
        }
        if let Some(text) = error {
            // The load that waits for the app goes with the launch that
            // failed, and with no other error: a refused seek or volume
            // says nothing about it.
            let of_launch = matches!(pending, Some(Pending { waiting: Waiting::Launch { .. }, .. }));
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
            if of_launch {
                self.drop_pending_load();
            }
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
                    // The panel hears of it now, not at the next status.
                    self.publish();
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
                    // The app plays: it has no answer with an id to clear
                    // an old error with.
                    self.status.error = None;
                    self.status.position = report.position;
                    self.status.position_at = Some(Instant::now());
                    self.status.player_state = Some(match report.kind.as_str() {
                        "playbackstop" => PlayerState::Idle,
                        _ if report.paused => PlayerState::Paused,
                        _ => PlayerState::Playing,
                    });
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
                    // Another app than the one that had our item: the item
                    // went with that one. The same app after a reconnect
                    // still has it.
                    if self.our_session.as_deref() != Some(&app.session_id) {
                        self.end_app_load();
                    }
                    self.send(stream, &app.transport_id, NS_CONNECTION, messages::connect())?;
                    self.transport = Some(app.transport_id.clone());
                    self.our_session = Some(app.session_id.clone());
                    self.status.media_session_id = None;
                    self.status.player_state = None;
                    self.status.idle_reason = None;
                    let speaks_media = app.speaks(NS_MEDIA);
                    self.status.app = Some(app.clone());
                    if speaks_media {
                        self.request(stream, &app.transport_id, NS_MEDIA, messages::get_status(), Waiting::Quiet)?;
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
                    if let Some((load, request)) = self.pending_load.take() {
                        self.load_now(stream, load, request)?;
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
                    self.drop_pending_load();
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
        self.end_app_load();
        self.transport = None;
        self.our_session = None;
        self.identity = None;
        self.status.app = None;
        self.status.media_session_id = None;
        self.status.player_state = None;
        self.status.content_id = None;
        self.status.item_id = None;
        // The length is of the item that is gone: the next one must not
        // show it until its own report comes.
        self.status.duration = None;
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
                self.status.player_state = Some(entry.player_state);
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
                if entry.player_state == PlayerState::Idle && entry.media.is_none() {
                    self.status.media_session_id = None;
                }
            }
            None => {
                self.status.media_session_id = None;
                self.status.player_state = Some(PlayerState::Idle);
            }
        }
        self.publish();
    }
}

/// A message for the log, with the access token of the server taken out:
/// the Jellyfin receiver gets it as `accessToken`, and a stream address
/// carries it as `ApiKey`.
fn without_secrets(payload: &Value) -> Value {
    match payload {
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| {
                let value = if key.eq_ignore_ascii_case("accessToken") {
                    Value::from("***")
                } else {
                    without_secrets(value)
                };
                (key.clone(), value)
            })
            .collect::<serde_json::Map<_, _>>()
            .into(),
        Value::Array(items) => items.iter().map(without_secrets).collect::<Vec<_>>().into(),
        Value::String(text) => jellyfin::redact(text).into(),
        other => other.clone(),
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
                Err(rustls::Error::General(OTHER_CERTIFICATE.into()))
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
            answer: Duration::from_secs(2),
            launch: Duration::from_secs(2),
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
            s.item_id.as_deref() == Some("i1") && s.player_state == Some(PlayerState::Playing)
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
        until(&session, &events, &mut log, "pause", 2., |s| s.player_state == Some(PlayerState::Paused));
        session.play();
        until(&session, &events, &mut log, "play", 2., |s| s.player_state == Some(PlayerState::Playing));
        session.seek(45.);
        let status = until(&session, &events, &mut log, "seek", 2., |s| {
            let seeks = mock.payloads(jellyfin::NAMESPACE).iter().filter(|p| p.1["command"] == "Seek").count();
            seeks == 1 && s.position == 45.
        });
        assert_eq!(status.player_state, Some(PlayerState::Playing));
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
            s.player_state == Some(PlayerState::Playing) && s.media_session_id == Some(1)
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
        until(&session, &events, &mut log, "media pause", 2., |s| s.player_state == Some(PlayerState::Paused));
        let pause = mock.payloads(NS_MEDIA).into_iter().find(|p| p.1["type"] == "PAUSE").unwrap();
        assert_eq!(pause.1["mediaSessionId"], 1);
        session.play();
        until(&session, &events, &mut log, "media play", 2., |s| s.player_state == Some(PlayerState::Playing));

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
        assert_eq!(status.player_state, Some(PlayerState::Playing));
        mock.set(|m| m.seek_delay = Duration::ZERO);

        session.set_subtitle(Some(2));
        until(&session, &events, &mut log, "subtitle", 2., |s| s.active_tracks == [2]);
        let tracks = mock.payloads(NS_MEDIA).into_iter().find(|p| p.1["type"] == "EDIT_TRACKS_INFO").unwrap();
        assert_eq!(tracks.1["activeTrackIds"], json!([2]));

        session.stop_media();
        until(&session, &events, &mut log, "media stop", 2., |s| s.player_state == Some(PlayerState::Idle) && s.idle_reason.as_deref() == Some("CANCELLED"));

        // The whole run went over one connection.
        assert_eq!(mock.state.lock().unwrap().connections, 1);
        drop(session);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(mock.seen().last().unwrap().1, "CLOSE");
    }

    #[test]
    fn connects_again_when_the_device_goes_silent() {
        let mock = Mock::start();
        // The silence is far above a ping, so a runner that stalls for a
        // moment (a CI machine, 2026-11-13: 400 ms was not enough) does not
        // count as a silent device.
        let short = Timing {
            ping: Duration::from_millis(50),
            silence: Duration::from_millis(1500),
            answer: Duration::from_secs(1),
            launch: Duration::from_secs(1),
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
        until(&session, &events, &mut log, "closed", 5., |s| !s.connected);
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
            answer: Duration::from_secs(1),
            launch: Duration::from_secs(1),
        }
    }

    /// The media receiver with an item, over a connection with short times.
    fn media_receiver_playing(mock: &Mock, timing: Timing) -> (Session, async_channel::Receiver<Event>, Vec<Event>) {
        mock.set(|m| m.fails = vec![jellyfin::APP_STABLE.into()]);
        let (session, events) = Session::connect_with(mock.device(), timing);
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        session.load(request());
        until(&session, &events, &mut log, "media load", 5., |s| s.player_state == Some(PlayerState::Playing) && s.media_session_id == Some(1));
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
            s.player_state == Some(PlayerState::Paused) && s.media_session_id == Some(1) && s.position == 299.
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
        assert_eq!((device.state, device.position), (PlayerState::Paused, 299.));
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
            s.connected && s.player_state == Some(PlayerState::Paused) && s.media_session_id == Some(1) && s.position == 299.
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
        assert_eq!((device.state, device.position), (PlayerState::Paused, 299.));
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
            s.player_state == Some(PlayerState::Paused) && s.media_session_id == Some(1) && s.position == 299.
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
        assert_eq!((status.app, status.media_session_id, status.player_state), (None, None, None), "{log:?}");
        assert_eq!(media_count(&mock, "SEEK") + media_count(&mock, "PAUSE") - sent_before, 0, "{:?}", mock.seen());
        session.load(request());
        until(&session, &events, &mut log, "our app again", 5., |s| {
            s.app.as_ref().is_some_and(|app| app.app_id == messages::APP_DEFAULT_MEDIA)
                && s.player_state == Some(PlayerState::Playing)
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
            s.player_state == Some(PlayerState::Playing) && s.media_session_id == Some(1)
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

    /// The numbers of the loads whose end the device told.
    fn ended(log: &[Event]) -> Vec<u64> {
        log.iter()
            .filter_map(|e| match e {
                Event::Ended { load } => Some(*load),
                _ => None,
            })
            .collect()
    }

    /// The end of an item is told with the number of its load. A new item
    /// that makes the device change its app ends the item before it, not
    /// itself: the app before goes while the new one comes up. The item
    /// goes when its own app does.
    #[test]
    fn the_end_of_an_item_names_its_load() {
        let mock = Mock::start();
        let (session, events) = Session::connect_with(mock.device(), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        // Statuses with no app, before any load: nothing ended.
        session.get_status().recv_timeout(Duration::from_secs(2)).unwrap();
        let first = session.load(request());
        until(&session, &events, &mut log, "playing", 6., |s| s.player_state == Some(PlayerState::Playing));
        assert_eq!(ended(&log), [0u64; 0], "{log:?}");
        // The next item has no Jellyfin form: the media receiver takes the
        // place of the Jellyfin app.
        let second = session.load(LoadRequest { jellyfin: None, ..request() });
        assert_eq!((first, second), (1, 2));
        until(&session, &events, &mut log, "media load", 6., |s| s.player_state == Some(PlayerState::Playing) && s.media_session_id == Some(1));
        drain(&events, &mut log);
        assert_eq!(ended(&log), [first], "the change of the app ended another load than the one before: {log:?}");
        // The app of the second item goes: that is its end.
        session.stop_app();
        until(&session, &events, &mut log, "stopped", 3., |s| s.app.is_none());
        drain(&events, &mut log);
        assert_eq!(ended(&log), [first, second], "{log:?}");
    }

    /// A load whose apps do not launch is told as ended: the app does not
    /// keep an item that never came to the device.
    #[test]
    fn a_load_that_never_reaches_an_app_is_told_as_ended() {
        let mock = Mock::start();
        mock.set(|m| m.fails = vec![jellyfin::APP_STABLE.into(), messages::APP_DEFAULT_MEDIA.into()]);
        let (session, events) = Session::connect_with(mock.device(), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        let load = session.load(request());
        until(&session, &events, &mut log, "launch error", 5., |s| s.error.is_some());
        drain(&events, &mut log);
        assert_eq!(ended(&log), [load], "{log:?}");
    }

    /// The certificate of the first connection is told, for the app to
    /// keep. A connection that is given it takes the device that shows it
    /// with no word, and refuses a device that shows another one: the
    /// trust of the first use holds across runs of the app.
    #[test]
    fn the_kept_certificate_is_told_once_and_holds_at_the_next_connection() {
        let mock = Mock::start();
        let pinned = |log: &[Event]| -> Vec<Vec<u8>> {
            log.iter()
                .filter_map(|e| match e {
                    Event::Pinned(certificate) => Some(certificate.clone()),
                    _ => None,
                })
                .collect()
        };
        // A first connection: the certificate the device shows is told.
        let (session, events) = Session::start(mock.device(), None, timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        drain(&events, &mut log);
        let told = pinned(&log);
        assert_eq!(told, [crate::chromecast::mock::CERT.to_vec()]);
        drop(session);
        // The next run gives it back: connected, and nothing new to keep.
        let (session, events) = Session::start(mock.device(), Some(told[0].clone()), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect with the kept one", 5., |s| s.connected);
        drain(&events, &mut log);
        assert!(pinned(&log).is_empty(), "{log:?}");
        drop(session);
        // Another certificate was kept for this id: the device is refused.
        let (session, events) = Session::start(mock.device(), Some(b"another certificate".to_vec()), timing());
        let mut log = Vec::new();
        let status = until(&session, &events, &mut log, "the refusal", 5., |s| s.error.is_some());
        assert!(!status.connected);
        assert!(status.error.as_deref().is_some_and(|e| e.contains(OTHER_CERTIFICATE)), "{:?}", status.error);
        assert!(pinned(&log).is_empty());
    }

    /// A device that cannot be reached: the status says why, so the panel
    /// can. Before, only an event carried the reason and the panel said
    /// "Connecting…" for ever.
    #[test]
    fn a_connect_that_fails_is_in_the_status() {
        // A port nobody listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let device = Device { port, ..Mock::start().device() };
        let (session, events) = Session::connect_with(device, timing());
        let mut log = Vec::new();
        let status = until(&session, &events, &mut log, "the error", 5., |s| s.error.is_some());
        assert!(!status.connected);
        assert!(status.error.as_deref().is_some_and(|e| e.contains("connect")), "{:?}", status.error);
    }

    /// The error of a command stays until the device does what a later
    /// request asks; then it is gone from the status. Before, it stayed
    /// for as long as the socket.
    #[test]
    fn an_error_goes_when_a_later_request_is_answered() {
        let mock = Mock::start();
        let (session, events) = Session::connect_with(mock.device(), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected && s.volume == 0.5);
        session.pause();
        until(&session, &events, &mut log, "the error", 2., |s| s.error.as_deref() == Some("no app runs"));
        session.set_volume(0.3);
        let status = until(&session, &events, &mut log, "volume", 2., |s| s.volume == 0.3);
        assert_eq!(status.error, None);
    }

    /// A stop sent right before the handle is dropped still reaches the
    /// device: the close waits behind it.
    #[test]
    fn a_stop_right_before_the_drop_goes_out() {
        let mock = Mock::start();
        let (session, events, mut log) = media_receiver_playing(&mock, timing());
        drain(&events, &mut log);
        session.stop_media();
        drop(session);
        let deadline = Instant::now() + Duration::from_secs(2);
        while media_count(&mock, "STOP") == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(media_count(&mock, "STOP"), 1, "{:?}", mock.seen());
    }

    /// A command does not wait for a read of the socket to time out: it
    /// is written the moment it comes. Ten round trips to the mock on the
    /// loopback stay far under the 100 ms each that a read waited before.
    #[test]
    fn a_command_goes_out_at_once() {
        let mock = Mock::start();
        let (session, events) = Session::connect_with(mock.device(), Timing::default());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected && s.volume == 0.5);
        let mut total = Duration::ZERO;
        for n in 0..10 {
            thread::sleep(Duration::from_millis(20 + n * 7));
            let started = Instant::now();
            session.get_status().recv_timeout(Duration::from_secs(2)).unwrap();
            total += started.elapsed();
        }
        assert!(total < Duration::from_millis(300), "ten commands took {total:?}");
    }

    /// A stop while the app of a load still launches ends the load: the
    /// item does not start on the device after the stop. The Jellyfin app
    /// hangs here, the media receiver comes up as the next app, and
    /// nothing is loaded into it. Before, the stop found "no app" and the
    /// load stayed, to play seconds later.
    #[test]
    fn a_stop_during_the_launch_ends_the_load() {
        let mock = Mock::start();
        let (session, events) = Session::connect_with(mock.device(), timing());
        let mut log = Vec::new();
        until(&session, &events, &mut log, "connect", 5., |s| s.connected);
        mock.set(|m| m.ignores = vec![jellyfin::APP_STABLE.into()]);
        session.load(request());
        thread::sleep(Duration::from_millis(100));
        session.stop_media();
        // The launch of the first app is given up and the next app comes.
        until(&session, &events, &mut log, "the next app", 6., |s| s.app.is_some());
        thread::sleep(Duration::from_millis(500));
        assert!(mock.state.lock().unwrap().media.is_none(), "the item plays after the stop");
        assert_eq!(media_count(&mock, "LOAD"), 0);
    }

    /// A seek while the media receiver is paused leaves it paused, as the
    /// Cast protocol does when `resumeState` is not given.
    #[test]
    fn a_seek_while_paused_keeps_the_media_receiver_paused() {
        let mock = Mock::start();
        let (session, events, mut log) = media_receiver_playing(&mock, timing());
        session.pause();
        until(&session, &events, &mut log, "pause", 2., |s| s.player_state == Some(PlayerState::Paused));
        session.seek(100.);
        until(&session, &events, &mut log, "seek", 2., |_| media_count(&mock, "SEEK") == 1);
        thread::sleep(Duration::from_millis(200));
        session.media_status();
        let status = until(&session, &events, &mut log, "status", 2., |s| s.position == 100.);
        let device = mock.state.lock().unwrap().media.clone().unwrap();
        assert_eq!(device.state, PlayerState::Paused, "the device plays after a seek while paused");
        assert_eq!(status.player_state, Some(PlayerState::Paused));
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    /// A trace line of a cast message does not show the access token.
    #[test]
    fn a_logged_message_has_no_token() {
        let message = serde_json::json!({
            "type": "LOAD",
            "accessToken": "fake-token",
            "media": { "contentId": "https://jellyfin.example/Videos/i1/master.m3u8?ApiKey=fake-token&b=1" },
            "items": [{ "AccessToken": "fake-token" }],
        });
        let logged = without_secrets(&message).to_string();
        assert!(!logged.contains("fake-token"), "{logged}");
        assert!(logged.contains("master.m3u8?ApiKey=***&b=1"), "{logged}");
        assert!(logged.contains("LOAD"));
    }
}
