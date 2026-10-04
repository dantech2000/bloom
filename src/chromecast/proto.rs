// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one message of the cast protocol (CASTV2), by hand. On the wire a
//! message is a 4-byte big-endian length and then the protobuf
//! `CastMessage`:
//!
//! ```text
//! 1 protocol_version  varint  always 0 (CASTV2_1_0)
//! 2 source_id         string
//! 3 destination_id    string
//! 4 namespace         string
//! 5 payload_type      varint  0 = text, 1 = binary
//! 6 payload_utf8      string
//! 7 payload_binary    bytes
//! ```
//!
//! That is all of it, so a protobuf crate would be more code than this.

use std::io::{self, Read};

use anyhow::{Result, anyhow, bail};

/// Longer frames are not cast messages; the devices stop at 64 KiB.
const MAX_FRAME: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CastMessage {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    pub payload: Payload,
}

impl CastMessage {
    pub fn text(source: &str, destination: &str, namespace: &str, text: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
            namespace: namespace.into(),
            payload: Payload::Text(text.into()),
        }
    }

    /// The text of the payload; empty for a binary one.
    pub fn text_payload(&self) -> &str {
        match &self.payload {
            Payload::Text(text) => text,
            Payload::Binary(_) => "",
        }
    }

    /// The message with its length in front, ready for the socket.
    pub fn frame(&self) -> Vec<u8> {
        let body = self.encode();
        let mut frame = Vec::with_capacity(body.len() + 4);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    /// The protobuf body, without the length.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.namespace.len() + self.payload_len());
        put_varint(&mut out, 1, 0);
        put_bytes(&mut out, 2, self.source.as_bytes());
        put_bytes(&mut out, 3, self.destination.as_bytes());
        put_bytes(&mut out, 4, self.namespace.as_bytes());
        match &self.payload {
            Payload::Text(text) => {
                put_varint(&mut out, 5, 0);
                put_bytes(&mut out, 6, text.as_bytes());
            }
            Payload::Binary(bytes) => {
                put_varint(&mut out, 5, 1);
                put_bytes(&mut out, 7, bytes);
            }
        }
        out
    }

    fn payload_len(&self) -> usize {
        match &self.payload {
            Payload::Text(text) => text.len(),
            Payload::Binary(bytes) => bytes.len(),
        }
    }

    /// Reads one protobuf body. Fields it does not know are skipped.
    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        let (mut source, mut destination, mut namespace) = (String::new(), String::new(), String::new());
        let (mut payload_type, mut text, mut binary) = (0u64, None, None);
        while !bytes.is_empty() {
            let key = get_varint(&mut bytes)?;
            let (field, wire) = (key >> 3, key & 7);
            match (field, wire) {
                (1, 0) => {
                    if get_varint(&mut bytes)? != 0 {
                        bail!("unknown cast protocol version");
                    }
                }
                (2, 2) => source = get_string(&mut bytes)?,
                (3, 2) => destination = get_string(&mut bytes)?,
                (4, 2) => namespace = get_string(&mut bytes)?,
                (5, 0) => payload_type = get_varint(&mut bytes)?,
                (6, 2) => text = Some(get_string(&mut bytes)?),
                (7, 2) => binary = Some(get_bytes(&mut bytes)?.to_vec()),
                (_, 0) => {
                    get_varint(&mut bytes)?;
                }
                (_, 1) => bytes = skip(bytes, 8)?,
                (_, 2) => {
                    get_bytes(&mut bytes)?;
                }
                (_, 5) => bytes = skip(bytes, 4)?,
                _ => bail!("bad protobuf wire type {wire}"),
            }
        }
        let payload = match payload_type {
            0 => Payload::Text(text.unwrap_or_default()),
            1 => Payload::Binary(binary.unwrap_or_default()),
            other => bail!("unknown payload type {other}"),
        };
        Ok(Self { source, destination, namespace, payload })
    }
}

/// Collects bytes from the socket and gives back whole messages.
#[derive(Default)]
pub struct Inbox {
    bytes: Vec<u8>,
}

impl Inbox {
    /// Reads what the socket has right now; `Ok(false)` when it closed.
    pub fn fill(&mut self, reader: &mut impl Read) -> io::Result<bool> {
        let mut chunk = [0u8; 16 * 1024];
        let n = reader.read(&mut chunk)?;
        self.bytes.extend_from_slice(&chunk[..n]);
        Ok(n > 0)
    }

