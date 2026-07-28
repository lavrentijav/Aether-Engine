//! Minecraft protocol wire primitives (uncompressed framing).
//!
//! Packets are `VarInt length | VarInt id | data`. We never enable compression
//! or encryption, which a vanilla 1.8.9 client accepts for offline play.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// Builds a packet body (everything after the length prefix).
#[derive(Default)]
pub struct PacketOut {
    buf: Vec<u8>,
}

impl PacketOut {
    /// Start a packet with the given id.
    pub fn new(id: i32) -> Self {
        let mut p = PacketOut { buf: Vec::new() };
        p.var_int(id);
        p
    }

    /// Append a VarInt.
    pub fn var_int(&mut self, value: i32) -> &mut Self {
        write_var_int(&mut self.buf, value);
        self
    }

    /// Append a length-prefixed UTF-8 string.
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.var_int(s.len() as i32);
        self.buf.extend_from_slice(s.as_bytes());
        self
    }

    /// Append a single byte.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    /// Append a bool (0/1).
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.buf.push(v as u8);
        self
    }
    /// Append a big-endian `u16`.
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// Append a big-endian `i32`.
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// Append a big-endian `i64`.
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// Append a big-endian `f32`.
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// Append a big-endian `f64`.
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// Append raw bytes.
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    /// Write the framed packet (length prefix + body) to `w`.
    pub fn send<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let mut frame = Vec::with_capacity(self.buf.len() + 3);
        write_var_int(&mut frame, self.buf.len() as i32);
        frame.extend_from_slice(&self.buf);
        w.write_all(&frame)
    }
}

/// Encode a VarInt into `out`.
pub fn write_var_int(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7f == 0 {
            out.push(v as u8);
            return;
        }
        out.push(((v & 0x7f) | 0x80) as u8);
        v >>= 7;
    }
}

/// Reads packet fields from an in-memory payload.
pub struct PacketIn<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> PacketIn<'a> {
    /// Wrap a payload slice.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Read a VarInt.
    pub fn var_int(&mut self) -> io::Result<i32> {
        let mut result: u32 = 0;
        for i in 0..5 {
            let byte = *self
                .buf
                .get(self.pos)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "varint eof"))?;
            self.pos += 1;
            result |= ((byte & 0x7f) as u32) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(result as i32);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "varint too long",
        ))
    }

    /// Read a length-prefixed string.
    pub fn string(&mut self) -> io::Result<String> {
        let len = self.var_int()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "string eof"))?;
        let s = std::str::from_utf8(&self.buf[self.pos..end])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad utf8"))?
            .to_owned();
        self.pos = end;
        Ok(s)
    }

    /// Read a big-endian `u16`.
    pub fn u16(&mut self) -> io::Result<u16> {
        let end = self.pos + 2;
        let b = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "u16 eof"))?;
        self.pos = end;
        Ok(u16::from_be_bytes(b.try_into().unwrap()))
    }

    /// Read a big-endian `i64`.
    pub fn i64(&mut self) -> io::Result<i64> {
        let end = self.pos + 8;
        let b = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "i64 eof"))?;
        self.pos = end;
        Ok(i64::from_be_bytes(b.try_into().unwrap()))
    }
}

/// Upper bound on an inbound frame length. The client only ever sends us small
/// packets (handshake, status, login, movement, keep-alive), so anything larger
/// is malformed or hostile. Without this cap a single forged length prefix would
/// make us allocate up to 2 GiB, a trivial memory-exhaustion DoS.
const MAX_PACKET_LEN: usize = 2 * 1024 * 1024;

/// Once a frame has started arriving, how long we'll wait for the *rest* of it
/// before giving up. This bounds a client that sends a length prefix and then
/// stalls, so a half-sent frame can't pin a connection thread indefinitely.
const FRAME_DEADLINE: Duration = Duration::from_secs(30);

/// Read one byte, retrying past transient timeouts until `deadline`.
///
/// Used once we are committed to a frame: a `WouldBlock` / `TimedOut` /
/// `Interrupted` just means the next byte hasn't arrived yet, so we wait rather
/// than desync the stream — but not past the deadline. Shares the retry loop
/// with [`read_frame_body`] via a one-byte buffer.
fn read_committed_byte<R: Read>(r: &mut R, deadline: Instant) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    read_frame_body(r, &mut byte, deadline)?;
    Ok(byte[0])
}

/// Fill `buf` completely, retrying past transient timeouts until `deadline`.
///
/// Unlike [`Read::read_exact`], a read timeout mid-frame is *not* immediately
/// fatal: once the length prefix is consumed the body is committed and the rest
/// of the bytes are imminent, so we keep waiting instead of tearing down (and
/// desyncing) the connection — bounded by `deadline` so a stalled sender can't
/// pin the thread forever.
fn read_frame_body<R: Read>(r: &mut R, buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "eof mid-packet",
                ))
            }
            Ok(n) => filled += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "frame stalled"));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Read one byte, returning `Ok(None)` if the read times out with nothing
