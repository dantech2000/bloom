// SPDX-License-Identifier: AGPL-3.0-or-later
//! The socket of the server. Over it come the commands of SyncPlay and news
//! such as "the library changed". One thread holds it open: it answers the
//! keep-alive of the server, and connects again when the socket is lost.
//!
//! The server knows a client by its name, its device id and its user. The
//! socket sends the same header as every request, so both are one session.

use std::{
    io::ErrorKind,
    net::TcpStream,
    sync::{
        Arc, mpsc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;
use tungstenite::{
    Connector, Message, WebSocket, client::IntoClientRequest, stream::MaybeTlsStream,
};

use crate::jellyfin::Client;

/// The server ends a session whose socket is silent for 60 seconds; it
/// counts only this message, and asks for it after 45 seconds.
const KEEP_ALIVE: Duration = Duration::from_secs(30);
/// With no word of the server for this long the socket counts as dead.
const SILENCE: Duration = Duration::from_secs(75);
/// How long one read waits; the loop looks at its timers between reads.
const READ_WAIT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub enum SocketEvent {
    /// The socket is open; `again` when it was open before in this run.
    Open { again: bool },
    Message { kind: String, data: Value },
    Closed,
}

/// Holds the socket open until it is dropped.
pub struct Realtime {
    stop: Arc<AtomicBool>,
    /// Messages for the server; the thread sends them in order.
    outbox: mpsc::Sender<String>,
}

impl Realtime {
    /// Sends a message to the server, such as a subscription. One queued
    /// while the socket is down is dropped when it opens again: the caller
    /// sends again on `Open`.
    pub fn send(&self, kind: &str, data: Option<Value>) {
        let message = match data {
            Some(data) => serde_json::json!({ "MessageType": kind, "Data": data }),
            None => serde_json::json!({ "MessageType": kind }),
        };
        let _ = self.outbox.send(message.to_string());
    }
}

impl Drop for Realtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Opens the socket of the session of `client` and keeps it open.
pub fn start(client: Client) -> (Realtime, async_channel::Receiver<SocketEvent>) {
    let (tx, rx) = async_channel::unbounded();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let (outbox, outgoing) = mpsc::channel::<String>();
    let _ = thread::Builder::new().name("server-socket".into()).spawn(move || {
        let (mut again, mut wait) = (false, Duration::from_secs(1));
        while !stopped.load(Ordering::Acquire) {
            match connect(&client) {
                Ok(socket) => {
                    wait = Duration::from_secs(1);
                    // Messages from before the socket opened are stale.
                    while outgoing.try_recv().is_ok() {}
                    if tx.send_blocking(SocketEvent::Open { again }).is_err() {
                        return;
                    }
                    again = true;
                    if let Err(err) = serve(socket, &tx, &outgoing, &stopped) {
                        log::info!("server socket closed: {err:#}");
                    }
                    let _ = tx.send_blocking(SocketEvent::Closed);
                }
                Err(err) => log::warn!("server socket: {err:#}"),
            }
            // Wait in small steps, so a drop ends the thread soon.
            let until = Instant::now() + wait;
            while Instant::now() < until && !stopped.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(100));
            }
            wait = (wait * 2).min(Duration::from_secs(30));
        }
    });
    (Realtime { stop, outbox }, rx)
}

fn connect(client: &Client) -> Result<Socket> {
    let base = client.base.trim_end_matches('/');
    let (address, secure) = match base.split_once("://") {
        Some(("https", rest)) => (format!("wss://{rest}/socket"), true),
        Some(("http", rest)) => (format!("ws://{rest}/socket"), false),
        _ => return Err(anyhow!("not a server address: {base}")),
    };
    let mut request = address.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", client.auth_header().parse()?);
    let uri = request.uri().clone();
    let host = uri.host().context("no host")?.to_string();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let stream = TcpStream::connect((host.as_str(), port)).context("connect")?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    let connector = if secure { Connector::Rustls(tls_config()?) } else { Connector::Plain };
    let (socket, _) = tungstenite::client_tls_with_config(request, stream, None, Some(connector))
        .map_err(|err| anyhow!("handshake: {err}"))?;
    // Reads return soon from here on, so the keep-alive stays on time.
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(READ_WAIT))?,
        MaybeTlsStream::Rustls(stream) => stream.get_ref().set_read_timeout(Some(READ_WAIT))?,
        _ => {}
    }
    Ok(socket)
}

/// The TLS setup ureq uses: rustls with the ring provider and the web roots.
/// Named here, so a second provider in the build cannot change it.
fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

fn serve(
    mut socket: Socket,
    tx: &async_channel::Sender<SocketEvent>,
    outgoing: &mpsc::Receiver<String>,
    stop: &AtomicBool,
) -> Result<()> {
    let mut keep_alive = KEEP_ALIVE;
    let mut last_sent = Instant::now();
    let mut last_heard = Instant::now();
    while !stop.load(Ordering::Acquire) {
        match socket.read() {
            Ok(Message::Text(text)) => {
                last_heard = Instant::now();
                let Ok(mut message) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                let kind = message["MessageType"].as_str().unwrap_or_default().to_string();
                let data = message["Data"].take();
                match kind.as_str() {
                    // The server names its limit in seconds; answer at half.
                    "ForceKeepAlive" => {
                        if let Some(limit) = data.as_f64().filter(|limit| *limit >= 2.) {
                            keep_alive = Duration::from_secs_f64(limit / 2.);
                        }
                        socket.send(Message::text(r#"{"MessageType":"KeepAlive"}"#))?;
                        last_sent = Instant::now();
                    }
                    "KeepAlive" => {}
                    _ => {
                        if tx.send_blocking(SocketEvent::Message { kind, data }).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => return Err(anyhow!("the server closed the socket")),
            Ok(_) => last_heard = Instant::now(),
            // No message in the time of one read.
            Err(tungstenite::Error::Io(err))
                if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(err) => return Err(err.into()),
        }
        while let Ok(text) = outgoing.try_recv() {
            socket.send(Message::text(text))?;
            last_sent = Instant::now();
        }
        if last_sent.elapsed() >= keep_alive {
            socket.send(Message::text(r#"{"MessageType":"KeepAlive"}"#))?;
            last_sent = Instant::now();
        }
        if last_heard.elapsed() >= SILENCE {
            return Err(anyhow!("no word of the server for {SILENCE:?}"));
        }
    }
    let _ = socket.close(None);
    Ok(())
}
