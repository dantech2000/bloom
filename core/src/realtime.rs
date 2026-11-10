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
/// The wait before the first try to connect again, and the longest one.
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(30);
/// A socket that stayed open this long was a good one: the next loss
/// starts the waits from the first again.
const STABLE: Duration = Duration::from_secs(10);

/// The wait before the next try. `wait` is the one the tries have come to
/// (it doubles with each); `open_for` is how long the socket was open,
/// none when it did not open. A socket that opens and is closed at once (a
/// proxy in front of a server that is down) is no success: the waits go on
/// to grow, or the app would knock every second for as long as that lasts.
fn wait_after(wait: Duration, open_for: Option<Duration>) -> Duration {
    if open_for.is_some_and(|open| open >= STABLE) { FIRST_WAIT } else { wait }
}

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
    /// sends again on `Open`. It goes out when the read that runs returns,
    /// up to `READ_WAIT` later.
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
        let (mut again, mut wait) = (false, FIRST_WAIT);
        while !stopped.load(Ordering::Acquire) {
            let mut open_for = None;
            match connect(&client) {
                Ok(socket) => {
                    let opened = Instant::now();
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
                    open_for = Some(opened.elapsed());
                }
                Err(err) => log::warn!("server socket: {err:#}"),
            }
            wait = wait_after(wait, open_for);
            // Wait in small steps, so a drop ends the thread soon.
            let until = Instant::now() + wait;
            while Instant::now() < until && !stopped.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(100));
            }
            wait = (wait * 2).min(LONGEST_WAIT);
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
    // With a limit: the system waits over a minute for a host that does
    // not answer, and the thread cannot see a stop meanwhile.
    use std::net::ToSocketAddrs as _;
    // Each address of the name in turn, as `TcpStream::connect` does: the
    // first can be one this Mac does not reach (IPv6 with no route).
    let mut stream = Err(anyhow!("the name has no address"));
    for address in (host.as_str(), port).to_socket_addrs().context("resolve")? {
        stream = TcpStream::connect_timeout(&address, Duration::from_secs(15)).context("connect");
        if stream.is_ok() {
            break;
        }
    }
    let stream = stream?;
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
                        // A number of seconds a day can hold: a huge one
                        // would be a panic in `from_secs_f64`.
                        if let Some(limit) = data.as_f64().filter(|limit| (2. ..=86_400.).contains(limit)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket that is closed as soon as it opens does not start the
    /// waits again: they grow as for a server that does not answer. One
    /// that stayed open does.
    #[test]
    fn a_socket_closed_at_once_does_not_reset_the_wait() {
        let mut wait = FIRST_WAIT;
        let mut waits = Vec::new();
        for _ in 0..7 {
            wait = wait_after(wait, Some(Duration::from_millis(40)));
            waits.push(wait.as_secs());
            wait = (wait * 2).min(LONGEST_WAIT);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(wait_after(wait, None), LONGEST_WAIT);
        assert_eq!(wait_after(wait, Some(STABLE)), FIRST_WAIT);
    }
}
