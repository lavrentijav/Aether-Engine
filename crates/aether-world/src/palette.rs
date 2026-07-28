//! Palette compression for sub-chunk block storage.
//!
//! Most sub-chunks use only a handful of distinct block states, so instead of
//! storing a 16-bit id per block we keep a small **palette** of the distinct
//! ids and a [`PackedArray`] of narrow indices into it. The index width
//! auto-expands `u4 → u8 → u16` as the palette grows.

use crate::block::{BlockProperties, BlockStateId};
use std::collections::HashMap;

/// A tightly packed array of fixed-width unsigned integers.
///
/// The width is one of 4, 8 or 16 bits, each of which divides 64 evenly, so an
/// entry never straddles a `u64` word — get/set are a single shift-and-mask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedArray {
    bits_per_entry: u8,
    len: usize,
    words: Vec<u64>,
}

impl PackedArray {
    fn entries_per_word(bits: u8) -> usize {
        64 / bits as usize
    }

    /// A zero-filled array of `len` entries at `bits_per_entry` bits each.
    ///
    /// `bits_per_entry` must be 4, 8 or 16.
    pub fn new(bits_per_entry: u8, len: usize) -> Self {
        assert!(
            matches!(bits_per_entry, 4 | 8 | 16),
            "bits_per_entry must be 4, 8 or 16, got {bits_per_entry}"
        );
        let epw = Self::entries_per_word(bits_per_entry);
        let words = vec![0u64; len.div_ceil(epw)];
        Self {
            bits_per_entry,
            len,
            words,
        }
    }

    /// Bits used per entry.
    #[inline]
    pub fn bits_per_entry(&self) -> u8 {
        self.bits_per_entry
    }

    /// Number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the array holds no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Raw backing words (used by the storage serializer).
    #[inline]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// Rebuild directly from raw words (used by the storage deserializer).
    pub fn from_words(bits_per_entry: u8, len: usize, words: Vec<u64>) -> Self {
        assert!(matches!(bits_per_entry, 4 | 8 | 16));
        let epw = Self::entries_per_word(bits_per_entry);
        assert_eq!(
            words.len(),
            len.div_ceil(epw),
            "word count does not match len"
        );
        Self {
            bits_per_entry,
            len,
            words,
        }
    }

    #[inline]
    fn locate(&self, i: usize) -> (usize, u32, u64) {
        let epw = Self::entries_per_word(self.bits_per_entry);
        let word = i / epw;
        let shift = ((i % epw) * self.bits_per_entry as usize) as u32;
        let mask = if self.bits_per_entry == 16 {
            0xffff
        } else {
            (1u64 << self.bits_per_entry) - 1
        };
        (word, shift, mask)
    }

    /// Read entry `i`.
    #[inline]
    pub fn get(&self, i: usize) -> u16 {
        assert!(i < self.len, "index {i} out of bounds ({})", self.len);
        let (word, shift, mask) = self.locate(i);
        ((self.words[word] >> shift) & mask) as u16
    }

    /// Write entry `i`.
    #[inline]
    pub fn set(&mut self, i: usize, value: u16) {
        assert!(i < self.len, "index {i} out of bounds ({})", self.len);
        let (word, shift, mask) = self.locate(i);
        let w = &mut self.words[word];
        *w = (*w & !(mask << shift)) | (((value as u64) & mask) << shift);
    }

    /// Return a copy widened to `new_bits`, preserving every entry.
    fn widened(&self, new_bits: u8) -> PackedArray {
        let mut out = PackedArray::new(new_bits, self.len);
        for i in 0..self.len {
            out.set(i, self.get(i));
        }
        out
    }
}

/// A sub-chunk palette: distinct block states plus their [`BlockProperties`],
/// with an auto-widening [`PackedArray`] of per-block indices.
#[derive(Debug, Clone)]
pub struct Palette {
    entries: Vec<BlockStateId>,
    props: Vec<BlockProperties>,
    lookup: HashMap<BlockStateId, u16>,
    indices: PackedArray,
}

impl Palette {
    /// A palette for `len` blocks, initialised entirely to air (index 0).
    pub fn new(len: usize) -> Self {
        let air = BlockStateId::AIR;
        let mut lookup = HashMap::new();
        lookup.insert(air, 0u16);
        Self {
            entries: vec![air],
            props: vec![BlockProperties::AIR],
            lookup,
            indices: PackedArray::new(4, len),
        }
    }

    /// Number of distinct block states in the palette.
    #[inline]
    pub fn distinct(&self) -> usize {
        self.entries.len()
    }

    /// Current index bit-width (4, 8 or 16).
    #[inline]
    pub fn bits_per_entry(&self) -> u8 {
        self.indices.bits_per_entry()
    }

    /// The minimum index width that can hold `distinct` palette entries.
    fn required_bits(distinct: usize) -> u8 {
        if distinct <= 16 {
            4
        } else if distinct <= 256 {
            8
        } else {
            16
        }
    }

