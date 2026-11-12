// SPDX-License-Identifier: AGPL-3.0-or-later
//! For the test servers of this crate.

use std::{
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The connections to the listener of a test server, until `stop` is set.
/// The listener does not block: the loop looks at `stop` between two
/// tries, so it ends with no last connection. A server that waited for one
/// to stop hung its test when that connect failed (seen on 2026-10-12: ten
/// tests at once, for good).
pub(crate) fn connections(listener: TcpListener, stop: Arc<AtomicBool>) -> impl Iterator<Item = TcpStream> {
    listener.set_nonblocking(true).expect("nonblocking");
    std::iter::from_fn(move || {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                // A stream takes the mode of its listener on macOS.
                Ok((stream, _)) if stream.set_nonblocking(false).is_ok() => return Some(stream),
                _ => std::thread::sleep(std::time::Duration::from_micros(500)),
            }
        }
        None
    })
}

/// A server for a test that answers each request by its method and path,
/// and keeps the paths it saw. It ends with the test.
pub(crate) struct Routes {
    pub url: String,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Routes {
    pub(crate) fn start(answer: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
        use std::io::{Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, stopping, answer) = (seen.clone(), stop.clone(), Arc::new(answer));
        let thread = std::thread::spawn(move || {
            // The requests of a page come at the same time: one thread each.
            std::thread::scope(|scope| {
                for mut stream in connections(listener, stopping) {
                    let (log, answer) = (log.clone(), answer.clone());
                    scope.spawn(move || {
                        let mut buffer = [0u8; 8192];
                        let n = stream.read(&mut buffer).unwrap_or(0);
                        let head = String::from_utf8_lossy(&buffer[..n]).to_string();
                        let mut words = head.split(' ');
                        let (method, path) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
                        let (status, body) = answer(method, path);
                        log.lock().unwrap().push(path.to_string());
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.flush();
                    });
                }
            });
        });
        Self { url, seen, stop, thread: Some(thread) }
    }

    /// How many requests had a path with this part.
    pub(crate) fn count(&self, path_part: &str) -> usize {
        self.seen.lock().unwrap().iter().filter(|path| path.contains(path_part)).count()
    }
}

impl Drop for Routes {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
