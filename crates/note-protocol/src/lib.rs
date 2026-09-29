//! NOTE local protocol v1 wire envelope (Spec §12.1, docs/protocol-v1.md).
//!
//! ```text
//! magic[4] = "NOTE" | version u16 LE | kind u16 LE | request_id u64 LE | payload_length u32 LE | payload
//! ```
//!
//! Control payloads are UTF-8 JSON; frames and audio are binary. This is a
//! specified byte layout, never a raw dump of a language struct.

pub mod frame;

pub const MAGIC: [u8; 4] = *b"NOTE";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 4 + 2 + 2 + 8 + 4;
pub const MAX_CONTROL_PAYLOAD: u32 = 256 * 1024;
pub const MAX_BINARY_PAYLOAD: u32 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Request = 1,
    Response = 2,
    Event = 3,
    Frame = 16,
    Audio = 17,
    Chunk = 18,
}

impl Kind {
    pub fn from_u16(value: u16) -> Option<Kind> {
        Some(match value {
            1 => Kind::Request,
            2 => Kind::Response,
            3 => Kind::Event,
            16 => Kind::Frame,
            17 => Kind::Audio,
            18 => Kind::Chunk,
            _ => return None,
        })
    }

    pub fn is_binary(self) -> bool {
        matches!(self, Kind::Frame | Kind::Audio | Kind::Chunk)
    }

    pub fn max_payload(self) -> u32 {
        if self.is_binary() {
            MAX_BINARY_PAYLOAD
        } else {
            MAX_CONTROL_PAYLOAD
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: Kind,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("bad magic {0:02x?}")]
    BadMagic([u8; 4]),
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("unknown message kind {0}")]
    UnknownKind(u16),
    #[error("payload of {len} bytes exceeds the {max}-byte limit for {kind:?}")]
    TooLarge { kind: Kind, len: u32, max: u32 },
    #[error("control payload is not UTF-8")]
    NotUtf8,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("payload of {len} bytes exceeds the {max}-byte limit for {kind:?}")]
    TooLarge { kind: Kind, len: usize, max: u32 },
}

impl Message {
    pub fn new(kind: Kind, request_id: u64, payload: impl Into<Vec<u8>>) -> Message {
        Message { kind, request_id, payload: payload.into() }
    }

    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let max = self.kind.max_payload();
        if self.payload.len() > max as usize {
            return Err(EncodeError::TooLarge { kind: self.kind, len: self.payload.len(), max });
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.kind as u16).to_le_bytes());
        out.extend_from_slice(&self.request_id.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }
}

/// Incremental decoder for a byte stream. Feed arbitrary chunks; complete
/// messages come out in order. Any error is fatal for the connection: the
/// caller closes it rather than trying to resynchronise.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn next_message(&mut self) -> Result<Option<Message>, DecodeError> {
        if self.buf.len() < HEADER_LEN {
            // Reject a wrong magic as soon as it is visible.
            let n = self.buf.len().min(4);
            if self.buf[..n] != MAGIC[..n] {
                let mut got = [0u8; 4];
                got[..n].copy_from_slice(&self.buf[..n]);
                return Err(DecodeError::BadMagic(got));
            }
            return Ok(None);
        }
        let h = &self.buf[..HEADER_LEN];
        let magic: [u8; 4] = h[0..4].try_into().unwrap();
        if magic != MAGIC {
            return Err(DecodeError::BadMagic(magic));
        }
        let version = u16::from_le_bytes(h[4..6].try_into().unwrap());
        if version != VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let raw_kind = u16::from_le_bytes(h[6..8].try_into().unwrap());
        let kind = Kind::from_u16(raw_kind).ok_or(DecodeError::UnknownKind(raw_kind))?;
        let request_id = u64::from_le_bytes(h[8..16].try_into().unwrap());
        let len = u32::from_le_bytes(h[16..20].try_into().unwrap());
        let max = kind.max_payload();
        if len > max {
            return Err(DecodeError::TooLarge { kind, len, max });
        }
        let total = HEADER_LEN + len as usize;
        if self.buf.len() < total {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LEN..total].to_vec();
        self.buf.drain(..total);
        if !kind.is_binary() && std::str::from_utf8(&payload).is_err() {
            return Err(DecodeError::NotUtf8);
        }
        Ok(Some(Message { kind, request_id, payload }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn hello_request_matches_the_shared_fixture() {
        let msg = Message::new(Kind::Request, 1, br#"{"method":"hello","protocol":1}"#.to_vec());
        assert_eq!(msg.encode().unwrap(), fixture("hello-request.bin"));
    }

    #[test]
    fn stream_split_at_every_byte_still_decodes() {
        let a = Message::new(Kind::Request, 7, b"{}".to_vec()).encode().unwrap();
        let b = Message::new(Kind::Frame, 8, vec![0xAB; 30_000]).encode().unwrap();
        let wire = [a, b].concat();
        let mut dec = Decoder::new();
        let mut got = Vec::new();
        for byte in &wire {
            dec.feed(std::slice::from_ref(byte));
            while let Some(m) = dec.next_message().unwrap() {
                got.push((m.kind, m.request_id, m.payload.len()));
            }
        }
        assert_eq!(got, [(Kind::Request, 7, 2), (Kind::Frame, 8, 30_000)]);
    }

    #[test]
    fn hostile_headers_are_rejected_before_allocation() {
        let mut header = Message::new(Kind::Request, 1, Vec::new()).encode().unwrap();
        header[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut dec = Decoder::new();
        dec.feed(&header);
        assert!(matches!(dec.next_message(), Err(DecodeError::TooLarge { .. })));

        let mut dec = Decoder::new();
        dec.feed(b"HTTP");
        assert!(matches!(dec.next_message(), Err(DecodeError::BadMagic(_))));

        let mut v2 = Message::new(Kind::Event, 1, Vec::new()).encode().unwrap();
        v2[4] = 2;
        let mut dec = Decoder::new();
        dec.feed(&v2);
        assert_eq!(dec.next_message(), Err(DecodeError::UnsupportedVersion(2)));
    }

    #[test]
    fn control_payload_must_be_utf8() {
        let bad = Message::new(Kind::Response, 3, vec![0xFF, 0xFE]).encode().unwrap();
        let mut dec = Decoder::new();
        dec.feed(&bad);
        assert_eq!(dec.next_message(), Err(DecodeError::NotUtf8));
    }
}
