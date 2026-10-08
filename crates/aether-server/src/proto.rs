//! Minecraft protocol wire primitives.
//!
//! A frame is `VarInt length | VarInt id | data` until the server sends Set
//! Compression, after which it gains a second header field:
//! `VarInt length | VarInt uncompressed-length | data`, where a zero
//! uncompressed-length means "short packet, stored as-is" and any other value
//! means the rest is zlib-deflated and inflates to exactly that many bytes.
//!
//! Both directions switch at the same moment, and the moment is per
//! connection — which is why the state lives in [`Conn`] rather than in a
//! global. Writing to a bare socket is deliberately not possible: the type
//! that owns the stream is the type that knows how to frame for it.

use std::io::{self, Read, Write};
use std::net::TcpStream;
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
    /// Append a 128-bit UUID as its 16 raw bytes (the wire type used by e.g.
    /// Spawn Player — *not* the hyphenated-string form Login Success uses).
    pub fn uuid(&mut self, v: u128) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// The packet body: the id VarInt and everything after it, unframed.
    #[cfg(test)]
    pub fn body(&self) -> &[u8] {
        &self.buf
    }

    /// Frame this packet for `c` and write it.
    pub fn send(&self, c: &mut Conn) -> io::Result<()> {
        let frame = frame_packet(&self.buf, c.threshold);
        c.stream.write_all(&frame)
    }

    /// Frame this packet for an arbitrary writer at an explicit threshold.
    ///
    /// Only for tests and for measuring wire size; live connections go through
    /// [`PacketOut::send`], which cannot get the threshold wrong.
    #[cfg(test)]
    pub fn write_to<W: Write>(&self, w: &mut W, threshold: Option<usize>) -> io::Result<()> {
        w.write_all(&frame_packet(&self.buf, threshold))
    }
}

/// Build the bytes that go on the wire for one packet body.
fn frame_packet(body: &[u8], threshold: Option<usize>) -> Vec<u8> {
    let Some(threshold) = threshold else {
        let mut frame = Vec::with_capacity(body.len() + 3);
        write_var_int(&mut frame, body.len() as i32);
        frame.extend_from_slice(body);
        return frame;
    };

    // The threshold is compared against the *uncompressed* length, and a body
    // below it must travel verbatim: deflating it anyway would be rejected,
    // since a zero uncompressed-length is precisely what tells the reader not
    // to inflate.
    let mut inner = Vec::new();
    if body.len() >= threshold {
        write_var_int(&mut inner, body.len() as i32);
        inner.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(body, 6));
    } else {
        write_var_int(&mut inner, 0);
        inner.extend_from_slice(body);
    }

    let mut frame = Vec::with_capacity(inner.len() + 3);
    write_var_int(&mut frame, inner.len() as i32);
    frame.extend_from_slice(&inner);
    frame
}

/// One client connection: the socket plus the framing it has been switched to.
///
/// Compression is off until [`Conn::enable_compression`] is called, which every
/// codec does immediately after writing its Set Compression packet — that
/// packet is itself the last one sent in the old framing.
pub struct Conn {
    stream: TcpStream,
    /// Body length at or above which a frame is deflated; `None` while the
    /// connection is still uncompressed.
    threshold: Option<usize>,
}

/// Set Compression, login state. The same id in every version this server
/// speaks, from 1.8 through 1.21.11.
pub const SET_COMPRESSION: i32 = 0x03;

impl Conn {
    /// Wrap a freshly accepted socket. Framing starts uncompressed.
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            threshold: None,
        }
    }

    /// Announce compression to the client and switch this connection to it.
    ///
    /// The Set Compression packet goes out under the *old* framing and the
    /// switch happens after it, which is the whole subtlety: get the order
    /// wrong and the client desynchronises on the very next packet.
    pub fn enable_compression(&mut self, threshold: usize) -> io::Result<()> {
        let mut p = PacketOut::new(SET_COMPRESSION);
        p.var_int(threshold as i32);
        p.send(self)?;
        self.threshold = Some(threshold);
        Ok(())
    }

    /// The active threshold, if compression is on.
    #[cfg(test)]
    pub fn threshold(&self) -> Option<usize> {
        self.threshold
    }

    /// A second handle on the same socket, framed the same way.
    ///
    /// Used to hand the write half to the player registry: it inherits the
    /// compression state rather than starting fresh, so a broadcast is framed
    /// exactly as the owning thread's own writes are.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            threshold: self.threshold,
        })
    }

    /// Set the read timeout on the underlying socket.
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(dur)
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
    /// A big-endian signed 16-bit integer — a container slot index, which is
    /// signed because -999 means "outside the window".
    pub fn i16(&mut self) -> io::Result<i16> {
        self.u16().map(|v| v as i16)
    }

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

    /// Read a single byte.
    pub fn u8(&mut self) -> io::Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "u8 eof"))?;
        self.pos += 1;
        Ok(b)
    }

    /// Read a bool (non-zero byte is `true`).
    pub fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }

    /// Read a big-endian `f32`.
    pub fn f32(&mut self) -> io::Result<f32> {
        let end = self.pos + 4;
        let b = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "f32 eof"))?;
        self.pos = end;
        Ok(f32::from_be_bytes(b.try_into().unwrap()))
    }

    /// Read a big-endian `f64`.
    pub fn f64(&mut self) -> io::Result<f64> {
        let end = self.pos + 8;
        let b = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "f64 eof"))?;
        self.pos = end;
        Ok(f64::from_be_bytes(b.try_into().unwrap()))
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
pub fn read_packet(c: &mut Conn) -> io::Result<Option<RawPacket>> {
    let threshold = c.threshold;
    read_packet_from(&mut c.stream, threshold)
}

