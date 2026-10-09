// SPDX-License-Identifier: AGPL-3.0-or-later
//! The socket of the stream and its readers.

use std::{
    io::Write as _,
    os::unix::net::{UnixListener, UnixStream},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};

use super::{Snapshot, VERSION};

/// True when the program at the other end closed the socket. What it sent
/// is thrown away: the stream is read-only.
fn closed(stream: &mut UnixStream) -> bool {
    use std::io::Read as _;
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut sent = [0u8; 256];
    let gone = loop {
        match stream.read(&mut sent) {
            Ok(0) => break true,
            Ok(_) => {}
            Err(err) => break err.kind() != std::io::ErrorKind::WouldBlock,
        }
    };
    gone || stream.set_nonblocking(false).is_err()
}

/// Lines a reader may be behind before it is closed.
const QUEUE: usize = 256;

/// The listener of a hub, while the stream is on.
struct Listening {
    path: String,
    stop: Arc<AtomicBool>,
}

/// One reader, as the hub holds it.
struct Reader {
    queue: mpsc::SyncSender<Arc<str>>,
    /// False once its program left or the hub closed it.
    there: Arc<AtomicBool>,
    /// The same socket as its thread writes to, to close it from here.
    socket: Option<UnixStream>,
}

impl Reader {
    /// Ends the reader: its thread writes no further line, and a write
    /// that waits on a program that does not read is broken off.
    fn close(&self) {
        self.there.store(false, Ordering::Relaxed);
        if let Some(socket) = &self.socket {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Takes the events of the app and gives each reader its lines.
pub struct Hub {
    on: AtomicBool,
    seq: AtomicU64,
    instance: String,
    /// The queue of each reader, and whether its program is still there.
    readers: Mutex<Vec<Reader>>,
    /// What plays now, for a reader that connects in the middle of it.
    state: Mutex<Value>,
    listening: Mutex<Option<Listening>>,
    /// Readers closed because they fell behind.
    pub closed_slow: AtomicU64,
}

/// The hub of the app.
pub fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub::new(std::env::var("BLOOM_TEST_NAME").unwrap_or_else(|_| "default".into())))
}

impl Hub {
    pub fn new(instance: String) -> Self {
        Self {
            on: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            instance,
            readers: Mutex::new(Vec::new()),
            state: Mutex::new(Snapshot::default().state_json(0, false)),
            listening: Mutex::new(None),
            closed_slow: AtomicU64::new(0),
        }
    }

    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// The programs that read now. One that went away is not counted, also
    /// when no event has gone out since.
    pub fn readers(&self) -> usize {
        let mut readers = self.readers.lock().unwrap();
        readers.retain(|reader| reader.there.load(Ordering::Relaxed));
        readers.len()
    }

    /// One line. An event has the next number; the two lines that greet a
    /// reader have the number of the last event, so the numbers of the
    /// events have no gap when another reader connects.
    fn line(&self, event: &str, data: Value, counts: bool) -> Arc<str> {
        let line = json!({
            "v": VERSION,
            "seq": if counts { self.seq.fetch_add(1, Ordering::Relaxed) + 1 } else { self.seq.load(Ordering::Relaxed) },
            "time": jiff::Timestamp::now().to_string(),
            "event": event,
            "instance": self.instance,
            "data": data,
        });
        Arc::from(format!("{line}\n"))
    }

