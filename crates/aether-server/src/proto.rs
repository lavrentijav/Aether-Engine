//! Minecraft protocol wire primitives (uncompressed framing).
//!
//! Packets are `VarInt length | VarInt id | data`. We never enable compression
//! or encryption, which a vanilla 1.8.9 client accepts for offline play.

use std::io::{self, Read, Write};

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

/// Read a VarInt directly from a stream, one byte at a time.
///
/// Returns `Ok(None)` if the very first byte read hits a read timeout /
/// `WouldBlock` — used by the play loop to interleave keep-alives.
fn read_var_int_stream<R: Read>(r: &mut R) -> io::Result<Option<i32>> {
    let mut result: u32 = 0;
    for i in 0..5 {
        let mut byte = [0u8; 1];
        match r.read(&mut byte) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof")),
            Ok(_) => {}
            Err(e)
                if i == 0
                    && matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
        result |= ((byte[0] & 0x7f) as u32) << (7 * i);
        if byte[0] & 0x80 == 0 {
            return Ok(Some(result as i32));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "varint too long",
    ))
}

/// A decoded packet: its id and payload (id VarInt already stripped).
pub struct RawPacket {
    /// Packet id.
    pub id: i32,
    /// Payload after the id.
    pub data: Vec<u8>,
}

/// Read a full packet frame from `r`.
///
/// `Ok(None)` means the read timed out with no bytes available yet (the caller
/// can send a keep-alive and retry). Any real short-read is an error.
pub fn read_packet<R: Read>(r: &mut R) -> io::Result<Option<RawPacket>> {
    let len = match read_var_int_stream(r)? {
        Some(l) if l >= 0 => l as usize,
        Some(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "neg len")),
        None => return Ok(None),
    };
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let mut pin = PacketIn::new(&buf);
    let id = pin.var_int()?;
    let data = buf[pin.pos..].to_vec();
    Ok(Some(RawPacket { id, data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip_through_stream() {
        for v in [0i32, 1, 127, 128, 25565, 2_097_151, i32::MAX, -1] {
            let mut out = Vec::new();
            write_var_int(&mut out, v);
            let mut cur = std::io::Cursor::new(out);
            assert_eq!(read_var_int_stream(&mut cur).unwrap(), Some(v));
        }
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
}
