//! # aether-net
//!
//! Phase 0 skeleton for the networking layer. The full Network Gateway and the
//! Internal Binary Protocol arrive in Phase 3; for now this crate carries the
//! one primitive every Minecraft-protocol frame needs — the LEB128-style
//! **VarInt / VarLong** codec — so the rest of the workspace can link against a
//! real `aether-net` and higher layers can be grown incrementally.

/// Errors returned by the VarInt / VarLong decoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarIntError {
    /// The buffer ended before the value was fully decoded.
    UnexpectedEof,
    /// The encoding used more bytes than the target type allows.
    TooLong,
}

impl std::fmt::Display for VarIntError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VarIntError::UnexpectedEof => {
                f.write_str("unexpected end of buffer while reading VarInt")
            }
            VarIntError::TooLong => f.write_str("VarInt is longer than its type permits"),
        }
    }
}

impl std::error::Error for VarIntError {}

/// Append `value` to `out` as a Minecraft-protocol VarInt (max 5 bytes).
pub fn write_varint(value: i32, out: &mut Vec<u8>) {
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

/// Read a VarInt from `buf`, returning the value and the number of bytes read.
pub fn read_varint(buf: &[u8]) -> Result<(i32, usize), VarIntError> {
    let mut result: u32 = 0;
    for i in 0..5 {
        let byte = *buf.get(i).ok_or(VarIntError::UnexpectedEof)?;
        result |= ((byte & 0x7f) as u32) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((result as i32, i + 1));
        }
    }
    Err(VarIntError::TooLong)
}

/// Append `value` to `out` as a Minecraft-protocol VarLong (max 10 bytes).
pub fn write_varlong(value: i64, out: &mut Vec<u8>) {
    let mut v = value as u64;
    loop {
        if v & !0x7f == 0 {
            out.push(v as u8);
            return;
        }
        out.push(((v & 0x7f) | 0x80) as u8);
        v >>= 7;
    }
}

/// Read a VarLong from `buf`, returning the value and the number of bytes read.
pub fn read_varlong(buf: &[u8]) -> Result<(i64, usize), VarIntError> {
    let mut result: u64 = 0;
    for i in 0..10 {
        let byte = *buf.get(i).ok_or(VarIntError::UnexpectedEof)?;
        result |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((result as i64, i + 1));
        }
    }
    Err(VarIntError::TooLong)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_edge_values() {
        for v in [0, 1, 2, 127, 128, 255, 25565, i32::MAX, -1, i32::MIN] {
            let mut buf = Vec::new();
            write_varint(v, &mut buf);
            let (got, n) = read_varint(&buf).unwrap();
            assert_eq!(got, v, "value {v}");
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn known_encodings_match_spec() {
        let mut buf = Vec::new();
        write_varint(0, &mut buf);
        assert_eq!(buf, [0x00]);
        buf.clear();
        write_varint(128, &mut buf);
        assert_eq!(buf, [0x80, 0x01]);
        buf.clear();
        write_varint(-1, &mut buf);
        assert_eq!(buf, [0xff, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn varlong_round_trips() {
        for v in [0i64, 1, 127, 128, i64::MAX, -1, i64::MIN] {
            let mut buf = Vec::new();
            write_varlong(v, &mut buf);
            let (got, n) = read_varlong(&buf).unwrap();
            assert_eq!(got, v);
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn truncated_and_overlong_are_errors() {
        assert_eq!(read_varint(&[]), Err(VarIntError::UnexpectedEof));
        assert_eq!(read_varint(&[0x80]), Err(VarIntError::UnexpectedEof));
        assert_eq!(
            read_varint(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]),
            Err(VarIntError::TooLong)
        );
    }
}