/// Read a frame from an arbitrary reader at an explicit compression state.
///
/// Only for tests and for the pre-connection handshake; live connections go
/// through [`read_packet`], which carries the state with the socket.
pub fn read_packet_from<R: Read>(
    r: &mut R,
    threshold: Option<usize>,
) -> io::Result<Option<RawPacket>> {
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

    // Under compression the frame carries a second header: the length the body
    // inflates to, or zero for a body stored verbatim because it fell below the
    // threshold.
    let body = if threshold.is_some() {
        let mut head = PacketIn::new(&buf);
        let inflated_len = head.var_int()?;
        if inflated_len < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "neg inflated len",
            ));
        }
        let rest = &buf[head.pos..];
        if inflated_len == 0 {
            rest.to_vec()
        } else {
            // Cap before inflating: a small frame may claim to expand to
            // anything, and honouring that claim is a decompression bomb.
            if inflated_len as usize > MAX_PACKET_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "inflated packet too large",
                ));
            }
            let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                rest,
                inflated_len as usize,
            )
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad deflate stream"))?;
            if out.len() != inflated_len as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "inflated length mismatch",
                ));
            }
            out
        }
    } else {
        buf
    };

    let mut pin = PacketIn::new(&body);
    let id = pin.var_int()?;
    let data = body[pin.pos..].to_vec();
    Ok(Some(RawPacket { id, data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull a VarInt straight out of a byte slice, written from the wire
    /// format rather than by calling the encoder's own helpers — a decoder
    /// that mirrors the encoder agrees with it even when both are wrong.
    fn read_varint_at(b: &[u8], pos: &mut usize) -> i32 {
        let mut v = 0i32;
        for i in 0..5 {
            let byte = b[*pos];
            *pos += 1;
            v |= ((byte & 0x7f) as i32) << (7 * i);
            if byte & 0x80 == 0 {
                break;
            }
        }
        v
    }

    #[test]
    fn below_threshold_travels_verbatim_behind_a_zero_marker() {
        let mut p = PacketOut::new(0x2A);
        p.bytes(&[7u8; 20]);
        let mut wire = Vec::new();
        p.write_to(&mut wire, Some(256)).unwrap();

        let mut pos = 0;
        let frame_len = read_varint_at(&wire, &mut pos) as usize;
        assert_eq!(
            frame_len,
            wire.len() - pos,
            "length covers the rest exactly"
        );
        assert_eq!(
            read_varint_at(&wire, &mut pos),
            0,
            "zero is what tells the reader not to inflate"
        );
        assert_eq!(&wire[pos..], p.body(), "short body goes as-is");
    }

    #[test]
    fn at_or_above_threshold_is_deflated() {
        // A long run of one byte is the shape a column's light arrays take,
        // and the case this whole mechanism exists for.
        let mut p = PacketOut::new(0x21);
        p.bytes(&[0xFFu8; 4096]);
        let mut wire = Vec::new();
        p.write_to(&mut wire, Some(256)).unwrap();

        let mut pos = 0;
        let frame_len = read_varint_at(&wire, &mut pos) as usize;
        assert_eq!(frame_len, wire.len() - pos);
        assert_eq!(
            read_varint_at(&wire, &mut pos) as usize,
            p.body().len(),
            "non-zero marker states the inflated size"
        );
        assert!(
            wire.len() < 200,
            "4 KiB of a single byte should collapse, got {}",
            wire.len()
        );
    }

    #[test]
    fn the_threshold_is_inclusive_and_measured_uncompressed() {
        // Exactly at the threshold compresses; one byte under does not. The
        // comparison is against the body *before* deflating, so a body that
        // would compress well still travels verbatim when it is short.
        let threshold = 64usize;
        for (body_len, expect_compressed) in [(threshold - 1, false), (threshold, true)] {
            let mut p = PacketOut::new(0x00);
            // The id varint is one byte, so pad to land the body on the mark.
            p.bytes(&vec![0xABu8; body_len - 1]);
            assert_eq!(p.body().len(), body_len);

            let mut wire = Vec::new();
            p.write_to(&mut wire, Some(threshold)).unwrap();
            let mut pos = 0;
            let _frame_len = read_varint_at(&wire, &mut pos);
            let marker = read_varint_at(&wire, &mut pos);
            assert_eq!(marker != 0, expect_compressed, "body of {body_len} bytes");
        }
    }

    #[test]
    fn compressed_frames_round_trip_in_both_shapes() {
        // Either side of the threshold, and empty, must survive the round trip.
        for body_len in [0usize, 10, 255, 256, 5000] {
            let mut p = PacketOut::new(0x33);
            p.bytes(&vec![0x5Au8; body_len]);
            let mut wire = Vec::new();
            p.write_to(&mut wire, Some(256)).unwrap();

            let mut cur = std::io::Cursor::new(wire);
            let pkt = read_packet_from(&mut cur, Some(256)).unwrap().unwrap();
            assert_eq!(pkt.id, 0x33);
            assert_eq!(pkt.data, vec![0x5Au8; body_len], "body of {body_len}");
        }
    }

    #[test]
    fn an_overstated_inflated_length_is_refused() {
        // A few compressed bytes may claim to expand to anything; honouring
        // that claim is a decompression bomb.
        let mut inner = Vec::new();
        write_var_int(&mut inner, (MAX_PACKET_LEN + 1) as i32);
        inner.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 16], 6));
        let mut wire = Vec::new();
        write_var_int(&mut wire, inner.len() as i32);
        wire.extend_from_slice(&inner);

        let mut cur = std::io::Cursor::new(wire);
        let err = read_packet_from(&mut cur, Some(256)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn set_compression_is_the_last_packet_in_the_old_framing() {
        // The ordering subtlety: the announcement itself must go out
        // uncompressed, and everything after it compressed. Getting this
        // backwards desynchronises the client on the very next packet.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();

        let mut conn = Conn::new(client);
        assert_eq!(conn.threshold(), None, "starts uncompressed");
        conn.enable_compression(256).unwrap();
        assert_eq!(conn.threshold(), Some(256));

        let mut big = PacketOut::new(0x21);
        big.bytes(&[0xFFu8; 1000]);
        big.send(&mut conn).unwrap();
        drop(conn);

        let mut got = Vec::new();
        peer.read_to_end(&mut got).unwrap();

        // Frame one, old framing: length then the body itself.
        let mut pos = 0;
        let len = read_varint_at(&got, &mut pos) as usize;
        let first = &got[pos..pos + len];
        pos += len;
        assert_eq!(
            first[0], SET_COMPRESSION as u8,
            "first frame is Set Compression, unframed by it"
        );

        // Frame two, new framing: length, inflated-length marker, deflated body.
        let len2 = read_varint_at(&got, &mut pos) as usize;
        let mut inner = pos;
        assert_eq!(
            read_varint_at(&got, &mut inner) as usize,
            big.body().len(),
            "second frame carries the inflated size"
        );
        assert!(len2 < 200, "1 KiB of one byte should deflate, got {len2}");
    }

    #[test]
    fn multibyte_length_prefixes_round_trip() {
        // Bodies whose length prefix spans one and two VarInt bytes.
        for body_len in [0usize, 1, 127, 128, 300, 16384] {
            let mut p = PacketOut::new(0x00);
            p.bytes(&vec![0xABu8; body_len]);
            let mut wire = Vec::new();
            p.write_to(&mut wire, None).unwrap();

            let mut cur = std::io::Cursor::new(wire);
            let pkt = read_packet_from(&mut cur, None).unwrap().unwrap();
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
        assert!(read_packet_from(&mut Idle, None).unwrap().is_none());
    }

    #[test]
    fn packet_frames_round_trip() {
        let mut p = PacketOut::new(0x00);
        p.string("hello").u16(25565).var_int(2);
        let mut wire = Vec::new();
        p.write_to(&mut wire, None).unwrap();

        let mut cur = std::io::Cursor::new(wire);
        let pkt = read_packet_from(&mut cur, None).unwrap().unwrap();
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
        let err = read_packet_from(&mut cur, None).unwrap_err();
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
        p.write_to(&mut wire, None).unwrap();

        let mut r = FlakyReader {
            data: wire,
            pos: 0,
            chunk: 3,
            block_next: false,
        };
        let pkt = read_packet_from(&mut r, None).unwrap().unwrap();
        assert_eq!(pkt.id, 0x21);
        let mut pin = PacketIn::new(&pkt.data);
        assert_eq!(
            pin.string().unwrap(),
            "a fairly long payload that spans several reads"
        );
        assert_eq!(pin.i64().unwrap(), 1234567890);
    }
}