    /// Sends an event to every reader. Never waits: a reader whose queue
    /// is full is closed, and its program sees the end of the stream.
    pub fn emit(&self, event: &str, data: Value) {
        if !self.on() {
            return;
        }
        // The number is given under the lock, so the lines of two threads
        // reach a reader in the order of their numbers.
        let mut readers = self.readers.lock().unwrap();
        let line = self.line(event, data, true);
        readers.retain(|reader| match reader.queue.try_send(line.clone()) {
            Ok(()) => reader.there.load(Ordering::Relaxed),
            Err(mpsc::TrySendError::Full(_)) => {
                self.closed_slow.fetch_add(1, Ordering::Relaxed);
                // Its socket closes, and the lines that still wait in
                // its queue are not written.
                reader.close();
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        });
    }

    /// Keeps what plays now for the readers that connect later.
    pub fn set_state(&self, state: Value) {
        *self.state.lock().unwrap() = state;
    }

    /// Opens the socket and takes readers. The socket is for the owner
    /// only, or the stream stays off.
    pub fn listen(&'static self, path: String) -> std::io::Result<()> {
        self.listen_with(path, Snapshot::default().state_json(0, false))
    }

    /// As [`Hub::listen`], with what plays now: it is there before the
    /// socket is, so the first reader of the new socket is told of it.
    pub fn listen_with(&'static self, path: String, state: Value) -> std::io::Result<()> {
        self.stop();
        self.set_state(state);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(err) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
                let _ = std::fs::remove_file(&path);
                return Err(err);
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        // On before the thread takes a reader: `serve` turns a reader away
        // while the stream is off.
        self.on.store(true, Ordering::Relaxed);
        let spawned = thread::Builder::new().name("events".into()).spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                match stream {
                    Ok(stream) => self.serve(stream, &stopped),
                    Err(_) => thread::sleep(Duration::from_millis(200)),
                }
            }
        });
        if let Err(err) = spawned {
            self.on.store(false, Ordering::Relaxed);
            let _ = std::fs::remove_file(&path);
            return Err(err);
        }
        *self.listening.lock().unwrap() = Some(Listening { path, stop });
        Ok(())
    }

    /// One reader: a first line that says what this is, a second with what
    /// plays now, then the events as they come.
    fn serve(&self, mut stream: UnixStream, stopped: &AtomicBool) {
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let (tx, rx) = mpsc::sync_channel::<Arc<str>>(QUEUE);
        let there = Arc::new(AtomicBool::new(true));
        // The state and the place in the list under one lock: an event
        // that comes now is in the state, or it reaches this reader.
        let (hello, state) = {
            let mut readers = self.readers.lock().unwrap();
            // The stream went off between the accept and here: `stop` has
            // emptied the list, and this reader must not come after it,
            // also when the stream is on again with a new listener.
            if !self.on() || stopped.load(Ordering::Acquire) {
                return;
            }
            let hello = self.line(
                "hello",
                json!({ "app": crate::brand::NAME, "version": crate::config::APP_VERSION, "format": VERSION }),
                false,
            );
            let state = self.line("state", self.state.lock().unwrap().clone(), false);
            readers.push(Reader { queue: tx, there: there.clone(), socket: stream.try_clone().ok() });
            (hello, state)
        };
        let _ = thread::Builder::new().name("events-reader".into()).spawn(move || {
            /// Says that the reader is gone when its thread ends.
            struct Gone(Arc<AtomicBool>);
            impl Drop for Gone {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Relaxed);
                }
            }
            let _gone = Gone(there.clone());
            if stream.write_all(hello.as_bytes()).is_err() || stream.write_all(state.as_bytes()).is_err() {
                return;
            }
            // Ends when the hub drops the sender (the stream goes off, or
            // this reader fell behind), or the program went away: seen at
            // a write, and looked for once a second while no event comes.
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    // A reader the hub closed gets no more of its queue.
                    Ok(line) if there.load(Ordering::Relaxed) && stream.write_all(line.as_bytes()).is_ok() => {}
                    Err(mpsc::RecvTimeoutError::Timeout) if !closed(&mut stream) => {}
                    _ => return,
                }
            }
        });
    }

    /// Closes the socket and every reader.
    pub fn stop(&self) {
        self.on.store(false, Ordering::Relaxed);
        if let Some(listening) = self.listening.lock().unwrap().take() {
            listening.stop.store(true, Ordering::Release);
            // The thread waits in `accept`; a connection wakes it.
            let _ = UnixStream::connect(&listening.path);
            let _ = std::fs::remove_file(&listening.path);
        }
        let mut readers = self.readers.lock().unwrap();
        for reader in readers.iter() {
            reader.close();
        }
        readers.clear();
        // What played is not told to a reader of a later stream: the
        // setting for titles can have changed in between.
        self.set_state(Snapshot::default().state_json(0, false));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead as _, BufReader},
        time::Instant,
    };

    use super::*;

    fn socket(name: &str) -> String {
        format!("{}/bloom-events-test-{name}-{}.sock", std::env::temp_dir().display(), std::process::id())
    }

    fn reader(path: &str) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(path).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        BufReader::new(stream)
    }

    fn next(reader: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|err| panic!("{err}: {line:?}"))
    }

    /// A reader gets a line that says what the stream is, a line with what
    /// plays now, and then each event, numbered. The socket is for the
    /// owner alone. Off, there is no socket and nothing is sent.
    #[test]
    fn a_reader_gets_hello_the_state_and_the_events() {
        let hub: &'static Hub = Box::leak(Box::new(Hub::new("test".into())));
        let path = socket("stream");
        hub.emit("playback.paused", json!({}));
        hub.listen(path.clone()).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        hub.set_state(json!({ "playback": { "session": 3 } }));
        let mut one = reader(&path);
        let hello = next(&mut one);
        assert_eq!((&hello["event"], &hello["v"], &hello["instance"]), (&json!("hello"), &json!(VERSION), &json!("test")));
        assert_eq!(next(&mut one)["data"]["playback"]["session"], 3);
        // The reader is in the list once its two lines are on their way.
        let deadline = Instant::now() + Duration::from_secs(5);
        while hub.readers() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        hub.emit("playback.paused", json!({ "session": 3 }));
        hub.emit("playback.resumed", json!({ "session": 3 }));
        let (paused, resumed) = (next(&mut one), next(&mut one));
        assert_eq!((&paused["event"], &resumed["event"]), (&json!("playback.paused"), &json!("playback.resumed")));
        assert_eq!(resumed["seq"].as_u64().unwrap(), paused["seq"].as_u64().unwrap() + 1);
        hub.stop();
        assert!(!std::path::Path::new(&path).exists());
        let mut rest = String::new();
        assert_eq!(one.read_line(&mut rest).unwrap(), 0, "the stream did not end: {rest:?}");
    }

    /// A reader that does not read holds nothing up: the events go on at
    /// once, and the reader is closed when it is a whole queue behind.
    /// A second reader takes no number from the events of the first, and
    /// a reader that went away is not counted, with no event in between.
    #[test]
    fn the_numbers_have_no_gap_and_a_reader_that_left_is_not_counted() {
        let path = socket("gap");
        let hub: &'static Hub = Box::leak(Box::new(Hub::new("test".into())));
        hub.listen(path.clone()).unwrap();
        let mut first = reader(&path);
        let (hello, state) = (next(&mut first), next(&mut first));
        assert_eq!((hello["event"].as_str(), hello["seq"].as_u64(), state["seq"].as_u64()), (Some("hello"), Some(0), Some(0)));
        hub.emit("one", json!({}));
        assert_eq!(next(&mut first)["seq"], 1);
        let mut second = reader(&path);
        assert_eq!((next(&mut second)["seq"].as_u64(), next(&mut second)["seq"].as_u64()), (Some(1), Some(1)));
        hub.emit("two", json!({}));
        assert_eq!((next(&mut first)["seq"].as_u64(), next(&mut second)["seq"].as_u64()), (Some(2), Some(2)));
        assert_eq!(hub.readers(), 2);
        drop(second);
        let until = Instant::now() + Duration::from_secs(5);
        while hub.readers() != 1 && Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(hub.readers(), 1, "the reader that left is still counted");
        hub.stop();
    }

    #[test]
    fn a_reader_that_does_not_read_is_closed_and_holds_nothing_up() {
        let hub: &'static Hub = Box::leak(Box::new(Hub::new("test".into())));
        let path = socket("slow");
        hub.listen(path.clone()).unwrap();
        let _silent = UnixStream::connect(&path).unwrap();
        // Set now: a socket the other end has closed takes no option.
        _silent.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while hub.readers() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        // Far more than the queue and the buffer of the socket hold.
        let padding = "x".repeat(2000);
        let started = Instant::now();
        for _ in 0..4000 {
            hub.emit("playback.seeked", json!({ "padding": padding }));
        }
        let took = started.elapsed();
        assert_eq!(hub.readers(), 0, "the reader that does not read is still in the list");
        assert_eq!(hub.closed_slow.load(Ordering::Relaxed), 1);
        assert!(took < Duration::from_secs(2), "the events waited for the reader: {took:?}");
        // Its socket is closed, and the lines that waited in its queue do
        // not come: far fewer than the 256 of a queue arrive before the end.
        let mut silent = BufReader::new(_silent);
        let mut lines = 0;
        let mut line = String::new();
        let ended = loop {
            line.clear();
            match silent.read_line(&mut line) {
                Ok(0) => break true,
                Ok(_) => lines += 1,
                Err(_) => break false,
            }
        };
        assert!(ended, "the socket of the closed reader stays open");
        assert!(lines < 200, "the closed reader still got {lines} lines");
        // What played is not kept over a stop: a later reader is told nothing of it.
        hub.set_state(json!({ "playback": { "item": { "name": "A title" } } }));
        hub.stop();
        hub.listen(path.clone()).unwrap();
        let mut later = reader(&path);
        next(&mut later);
        assert_eq!(next(&mut later)["data"]["playback"], Value::Null);
        // A stream that is opened while a film plays tells its first
        // reader of the film.
        hub.listen_with(path.clone(), json!({ "playback": { "paused": true } })).unwrap();
        let mut first = reader(&path);
        next(&mut first);
        assert_eq!(next(&mut first)["data"]["playback"]["paused"], true);
        hub.stop();
    }
}