/// available (`WouldBlock` / `TimedOut`). `Interrupted` (EINTR) is retried.
fn read_idle_byte<R: Read>(r: &mut R) -> io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof")),
            Ok(_) => return Ok(Some(byte[0])),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// A decoded packet: its id and payload (id VarInt already stripped).
#[derive(Debug)]
pub struct RawPacket {
    /// Packet id.
    pub id: i32,
    /// Payload after the id.
    pub data: Vec<u8>,
}

/// Read a full packet frame from `r`.
///
/// `Ok(None)` means the read timed out before any byte of a frame arrived (the
/// caller can send a keep-alive and retry). Once the first byte is in we are
/// committed: the length varint and body are read to completion, tolerating
/// transient read timeouts but bounded by [`FRAME_DEADLINE`], so a partially
/// sent frame neither desyncs the stream nor pins the thread forever.
pub fn read_packet<R: Read>(r: &mut R) -> io::Result<Option<RawPacket>> {
    // First byte of the length varint: a timeout here just means "idle".
    let first = match read_idle_byte(r)? {
        Some(b) => b,
        None => return Ok(None),
    };
    let deadline = Instant::now() + FRAME_DEADLINE;

    // Assemble the rest of the length varint (up to 5 bytes total).
    let mut result = (first & 0x7f) as u32;
    let mut complete = first & 0x80 == 0;
    for i in 1..5 {
        if complete {
            break;
        }
        let byte = read_committed_byte(r, deadline)?;
        result |= ((byte & 0x7f) as u32) << (7 * i);
        complete = byte & 0x80 == 0;
    }
    if !complete {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "varint too long",
        ));
    }

    let len = result as i32;
    if len < 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "neg len"));
    }
    let len = len as usize;
    if len > MAX_PACKET_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "packet too large",
        ));
    }

    let mut buf = vec![0u8; len];
    read_frame_body(r, &mut buf, deadline)?;
    let mut pin = PacketIn::new(&buf);
    let id = pin.var_int()?;
    let data = buf[pin.pos..].to_vec();
    Ok(Some(RawPacket { id, data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multibyte_length_prefixes_round_trip() {
        // Bodies whose length prefix spans one and two VarInt bytes.
        for body_len in [0usize, 1, 127, 128, 300, 16384] {
            let mut p = PacketOut::new(0x00);
            p.bytes(&vec![0xABu8; body_len]);
            let mut wire = Vec::new();
            p.send(&mut wire).unwrap();

            let mut cur = std::io::Cursor::new(wire);
            let pkt = read_packet(&mut cur).unwrap().unwrap();
            assert_eq!(pkt.id, 0x00);
            assert_eq!(pkt.data, vec![0xABu8; body_len]);
        }
    }

    #[test]
    fn idle_first_byte_returns_none() {
        // A reader that reports WouldBlock with no data yields Ok(None).
        struct Idle;
        impl Read for Idle {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "idle"))
            }
        }
        assert!(read_packet(&mut Idle).unwrap().is_none());
    }

    #[test]
    fn packet_frames_round_trip() {
        let mut p = PacketOut::new(0x00);
        p.string("hello").u16(25565).var_int(2);
        let mut wire = Vec::new();
        p.send(&mut wire).unwrap();

        let mut cur = std::io::Cursor::new(wire);
        let pkt = read_packet(&mut cur).unwrap().unwrap();
        assert_eq!(pkt.id, 0x00);
        let mut pin = PacketIn::new(&pkt.data);
        assert_eq!(pin.string().unwrap(), "hello");
        assert_eq!(pin.u16().unwrap(), 25565);
        assert_eq!(pin.var_int().unwrap(), 2);
    }

    #[test]
    fn oversized_length_is_rejected_without_allocating() {
        // A forged length prefix far beyond MAX_PACKET_LEN must error, not
        // try to allocate gigabytes.
        let mut wire = Vec::new();
        write_var_int(&mut wire, (MAX_PACKET_LEN + 1) as i32);
        let mut cur = std::io::Cursor::new(wire);
        let err = read_packet(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// A reader that hands out its bytes in fixed chunks, injecting a transient
    /// `WouldBlock` between every chunk to mimic a slow / timing-out socket.
    struct FlakyReader {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
        block_next: bool,
    }

    impl Read for FlakyReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            if self.block_next {
                self.block_next = false;
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "slow"));
            }
            self.block_next = true;
            let n = self.chunk.min(buf.len()).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn body_read_survives_midframe_timeouts() {
        // Once the length prefix is consumed, transient timeouts inside the body
        // must not desync the frame — the reader keeps waiting for the rest.
        let mut p = PacketOut::new(0x21);
        p.string("a fairly long payload that spans several reads")
            .i64(1234567890);
        let mut wire = Vec::new();
        p.send(&mut wire).unwrap();

        let mut r = FlakyReader {
            data: wire,
            pos: 0,
            chunk: 3,
            block_next: false,
        };
        let pkt = read_packet(&mut r).unwrap().unwrap();
        assert_eq!(pkt.id, 0x21);
        let mut pin = PacketIn::new(&pkt.data);
        assert_eq!(
            pin.string().unwrap(),
            "a fairly long payload that spans several reads"
        );
        assert_eq!(pin.i64().unwrap(), 1234567890);
    }
}
