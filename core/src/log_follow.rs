// SPDX-License-Identifier: AGPL-3.0-or-later
//! The follow of a log file: the lines the server wrote after the part a
//! viewer has. Jellyfin 12 answers `/System/Logs/Log` without ranges (a
//! `Range` header gets the whole file with 200), so a follow reads the
//! body as it comes and keeps only what lies after the known end; a server
//! that answers 206 sends only the new bytes. A follow asks first whether
//! the file grew (its size in `/System/Logs`), so a log that is quiet costs
//! one small request a poll.

use std::io::{ErrorKind, Read};

/// The lines of a log after a known end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogGrowth {
    /// Whole lines only: a line the server is still writing comes with the
    /// next follow.
    pub text: String,
    /// The byte after the last whole line: where the next follow starts.
    pub end: u64,
    /// The file is shorter than the known end (it was cut or replaced):
    /// `text` is the end of the file as it is now, and the viewer starts
    /// again.
    pub restarted: bool,
    /// More was new than `max` bytes: `text` is the end of it, from the
    /// start of a line.
    pub cut: bool,
}

impl LogGrowth {
    /// Nothing new after `end`.
    pub fn nothing(end: u64) -> Self {
        Self { end, ..Self::default() }
    }

    /// The same growth for a body that started at byte `from` of the file
    /// (an answer of 206).
    pub fn shifted(mut self, from: u64) -> Self {
        self.end += from;
        self
    }
}

/// Reads a whole file body from `reader` and keeps its bytes from `from`
/// on, at most the last `max` of them, as whole lines. Memory stays near
/// `2 * max` for a file of any size.
pub fn since_of(mut reader: impl Read, from: u64, max: usize) -> std::io::Result<LogGrowth> {
    let max = max.max(1);
    // The last bytes of the stream, and the place of the first of them.
    let mut kept: Vec<u8> = Vec::new();
    let mut kept_start: u64 = 0;
    let mut total: u64 = 0;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        total += read as u64;
        kept.extend_from_slice(&chunk[..read]);
        // Twice the room before the front goes: one move for each `max`
        // bytes read, not one for each chunk.
        if kept.len() > max.saturating_mul(2) {
            let gone = kept.len() - max;
            kept.drain(..gone);
            kept_start += gone as u64;
        }
    }
    let restarted = total < from;
    let begin = if restarted { 0 } else { from };
    let mut cut = false;
    let mut start = match begin.checked_sub(kept_start) {
        Some(offset) => offset as usize,
        None => {
            cut = true;
            0
        }
    };
    if kept.len() - start > max {
        start = kept.len() - max;
        cut = true;
    }
    let mut at = kept_start + start as u64;
    let mut region = &kept[start..];
    if cut {
        // The first line is the rest of a line: it goes.
        match region.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                region = &region[newline + 1..];
                at += newline as u64 + 1;
            }
            None => {
                at += region.len() as u64;
                region = &[];
            }
        }
    }
    let whole = region.iter().rposition(|byte| *byte == b'\n').map_or(0, |newline| newline + 1);
    Ok(LogGrowth {
        text: String::from_utf8_lossy(&region[..whole]).into_owned(),
        end: at + whole as u64,
        restarted,
        cut,
    })
}

/// Whether the file grew past (or shrank under) the known end, by its size
/// in the list of log files: a follow reads the file only then. `None`
/// (the file is not listed) asks for no read either.
pub fn changed(size: Option<u64>, end: u64) -> bool {
    size.is_some_and(|size| size != end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that hands out one byte at a time, as a slow network does.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.split_first() {
                Some((first, rest)) if !buf.is_empty() => {
                    buf[0] = *first;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn a_first_read_keeps_the_whole_lines() {
        let growth = since_of(&b"a\nb\nc"[..], 0, 1024).unwrap();
        assert_eq!(growth, LogGrowth { text: "a\nb\n".into(), end: 4, restarted: false, cut: false });
    }

    #[test]
    fn a_follow_keeps_what_came_after_the_end() {
        let growth = since_of(&b"a\nb\nc\nd\n"[..], 4, 1024).unwrap();
        assert_eq!((growth.text.as_str(), growth.end), ("c\nd\n", 8));
        // The line that was half written last time comes whole now.
        let growth = since_of(&b"a\nb\nc is long\n"[..], 4, 1024).unwrap();
        assert_eq!((growth.text.as_str(), growth.end), ("c is long\n", 14));
    }

    #[test]
    fn nothing_new_keeps_the_end() {
        assert_eq!(since_of(&b"a\nb\n"[..], 4, 1024).unwrap(), LogGrowth::nothing(4));
        // Half a line is not a line yet.
        assert_eq!(since_of(&b"a\nb\nhal"[..], 4, 1024).unwrap(), LogGrowth::nothing(4));
    }

    #[test]
    fn a_shorter_file_starts_again() {
        let growth = since_of(&b"new\n"[..], 100, 1024).unwrap();
        assert_eq!(growth, LogGrowth { text: "new\n".into(), end: 4, restarted: true, cut: false });
    }

    #[test]
    fn a_large_growth_keeps_its_end_from_a_line_start() {
        let mut text = String::new();
        for n in 0..200 {
            text.push_str(&format!("line {n}\n"));
        }
        let bytes = text.as_bytes();
        let growth = since_of(Trickle(bytes), 3, 40).unwrap();
        assert!(growth.cut && !growth.restarted);
        assert_eq!(growth.end, bytes.len() as u64);
        assert!(growth.text.ends_with("line 199\n"));
        assert!(growth.text.len() <= 40);
        // Each line is whole.
        assert!(growth.text.lines().all(|line| line.starts_with("line ")));
    }

    #[test]
    fn a_206_answer_is_shifted_to_the_file() {
        let growth = since_of(&b"c\nd\n"[..], 0, 1024).unwrap().shifted(4);
        assert_eq!((growth.text.as_str(), growth.end), ("c\nd\n", 8));
    }

    #[test]
    fn a_read_happens_when_the_size_moved() {
        assert!(changed(Some(10), 4));
        assert!(changed(Some(2), 4));
        assert!(!changed(Some(4), 4));
        assert!(!changed(None, 4));
    }
}
