//! Decode a vanilla **network chunk section** (paletted container) into an
//! engine [`SubChunk`].
//!
//! The wire layout for a block-states section (Minecraft 1.18 – 1.20.1) is:
//!
//! ```text
//! block_count   : i16                     (non-air blocks; advisory)
//! bits_per_entry: u8
//! palette:
//!   bits == 0            -> value: VarInt                    (single-valued)
//!   1 <= bits <= 8       -> len: VarInt, ids: VarInt[len]    (indirect)
//!   bits >= 9            -> (none)                           (direct: raw ids)
//! data_len      : VarInt                  (number of u64 words)
//! data          : u64[data_len]           (big-endian, entries do not span words)
//! ```
//!
//! Entries are ordered **YZX** (`index = y*256 + z*16 + x`).

use crate::vanilla_registry::VanillaRegistry;
use aether_net::read_varint;
use aether_world::SubChunk;

/// Errors from decoding a vanilla chunk section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    /// Ran out of bytes mid-field.
    UnexpectedEof,
    /// A VarInt was malformed.
    BadVarInt,
    /// `bits_per_entry` or a palette length was invalid.
    Invalid(&'static str),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::UnexpectedEof => f.write_str("unexpected end of chunk-section buffer"),
            ProxyError::BadVarInt => f.write_str("malformed VarInt in chunk section"),
            ProxyError::Invalid(w) => write!(f, "invalid chunk section field: {w}"),
        }
    }
}

impl std::error::Error for ProxyError {}

