//! On-disk sub-chunk blob format (version 1) and the KV key encoding.
//!
//! This module is intentionally free of any backend or compression concerns so
//! the exact same wire layout can be shared with `aether-convert`. A blob is:
//!
//! ```text
//! magic     : b"ASC1"                    (4 bytes)
//! entries   : u16 LE                     (palette size, incl. air at 0)
//!   id      : u32 LE                      × entries
//!   flags   : u8  (bit0 solid, bit1 collision, bit2 redstone)  × entries
//! bits      : u8   (index width: written as 4 / 8 / 16; 0 / 1 / 2 also read)
//! words     : u16 LE                     (packed-index word count)
//!   word    : u64 LE                      × words
//! ```
//!
//! The block count is fixed at [`VOLUME`](crate::subchunk::VOLUME), so it is not
//! stored.

use crate::block::{BlockProperties, BlockStateId};
use crate::palette::{PackedArray, Palette};
use crate::subchunk::{SubChunk, VOLUME};

/// Magic prefix identifying an Aether Sub-Chunk blob, version 1.
pub const MAGIC: [u8; 4] = *b"ASC2";

/// The previous magic, which held no light data.
///
/// Kept named rather than forgotten so the error a reader gets is "this is an
/// older sub-chunk", not "this is not a sub-chunk".
pub const MAGIC_V1: [u8; 4] = *b"ASC1";

/// Errors from (de)serializing a sub-chunk blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The blob did not start with the expected [`MAGIC`].
    BadMagic,
    /// The blob is an older sub-chunk format this build cannot read.
    ///
    /// Distinct from [`FormatError::BadMagic`] on purpose: "your world is from
    /// an older build" is a thing an operator can act on, and "this is not a
    /// sub-chunk at all" is a thing they should worry about.
    OldFormat,
    /// The blob ended before all declared fields were read.
    Truncated,
    /// A field held a value outside its valid range.
    Invalid(&'static str),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::BadMagic => f.write_str("not an Aether sub-chunk blob (bad magic)"),
            FormatError::OldFormat => f.write_str(
                "sub-chunk written by an older build (no light data); the world must be \
                 regenerated or converted",
            ),
            FormatError::Truncated => f.write_str("sub-chunk blob is truncated"),
            FormatError::Invalid(what) => write!(f, "invalid sub-chunk blob field: {what}"),
        }
    }
}

impl std::error::Error for FormatError {}

fn flags_of(p: BlockProperties) -> u8 {
    (p.solid as u8) | ((p.collision as u8) << 1) | ((p.redstone as u8) << 2)
}

/// Light packed into one byte: emission in the high nibble, opacity in the low.
///
/// Both are `0..=15` by definition, so a nibble each is exact rather than a
/// compression. Stored alongside the flags — and therefore duplicated from the
/// registry, which is the authority — so that a blob can be read back without
/// one. `aether-convert` and every format test depend on that being true.
fn light_of(p: BlockProperties) -> u8 {
    (p.light_emission.min(15) << 4) | p.light_opacity.min(15)
}

fn props_of(flags: u8, light: u8) -> BlockProperties {
    BlockProperties {
        solid: flags & 1 != 0,
        collision: flags & 2 != 0,
        redstone: flags & 4 != 0,
        light_emission: light >> 4,
        light_opacity: light & 0x0F,
    }
}