    /// The next whole message, when one is in.
    pub fn next(&mut self) -> Result<Option<CastMessage>> {
        if self.bytes.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3]]) as usize;
        if len > MAX_FRAME {
            bail!("cast frame of {len} bytes");
        }
        if self.bytes.len() < 4 + len {
            return Ok(None);
        }
        let message = CastMessage::decode(&self.bytes[4..4 + len])?;
        self.bytes.drain(..4 + len);
        Ok(Some(message))
    }
}

fn put_varint(out: &mut Vec<u8>, field: u32, mut value: u64) {
    push_varint(out, u64::from(field << 3));
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    push_varint(out, u64::from(field << 3 | 2));
    push_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn get_varint(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first().ok_or_else(|| anyhow!("short varint"))?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("varint too long")
}

fn get_bytes<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8]> {
    let len = get_varint(bytes)? as usize;
    if bytes.len() < len {
        bail!("field of {len} bytes in {} bytes", bytes.len());
    }
    let (field, rest) = bytes.split_at(len);
    *bytes = rest;
    Ok(field)
}

fn get_string(bytes: &mut &[u8]) -> Result<String> {
    Ok(String::from_utf8(get_bytes(bytes)?.to_vec())?)
}

fn skip(bytes: &[u8], n: usize) -> Result<&[u8]> {
    bytes.get(n..).ok_or_else(|| anyhow!("short field"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";

    /// The PING of the sender, byte for byte from the protobuf definition.
    fn ping_frame() -> Vec<u8> {
        let mut body = vec![0x08, 0x00];
        body.extend_from_slice(b"\x12\x08sender-0");
        body.extend_from_slice(b"\x1a\x0areceiver-0");
        body.push(0x22);
        body.push(HEARTBEAT.len() as u8);
        body.extend_from_slice(HEARTBEAT.as_bytes());
        body.extend_from_slice(b"\x28\x00");
        body.extend_from_slice(b"\x32\x0f{\"type\":\"PING\"}");
        assert_eq!(body.len(), 84);
        let mut frame = vec![0, 0, 0, 84];
        frame.extend(body);
        frame
    }

    #[test]
    fn encodes_the_ping_as_the_spec_says() {
        let message = CastMessage::text("sender-0", "receiver-0", HEARTBEAT, r#"{"type":"PING"}"#);
        assert_eq!(message.frame(), ping_frame());
    }

    #[test]
    fn decodes_what_it_encoded_and_skips_unknown_fields() {
        let frame = ping_frame();
        let decoded = CastMessage::decode(&frame[4..]).unwrap();
        assert_eq!(decoded, CastMessage::text("sender-0", "receiver-0", HEARTBEAT, r#"{"type":"PING"}"#));

        // A binary payload, and a field 9 (varint) and field 10 (bytes)
        // that a newer device could add.
        let binary = CastMessage {
            source: "a".into(),
            destination: "b".into(),
            namespace: "urn:x-cast:test".into(),
            payload: Payload::Binary(vec![1, 2, 3, 0, 255]),
        };
        let mut body = binary.encode();
        body.extend_from_slice(&[0x48, 0x96, 0x01, 0x52, 0x02, 0xaa, 0xbb]);
        assert_eq!(CastMessage::decode(&body).unwrap(), binary);
    }

    #[test]
    fn long_strings_get_two_byte_lengths() {
        let text = "x".repeat(300);
        let message = CastMessage::text("s", "d", "n", text.clone());
        let body = message.encode();
        // field 6, then 300 as a varint: 0xac 0x02.
        let at = body.len() - 300 - 3;
        assert_eq!(&body[at..at + 3], &[0x32, 0xac, 0x02]);
        assert_eq!(CastMessage::decode(&body).unwrap().text_payload(), text);
    }

    #[test]
    fn inbox_gives_whole_messages_only() {
        let frame = ping_frame();
        let mut inbox = Inbox::default();
        let mut first = &frame[..20];
        assert!(inbox.fill(&mut first).unwrap());
        assert!(inbox.next().unwrap().is_none());
        let mut rest = &frame[20..];
        assert!(inbox.fill(&mut rest).unwrap());
        let mut again = &frame[..];
        assert!(inbox.fill(&mut again).unwrap());
        assert_eq!(inbox.next().unwrap().unwrap().text_payload(), r#"{"type":"PING"}"#);
        assert_eq!(inbox.next().unwrap().unwrap().text_payload(), r#"{"type":"PING"}"#);
        assert!(inbox.next().unwrap().is_none());
        assert!(!inbox.fill(&mut &b""[..]).unwrap(), "a closed socket");
    }

    #[test]
    fn rejects_a_huge_frame() {
        let mut inbox = Inbox::default();
        inbox.fill(&mut &[0x7f, 0xff, 0xff, 0xff, 0][..]).unwrap();
        assert!(inbox.next().is_err());
    }
}