    /// Get or insert the palette index for `(id, props)`.
    ///
    /// Enforces one consistent id → properties mapping: re-interning a known id
    /// with different properties is a caller bug (masks would then diverge from
    /// the stored palette).
    fn intern(&mut self, id: BlockStateId, props: BlockProperties) -> u16 {
        if let Some(&idx) = self.lookup.get(&id) {
            debug_assert_eq!(
                self.props[idx as usize], props,
                "block id {id:?} interned with conflicting properties"
            );
            return idx;
        }
        let idx = self.entries.len() as u16;
        assert!(idx < 4096, "sub-chunk palette overflow (>4096 states)");
        self.entries.push(id);
        self.props.push(props);
        self.lookup.insert(id, idx);

        let needed = Self::required_bits(self.entries.len());
        if needed > self.indices.bits_per_entry() {
            self.indices = self.indices.widened(needed);
        }
        idx
    }

    /// Set block at flat position `pos`, returning its old and new properties.
    pub fn set(
        &mut self,
        pos: usize,
        id: BlockStateId,
        props: BlockProperties,
    ) -> (BlockProperties, BlockProperties) {
        let old_props = self.props[self.indices.get(pos) as usize];
        let idx = self.intern(id, props);
        self.indices.set(pos, idx);
        (old_props, props)
    }

    /// The block state at flat position `pos`.
    #[inline]
    pub fn get(&self, pos: usize) -> BlockStateId {
        self.entries[self.indices.get(pos) as usize]
    }

    /// The properties of the block at flat position `pos`.
    #[inline]
    pub fn props_at(&self, pos: usize) -> BlockProperties {
        self.props[self.indices.get(pos) as usize]
    }

    /// Distinct palette entries, index order (index 0 is always air).
    #[inline]
    pub fn entries(&self) -> &[BlockStateId] {
        &self.entries
    }

    /// Per-entry properties, parallel to [`Palette::entries`].
    #[inline]
    pub fn entry_props(&self) -> &[BlockProperties] {
        &self.props
    }

    /// The packed index array (used by the storage serializer).
    #[inline]
    pub fn indices(&self) -> &PackedArray {
        &self.indices
    }

    /// Reassemble a palette from its serialized parts.
    pub fn from_parts(
        entries: Vec<BlockStateId>,
        props: Vec<BlockProperties>,
        indices: PackedArray,
    ) -> Self {
        assert_eq!(entries.len(), props.len(), "entries/props length mismatch");
        assert!(!entries.is_empty(), "palette must contain at least air");
        let lookup = entries
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u16))
            .collect();
        Self {
            entries,
            props,
            lookup,
            indices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_array_round_trips_all_widths() {
        for &bits in &[4u8, 8, 16] {
            let mut pa = PackedArray::new(bits, 100);
            let cap = (1u32 << bits) - 1;
            for i in 0..100 {
                pa.set(i, (i as u32 % (cap + 1)) as u16);
            }
            for i in 0..100 {
                assert_eq!(
                    pa.get(i),
                    (i as u32 % (cap + 1)) as u16,
                    "bits={bits} i={i}"
                );
            }
        }
    }

    #[test]
    fn palette_auto_expands_width() {
        let mut p = Palette::new(4096);
        assert_eq!(p.bits_per_entry(), 4);
        // air + 15 distinct = 16 entries, still u4.
        for i in 0..15 {
            p.set(i, BlockStateId(i as u32 + 1), BlockProperties::SOLID);
        }
        assert_eq!(p.bits_per_entry(), 4);
        // Grow to air + 255 distinct = 256 entries: still u8.
        for i in 15..255 {
            p.set(i, BlockStateId(i as u32 + 1), BlockProperties::SOLID);
        }
        assert_eq!(p.distinct(), 256);
        assert_eq!(p.bits_per_entry(), 8);
        // The 257th entry forces u16.
        for i in 255..300 {
            p.set(i, BlockStateId(i as u32 + 1), BlockProperties::SOLID);
        }
        assert_eq!(p.bits_per_entry(), 16);
    }

    #[test]
    fn palette_preserves_values_across_widening() {
        let mut p = Palette::new(4096);
        for i in 0..1000 {
            p.set(i, BlockStateId(i as u32 + 1), BlockProperties::SOLID);
        }
        for i in 0..1000 {
            assert_eq!(p.get(i), BlockStateId(i as u32 + 1));
            assert_eq!(p.props_at(i), BlockProperties::SOLID);
        }
    }

    #[test]
    fn dedup_keeps_palette_small() {
        let mut p = Palette::new(4096);
        for i in 0..4096 {
            let id = BlockStateId((i % 3) as u32 + 1);
            p.set(i, id, BlockProperties::SOLID);
        }
        // air + 3 distinct = 4 entries.
        assert_eq!(p.distinct(), 4);
        assert_eq!(p.bits_per_entry(), 4);
    }
}
