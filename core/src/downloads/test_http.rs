// SPDX-License-Identifier: AGPL-3.0-or-later
//! For the test servers of the downloads: one request, read off a
//! connection to the end of its body.

use std::{io::Read as _, net::TcpStream};

pub(crate) struct Request {
    /// The request line and the headers.
    pub head: String,
    pub body: String,
}

impl Request {
    /// `None` when the connection closed before the end of the headers.
    /// The body is read to its end: an answer sent over unread bytes can
    /// reset the connection under the client.
    pub fn read(stream: &mut TcpStream) -> Option<Self> {
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        let mut more = |data: &mut Vec<u8>| match stream.read(&mut buffer) {
            Ok(n) if n > 0 => {
                data.extend_from_slice(&buffer[..n]);
                true
            }
            _ => false,
        };
        let end = loop {
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break end;
            }
            if !more(&mut data) {
                return None;
            }
        };
        let mut request = Self { head: String::from_utf8_lossy(&data[..end]).into_owned(), body: String::new() };
        let length: usize = request.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        while data.len() < end + 4 + length && more(&mut data) {}
        request.body = String::from_utf8_lossy(&data[end + 4..]).into_owned();
        Some(request)
    }

    /// "GET /path?query HTTP/1.1".
    pub fn line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }

    /// The path, without the query.
    pub fn path(&self) -> &str {
        let target = self.line().split_whitespace().nth(1).unwrap_or("/");
        target.split('?').next().unwrap_or("/")
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.head
            .lines()
            .skip(1)
            .find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case(name)))
            .map(|(_, value)| value.trim().to_string())
    }
}
