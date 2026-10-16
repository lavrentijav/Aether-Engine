//! The wire primitives the two voice protocols share with the game.
//!
//! Both Simple Voice Chat and Plasmo Voice build their packets out of the same
//! Minecraft `FriendlyByteBuf` primitives — VarInt-prefixed byte arrays and
//! strings, big-endian integers, bare 128-bit UUIDs — so the readers and
//! writers live here once and are reused by both protocol modules.
//!
//! They are deliberately fallible: a voice packet arrives over an untrusted UDP
//! socket from the open internet, and a truncated or hostile length prefix must
//! be a decode error, never a panic or an allocation the sender chooses.

/// A cursor over a byte slice.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    /// Remaining bytes.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.remaining() < n {
            return Err(format!(
                "truncated packet: wanted {n} bytes, {} left",
                self.remaining()
            ));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Take exactly `n` bytes (a public wrapper for protocol bodies that carry
    /// an explicit length).
    pub fn take_n(&mut self, n: usize) -> Result<Vec<u8>, String> {
        Ok(self.take(n)?.to_vec())
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn i16(&mut self) -> Result<i16, String> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i64(&mut self) -> Result<i64, String> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes(b.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.i32()? as u32))
    }

    pub fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.i64()? as u64))
    }

    pub fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }

    /// A Minecraft VarInt, at most five bytes as the game writes it.
    pub fn var_int(&mut self) -> Result<i32, String> {
        let mut value: u32 = 0;
        let mut shift = 0;
        for _ in 0..5 {
            let byte = self.u8()?;
            value |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
            shift += 7;
        }
        Err("var-int longer than five bytes".to_string())
    }

    /// A bare 128-bit UUID, two big-endian `i64`s as the game writes it.
    pub fn uuid(&mut self) -> Result<u128, String> {
        let b = self.take(16)?;
        Ok(u128::from_be_bytes(b.try_into().unwrap()))
    }

    /// A 16-byte Simple Voice Chat secret.
    pub fn secret16(&mut self) -> Result<[u8; 16], String> {
        let b = self.take(16)?;
        Ok(b.try_into().unwrap())
    }

    /// A VarInt-prefixed byte array. `max` bounds the accepted length so a
    /// hostile prefix cannot make the server allocate (or scan) unboundedly.
    pub fn byte_array(&mut self, max: usize) -> Result<Vec<u8>, String> {
        let len = self.var_int()?;
        if len < 0 || len as usize > max {
            return Err(format!("byte-array length {len} outside 0..={max}"));
        }
        Ok(self.take(len as usize)?.to_vec())
    }

    /// The rest of the buffer.
    pub fn rest(&mut self) -> Vec<u8> {
        let out = self.buf[self.pos..].to_vec();
        self.pos = self.buf.len();
        out
    }

    /// A `u16`-prefixed (`writeUTF`) string: two-byte big-endian length then the
    /// UTF-8 bytes, as `DataOutputStream.writeUTF` writes it (Java's modified
    /// UTF-8; ASCII and BMP text are identical).
    pub fn utf(&mut self, max: usize) -> Result<String, String> {
        let len = self.u16()? as usize;
        if len > max {
            return Err(format!("utf length {len} above {max}"));
        }
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|_| "invalid utf-8".to_string())
    }
}

/// A growable packet buffer.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Writer {
        Writer {
            buf: Vec::with_capacity(n),
        }
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.i32(v.to_bits() as i32)
    }

    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.i64(v.to_bits() as i64)
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    pub fn var_int(&mut self, v: i32) -> &mut Self {
        let mut u = v as u32;
        loop {
            if u & !0x7f == 0 {
                self.buf.push(u as u8);
                break;
            }
            self.buf.push(((u & 0x7f) | 0x80) as u8);
            u >>= 7;
        }
        self
    }

    pub fn uuid(&mut self, v: u128) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn secret16(&mut self, v: &[u8; 16]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub fn byte_array(&mut self, v: &[u8]) -> &mut Self {
        self.var_int(v.len() as i32);
        self.buf.extend_from_slice(v);
        self
    }

    pub fn utf(&mut self, s: &str) -> &mut Self {
        self.u16(s.len() as u16);
        self.buf.extend_from_slice(s.as_bytes());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_int_round_trips_the_edges() {
        for v in [0, 1, 127, 128, 255, 300, 16383, 16384, i32::MAX] {
            let mut w = Writer::new();
            w.var_int(v);
            let mut r = Reader::new(w.as_slice());
            assert_eq!(r.var_int().unwrap(), v, "round-trip {v}");
            assert!(r.is_empty());
        }
    }

    #[test]
    fn var_int_rejects_an_unterminated_prefix() {
        let bytes = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x01];
        assert!(Reader::new(&bytes).var_int().is_err());
    }

    #[test]
    fn a_byte_array_cannot_exceed_its_bound() {
        let mut w = Writer::new();
        w.byte_array(&[1, 2, 3]);
        let mut r = Reader::new(w.as_slice());
        assert!(r.byte_array(2).is_err());
    }

    #[test]
    fn utf_uses_a_big_endian_length() {
        let mut w = Writer::new();
        w.utf("voicechat:secret");
        assert_eq!(&w.as_slice()[..2], &[0x00, 0x10]);
        let mut r = Reader::new(w.as_slice());
        assert_eq!(r.utf(64).unwrap(), "voicechat:secret");
    }
}