/// Serialize a sub-chunk to its raw (uncompressed) blob form.
pub fn serialize_subchunk(sc: &SubChunk) -> Vec<u8> {
    let palette = sc.palette();
    let entries = palette.entries();
    let props = palette.entry_props();
    // Narrower indices are an in-memory form only. A blob keeps the widths
    // every build reads (4, 8, 16), so a world saved by this build still opens
    // under an older one; on disk the difference is a few hundred bytes the
    // backend's compression mostly takes back.
    let indices = palette.widened_to(4);

    let mut out = Vec::with_capacity(4 + 2 + entries.len() * 6 + 4 + indices.words().len() * 8);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for id in entries {
        out.extend_from_slice(&id.raw().to_le_bytes());
    }
    for p in props {
        out.push(flags_of(*p));
        out.push(light_of(*p));
    }
    out.push(indices.bits_per_entry());
    out.extend_from_slice(&(indices.words().len() as u16).to_le_bytes());
    for w in indices.words() {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        let end = self.pos.checked_add(n).ok_or(FormatError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(FormatError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// Deserialize a raw (uncompressed) blob back into a sub-chunk.
pub fn deserialize_subchunk(buf: &[u8]) -> Result<SubChunk, FormatError> {
    let mut r = Reader { buf, pos: 0 };
    let magic = r.take(4)?;
    if magic == MAGIC_V1 {
        return Err(FormatError::OldFormat);
    }
    if magic != MAGIC {
        return Err(FormatError::BadMagic);
    }
    let entry_count = r.u16()? as usize;
    if entry_count == 0 {
        return Err(FormatError::Invalid("empty palette"));
    }
    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        entries.push(BlockStateId(r.u32()?));
    }
    let mut props = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let flags = r.u8()?;
        props.push(props_of(flags, r.u8()?));
    }
    let bits = r.u8()?;
    if !crate::palette::valid_width(bits) {
        return Err(FormatError::Invalid("bits_per_entry"));
    }
    let word_count = r.u16()? as usize;
    let expected = if bits == 0 {
        0
    } else {
        VOLUME.div_ceil(64 / bits as usize)
    };
    if word_count != expected {
        return Err(FormatError::Invalid("packed word count"));
    }
    let mut words = Vec::with_capacity(word_count);
    for _ in 0..word_count {
        words.push(r.u64()?);
    }
    let indices = PackedArray::from_words(bits, VOLUME, words);
    if (0..VOLUME).any(|i| indices.get(i) as usize >= entries.len()) {
        return Err(FormatError::Invalid("palette index out of range"));
    }
    let palette = Palette::from_parts(entries, props, indices);
    let mut sc = SubChunk::from_palette(palette);
    sc.compact();
    Ok(sc)
}

/// Coordinates of a sub-chunk within a world: chunk column `(cx, cz)` and the
/// vertical sub-chunk index `cy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubChunkKey {
    /// Chunk X (column).
    pub cx: i32,
    /// Vertical sub-chunk index (world height / 16).
    pub cy: i8,
    /// Chunk Z (column).
    pub cz: i32,
}

impl SubChunkKey {
    /// Construct a key.
    pub const fn new(cx: i32, cy: i8, cz: i32) -> Self {
        Self { cx, cy, cz }
    }

    /// Encode to a 9-byte, order-preserving KV key.
    ///
    /// The sign bit of each signed field is flipped so that lexicographic byte
    /// order matches numeric order — range scans over a region stay contiguous
    /// in the LSM tree.
    pub fn encode(&self) -> [u8; 9] {
        let mut k = [0u8; 9];
        k[0..4].copy_from_slice(&(self.cx as u32 ^ 0x8000_0000).to_be_bytes());
        k[4..8].copy_from_slice(&(self.cz as u32 ^ 0x8000_0000).to_be_bytes());
        k[8] = (self.cy as u8) ^ 0x80;
        k
    }

    /// Decode a key produced by [`SubChunkKey::encode`].
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 9 {
            return None;
        }
        let cx = (u32::from_be_bytes(bytes[0..4].try_into().unwrap()) ^ 0x8000_0000) as i32;
        let cz = (u32::from_be_bytes(bytes[4..8].try_into().unwrap()) ^ 0x8000_0000) as i32;
        let cy = (bytes[8] ^ 0x80) as i8;
        Some(Self { cx, cy, cz })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trips() {
        let mut sc = SubChunk::new();
        sc.set(0, 0, 0, BlockStateId(1), BlockProperties::SOLID);
        sc.set(
            15,
            15,
            15,
            BlockStateId(55),
            BlockProperties {
                solid: false,
                collision: false,
                redstone: true,
                light_emission: 0,
                light_opacity: 15,
            },
        );
        let blob = serialize_subchunk(&sc);
        let back = deserialize_subchunk(&blob).unwrap();
        assert_eq!(back.get(0, 0, 0), BlockStateId(1));
        assert_eq!(back.get(15, 15, 15), BlockStateId(55));
        assert_eq!(back.solid_mask(), sc.solid_mask());
        assert_eq!(back.redstone_mask(), sc.redstone_mask());
    }

    #[test]
    fn bad_magic_and_truncation_are_errors() {
        assert_eq!(
            deserialize_subchunk(b"XXXX").unwrap_err(),
            FormatError::BadMagic
        );
        // An older sub-chunk is named as such rather than as garbage: the two
        // call for very different reactions from whoever reads the error.
        assert_eq!(
            deserialize_subchunk(b"ASC1").unwrap_err(),
            FormatError::OldFormat
        );
        assert_eq!(
            deserialize_subchunk(b"ASC2").unwrap_err(),
            FormatError::Truncated
        );
    }

    #[test]
    fn key_encoding_is_order_preserving() {
        let a = SubChunkKey::new(-10, -1, 5).encode();
        let b = SubChunkKey::new(-10, 0, 5).encode();
        let c = SubChunkKey::new(-9, -1, 5).encode();
        assert!(a < b, "cy ordering");
        assert!(b < c, "cx ordering dominates");
        assert!(SubChunkKey::new(-1, 0, 0).encode() < SubChunkKey::new(1, 0, 0).encode());
    }

    #[test]
    fn key_round_trips() {
        for k in [
            SubChunkKey::new(0, 0, 0),
            SubChunkKey::new(-2048, -4, 2048),
            SubChunkKey::new(i32::MIN, i8::MIN, i32::MAX),
        ] {
            assert_eq!(SubChunkKey::decode(&k.encode()), Some(k));
        }
    }
}
