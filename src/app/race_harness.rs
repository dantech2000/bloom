// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harness of the race tests: the real [`Bloom`] in a test window of
//! GPUI, signed in to a small HTTP server on a thread of its own. The test
//! scheduler of GPUI runs the tasks of the app in a random order from its
//! seed, and runs the blocking work of a request on the test thread, so a
//! test steps the app one task at a time (`tick_until`) to leave an answer
//! on its way while the user does something else.
//!
//! The tests keep away from the files of the user: `app` puts the process
//! in the sandbox (`Bloom::save_config` writes nothing, a session opens no
//! download engine and no server socket), and the mock answers 404 for
//! images, so the image cache on disk gets nothing. The environment of the
//! process is not changed: `set_var` is unsafe while other tests run.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use gpui_kit::{Entity, TestAppContext, VisualTestContext};
use serde_json::{Value, json};

use super::{Bloom, Session, theme};
use crate::{config::Config, jellyfin::{Client, Item}, ui::theme::UiTheme};

static SANDBOX: AtomicBool = AtomicBool::new(false);

/// True once a test made the app of this process; see the module notes.
pub(crate) fn sandboxed() -> bool {
    SANDBOX.load(Ordering::Relaxed)
}

/// Request as the server saw it: method, path, body.
pub(crate) type Seen = (String, String, String);

type Answer = dyn Fn(&str, &str, &str) -> (u16, String) + Send + Sync;

/// A server that answers what `answer(method, path, body)` says, and keeps
/// every request it saw. It ends with the test: the drop stops the
/// listener and joins its threads.
pub(crate) struct MockServer {
    pub url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MockServer {
    pub(crate) fn start(answer: impl Fn(&str, &str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let workers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let (log, answer): (_, Arc<Answer>) = (seen.clone(), Arc::new(answer));
        let accept = {
            let (stop, workers) = (stop.clone(), workers.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let (log, answer) = (log.clone(), answer.clone());
                    let worker = std::thread::spawn(move || serve(stream, log, answer));
                    workers.lock().expect("workers").push(worker);
                }
            })
        };
        Self { url, seen, stop, accept: Some(accept), workers }
    }

    /// Requests seen so far, in the order the server took them.
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen").clone()
    }

    pub(crate) fn count(&self, method: &str, path_part: &str) -> usize {
        self.seen()
            .iter()
            .filter(|(m, p, _)| m == method && p.contains(path_part))
            .count()
    }

    /// The bodies of the requests with a method and a path part, in order.
    pub(crate) fn bodies(&self, method: &str, path_part: &str) -> Vec<Value> {
        self.seen()
            .iter()
            .filter(|(m, p, _)| m == method && p.contains(path_part))
            .map(|(_, _, body)| serde_json::from_str(body).unwrap_or(Value::Null))
            .collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The listener wakes from `accept` once more and sees the flag.
        let address = self.url.trim_start_matches("http://").to_string();
        let _ = TcpStream::connect(address);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let workers = std::mem::take(&mut *self.workers.lock().expect("workers"));
        for worker in workers {
            let _ = worker.join();
        }
    }
}

fn serve(mut stream: TcpStream, log: Arc<Mutex<Vec<Seen>>>, answer: Arc<Answer>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or_default().split(' ');
    let method = request.next().unwrap_or_default().to_string();
    let path = request.next().unwrap_or_default().to_string();
    let length: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&buf[head_end..head_end + length]).to_string();
    let (status, reply) = answer(&method, &path, &body);
    log.lock().expect("seen").push((method, path, body));
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.flush();
}

/// What a server answers when the test did not say: an empty list for a
/// read, nothing for a write, and no image (so nothing lands in the image
/// cache of this Mac).
pub(crate) fn plain(method: &str, path: &str) -> (u16, String) {
    if path.contains("/Images/") {
        (404, String::new())
    } else if method == "GET" {
        (200, r#"{"Items":[],"TotalRecordCount":0}"#.to_string())
    } else {
        (204, String::new())
    }
}

pub(crate) fn item(id: &str, name: &str, kind: &str) -> Item {
    serde_json::from_value(json!({
        "Id": id, "Name": name, "Type": kind, "ImageTags": {"Primary": "t"}
    }))
    .expect("item")
}

/// A page of items, as the server sends one.
pub(crate) fn items_json(ids: &[&str], kind: &str) -> String {
    let items: Vec<Value> = ids
        .iter()
        .map(|id| json!({"Id": id, "Name": format!("Title {id}"), "Type": kind, "ImageTags": {"Primary": "t"}}))
        .collect();
    json!({"Items": items, "TotalRecordCount": items.len()}).to_string()
}

/// A session with a mock server, for a test that does not go through the
/// sign-in (`Bloom::open_session` needs the server in the config).
pub(crate) fn session(url: &str, user: &str) -> Session {
    Session {
        server_id: url.to_string(),
        server_name: "mock".to_string(),
        user_id: user.to_string(),
        user_name: user.to_string(),
        user_image: None,
        client: Client::new(url, "race-test").with_session("token", user),
        is_admin: true,
        audio_language: None,
    }
}

/// The app in a test window, signed in to nothing, in the sandbox.
pub(crate) fn app(cx: &mut TestAppContext) -> (Entity<Bloom>, &mut VisualTestContext) {
    SANDBOX.store(true, Ordering::Relaxed);
    cx.update(|cx| {
        crate::ui::theme::init(cx);
        UiTheme::set(cx, theme(true));
    });
    cx.add_window_view(|window, cx| Bloom::new(Config::default(), window, cx))
}

/// Runs the tasks of the app one at a time until `done` says so. A request
/// whose work ran has its answer still on its way: a task of its own that
/// the next steps run.
pub(crate) fn tick_until(cx: &mut VisualTestContext, mut done: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if done() {
            return;
        }
        assert!(cx.executor().tick(), "nothing left to run, and the condition did not come");
    }
    panic!("the condition did not come in 100000 steps");
}

/// Puts a server with one profile into the config, so that
/// `Bloom::open_session(id, user)` opens a session with it.
pub(crate) fn add_server(this: &mut Bloom, id: &str, url: &str, user: &str) {
    this.config.upsert_server(crate::config::Server {
        id: id.into(),
        name: id.to_uppercase(),
        url: url.to_string(),
        profiles: vec![],
    });
    this.config.upsert_profile(
        id,
        crate::config::Profile { user_id: user.into(), name: user.to_uppercase(), token: "t".into(), image_tag: None },
    );
}