/// A cursor over the section byte buffer.
pub struct SectionReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> SectionReader<'a> {
    /// Wrap a buffer.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes consumed so far (lets a caller decode consecutive sections).
    pub fn position(&self) -> usize {
        self.pos
    }

    fn u8(&mut self) -> Result<u8, ProxyError> {
        let b = *self.buf.get(self.pos).ok_or(ProxyError::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    fn i16(&mut self) -> Result<i16, ProxyError> {
        let end = self.pos + 2;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(ProxyError::UnexpectedEof)?;
        self.pos = end;
        Ok(i16::from_be_bytes(s.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, ProxyError> {
        let end = self.pos + 8;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(ProxyError::UnexpectedEof)?;
        self.pos = end;
        Ok(u64::from_be_bytes(s.try_into().unwrap()))
    }

    fn varint(&mut self) -> Result<u32, ProxyError> {
        let (v, n) = read_varint(&self.buf[self.pos..]).map_err(|_| ProxyError::BadVarInt)?;
        self.pos += n;
        Ok(v as u32)
    }

    /// Decode one block-states section into a [`SubChunk`], advancing the cursor
    /// past it (so biome data or the next section follows immediately).
    pub fn read_block_states(&mut self, reg: &VanillaRegistry) -> Result<SubChunk, ProxyError> {
        let _block_count = self.i16()?; // advisory non-air count
        let bits = self.u8()?;

        // Read the palette (if any).
        let palette: Option<Vec<u32>> = match bits {
            0 => Some(vec![self.varint()?]),
            1..=8 => {
                let len = self.varint()? as usize;
                if len == 0 || len > 4096 {
                    return Err(ProxyError::Invalid("palette length"));
                }
                let mut p = Vec::with_capacity(len);
                for _ in 0..len {
                    p.push(self.varint()?);
                }
                Some(p)
            }
            9..=32 => None, // direct: values are raw state ids
            _ => return Err(ProxyError::Invalid("bits_per_entry")),
        };

        let data_len = self.varint()? as usize;
        let mut data = Vec::with_capacity(data_len);
        for _ in 0..data_len {
            data.push(self.u64()?);
        }

        let mut sc = SubChunk::new();

        if bits == 0 {
            // Single-valued: the whole section is one state.
            let (id, props) = reg.resolve(palette.as_ref().unwrap()[0]);
            if id != aether_world::BlockStateId::AIR {
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            sc.set(x, y, z, id, props);
                        }
                    }
                }
            }
            return Ok(sc);
        }

        if data_len == 0 {
            return Err(ProxyError::Invalid("missing data array"));
        }

        let per_long = 64 / bits as usize;
        let mask = (1u64 << bits) - 1;
        for i in 0..4096usize {
            let long_idx = i / per_long;
            let within = i % per_long;
            let raw = match data.get(long_idx) {
                Some(&w) => (w >> (within * bits as usize)) & mask,
                None => 0,
            } as u32;

            let state_id = match &palette {
                Some(p) => match p.get(raw as usize) {
                    Some(&s) => s,
                    None => continue, // corrupt index -> leave air
                },
                None => raw, // direct
            };

            let (id, props) = reg.resolve(state_id);
            if id == aether_world::BlockStateId::AIR {
                continue;
            }
            let x = i & 15;
            let z = (i >> 4) & 15;
            let y = (i >> 8) & 15;
            sc.set(x, y, z, id, props);
        }

        Ok(sc)
    }
}

/// Decode a single block-states section from `buf` (convenience wrapper).
pub fn decode_section(buf: &[u8], reg: &VanillaRegistry) -> Result<SubChunk, ProxyError> {
    SectionReader::new(buf).read_block_states(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_net::write_varint;
    use aether_world::BlockStateId;

    /// Encode an indirect block-states section: palette `[air, stone]`,
    /// bits=4, with `set` giving the block indices that should be stone.
    fn encode_indirect(set: &[(usize, usize, usize)]) -> Vec<u8> {
        let bits = 4usize;
        let per_long = 64 / bits;
        let words = 4096 / per_long; // 256
        let mut data = vec![0u64; words];
        for &(x, y, z) in set {
            let i = y * 256 + z * 16 + x;
            let long_idx = i / per_long;
            let within = i % per_long;
            data[long_idx] |= 1u64 << (within * bits); // palette index 1 = stone
        }

        let mut buf = Vec::new();
        buf.extend_from_slice(&(set.len() as i16).to_be_bytes()); // block_count
        buf.push(bits as u8);
        write_varint(2, &mut buf); // palette len
        write_varint(0, &mut buf); // air state id
        write_varint(1, &mut buf); // stone state id
        write_varint(words as i32, &mut buf); // data length
        for w in data {
            // Data words are fixed 8-byte big-endian, not VarInts.
            buf.extend_from_slice(&w.to_be_bytes());
        }
        buf
    }

    fn registry() -> VanillaRegistry {
        let mut r = VanillaRegistry::new();
        r.register_all([(0u32, "minecraft:air"), (1u32, "minecraft:stone")]);
        r
    }

    #[test]
    fn decodes_indirect_section() {
        let buf = encode_indirect(&[(0, 0, 0), (1, 2, 3), (15, 15, 15)]);
        let sc = decode_section(&buf, &registry()).unwrap();
        assert_ne!(sc.get(0, 0, 0), BlockStateId::AIR);
        assert_ne!(sc.get(1, 2, 3), BlockStateId::AIR);
        assert_ne!(sc.get(15, 15, 15), BlockStateId::AIR);
        assert_eq!(sc.get(2, 0, 0), BlockStateId::AIR);
        assert_eq!(sc.solid_mask().count(), 3);
    }

    #[test]
    fn decodes_single_valued_section() {
        // bits=0, single value = stone(1).
        let mut buf = Vec::new();
        buf.extend_from_slice(&4096i16.to_be_bytes());
        buf.push(0);
        write_varint(1, &mut buf); // stone
        write_varint(0, &mut buf); // data length 0
        let sc = decode_section(&buf, &registry()).unwrap();
        assert_eq!(sc.solid_mask().count(), 4096);
    }

    #[test]
    fn single_valued_air_is_empty() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0i16.to_be_bytes());
        buf.push(0);
        write_varint(0, &mut buf); // air
        write_varint(0, &mut buf);
        let sc = decode_section(&buf, &registry()).unwrap();
        assert!(sc.is_empty());
    }

    #[test]
    fn truncated_buffer_errors() {
        assert_eq!(
            decode_section(&[0x00], &registry()).unwrap_err(),
            ProxyError::UnexpectedEof
        );
    }
}
