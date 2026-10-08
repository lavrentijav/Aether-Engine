//! Palette compression for sub-chunk block storage.
//!
//! Most sub-chunks use only a handful of distinct block states, so instead of
//! storing a 16-bit id per block we keep a small **palette** of the distinct
//! ids and a [`PackedArray`] of narrow indices into it. The index width
//! auto-expands `0 → 1 → 2 → 4 → 8 → 16` bits as the palette grows, and
//! [`Palette::compact`] shrinks it back.
//!
//! Width 0 is the common case that matters most for memory: a sub-chunk of a
//! single block — all air, all stone, all water — stores no indices at all.

use crate::block::{BlockProperties, BlockStateId};

/// A tightly packed array of fixed-width unsigned integers.
///
/// The width is one of 0, 1, 2, 4, 8 or 16 bits. Each non-zero width divides
/// 64 evenly, so an entry never straddles a `u64` word — get/set are a single
/// shift-and-mask. Width 0 holds no words: every entry reads as 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedArray {
    bits_per_entry: u8,
    len: usize,
    words: Vec<u64>,
}

/// Whether `bits` is a width a [`PackedArray`] can hold.
#[inline]
pub fn valid_width(bits: u8) -> bool {
    matches!(bits, 0 | 1 | 2 | 4 | 8 | 16)
}

impl PackedArray {
    fn word_count(bits: u8, len: usize) -> usize {
        if bits == 0 {
            0
        } else {
            len.div_ceil(64 / bits as usize)
        }
    }

    /// A zero-filled array of `len` entries at `bits_per_entry` bits each.
    ///
    /// `bits_per_entry` must be 0, 1, 2, 4, 8 or 16.
    pub fn new(bits_per_entry: u8, len: usize) -> Self {
        assert!(
            valid_width(bits_per_entry),
            "bits_per_entry must be 0, 1, 2, 4, 8 or 16, got {bits_per_entry}"
        );
        Self {
            bits_per_entry,
            len,
            words: vec![0u64; Self::word_count(bits_per_entry, len)],
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
        assert!(valid_width(bits_per_entry));
        assert_eq!(
            words.len(),
            Self::word_count(bits_per_entry, len),
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
        let bits = self.bits_per_entry as usize;
        let epw = 64 / bits;
        let word = i / epw;
        let shift = ((i % epw) * bits) as u32;
        (word, shift, (1u64 << bits) - 1)
    }

    /// Read entry `i`.
    #[inline]
    pub fn get(&self, i: usize) -> u16 {
        assert!(i < self.len, "index {i} out of bounds ({})", self.len);
        if self.bits_per_entry == 0 {
            return 0;
        }
        let (word, shift, mask) = self.locate(i);
        ((self.words[word] >> shift) & mask) as u16
    }

    /// Write entry `i`. At width 0 only 0 can be written.
    #[inline]
    pub fn set(&mut self, i: usize, value: u16) {
        assert!(i < self.len, "index {i} out of bounds ({})", self.len);
        if self.bits_per_entry == 0 {
            debug_assert_eq!(value, 0, "a width-0 array holds only zeros");
            return;
        }
        let (word, shift, mask) = self.locate(i);
        let w = &mut self.words[word];
        *w = (*w & !(mask << shift)) | (((value as u64) & mask) << shift);
    }

    /// A copy at `new_bits`, preserving every entry (which must fit).
    fn resized(&self, new_bits: u8) -> PackedArray {
        let mut out = PackedArray::new(new_bits, self.len);
        if self.bits_per_entry != 0 {
            for i in 0..self.len {
                out.set(i, self.get(i));
            }
        }
        out
    }

    /// Heap bytes held by the array.
    pub fn heap_bytes(&self) -> usize {
        self.words.capacity() * 8
    }
}

/// A sub-chunk palette: distinct block states plus their [`BlockProperties`],
/// with an auto-widening [`PackedArray`] of per-block indices.
///
/// Lookups are a linear scan: a palette rarely holds more than a dozen
/// entries, and a scan over that few beats hashing — and costs no table.
#[derive(Debug, Clone)]
pub struct Palette {
    entries: Vec<BlockStateId>,
    props: Vec<BlockProperties>,
    indices: PackedArray,
}

impl Palette {
    /// A palette for `len` blocks, initialised entirely to air (index 0).
    pub fn new(len: usize) -> Self {
        Self::uniform(len, BlockStateId::AIR, BlockProperties::AIR)
    }

    /// A palette for `len` blocks all of `id`, which costs no index storage.
    pub fn uniform(len: usize, id: BlockStateId, props: BlockProperties) -> Self {
        Self {
            entries: vec![id],
            props: vec![props],
            indices: PackedArray::new(0, len),
        }
    }

    /// Number of distinct block states in the palette.
    #[inline]
    pub fn distinct(&self) -> usize {
        self.entries.len()
    }

    /// Current index bit-width (0, 1, 2, 4, 8 or 16).
    #[inline]
    pub fn bits_per_entry(&self) -> u8 {
        self.indices.bits_per_entry()
    }

    /// The minimum index width that can hold `distinct` palette entries.
    fn required_bits(distinct: usize) -> u8 {
        match distinct {
            0 | 1 => 0,
            2 => 1,
            3..=4 => 2,
            5..=16 => 4,
            17..=256 => 8,
            _ => 16,
        }
    }

    /// Get or insert the palette index for `(id, props)`.
    ///
    /// Enforces one consistent id → properties mapping: re-interning a known id
    /// with different properties is a caller bug (masks would then diverge from
    /// the stored palette).
    fn intern(&mut self, id: BlockStateId, props: BlockProperties) -> u16 {
        if let Some(idx) = self.entries.iter().position(|&e| e == id) {
            debug_assert_eq!(
                self.props[idx], props,
                "block id {id:?} interned with conflicting properties"
            );
            return idx as u16;
        }
        let idx = self.entries.len() as u16;
        assert!(idx < 4096, "sub-chunk palette overflow (>4096 states)");
        self.entries.push(id);
        self.props.push(props);

        let needed = Self::required_bits(self.entries.len());
        if needed > self.indices.bits_per_entry() {
            self.indices = self.indices.resized(needed);
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

    /// Distinct palette entries, in index order.
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

    /// The single block filling the whole palette's range, if there is one.
    pub fn uniform_block(&self) -> Option<BlockStateId> {
        if self.indices.bits_per_entry() == 0 {
            return Some(self.entries[0]);
        }
        let first = self.indices.get(0);
        (1..self.indices.len())
            .all(|i| self.indices.get(i) == first)
            .then(|| self.entries[first as usize])
    }

    /// Drop entries no block uses any more and narrow the indices to the
    /// smallest width that holds the rest.
    ///
    /// The palette only ever grows as blocks are written — a sub-chunk dug out
    /// to air still carries the stone it had — so this is what returns a
    /// sub-chunk to its minimal size: run on what the generator produced and
    /// on what is read back from disk.
    pub fn compact(&mut self) {
        let len = self.indices.len();
        let mut used = vec![false; self.entries.len()];
        for i in 0..len {
            used[self.indices.get(i) as usize] = true;
        }
        if len == 0 {
            used[0] = true;
        }
        let kept = used.iter().filter(|&&u| u).count();
        let bits = Self::required_bits(kept);
        if kept == self.entries.len() && bits == self.indices.bits_per_entry() {
            return;
        }
        let mut remap = vec![0u16; self.entries.len()];
        let mut entries = Vec::with_capacity(kept);
        let mut props = Vec::with_capacity(kept);
        for (old, _) in used.iter().enumerate().filter(|(_, &u)| u) {
            remap[old] = entries.len() as u16;
            entries.push(self.entries[old]);
            props.push(self.props[old]);
        }
        let mut indices = PackedArray::new(bits, len);
        if bits != 0 {
            for i in 0..len {
                indices.set(i, remap[self.indices.get(i) as usize]);
            }
        }
        *self = Self {
            entries,
            props,
            indices,
        };
    }

    /// The indices at least `min_bits` wide, for a reader that cannot take
    /// narrower ones.
    pub(crate) fn widened_to(&self, min_bits: u8) -> PackedArray {
        if self.indices.bits_per_entry() >= min_bits {
            self.indices.clone()
        } else {
            self.indices.resized(min_bits)
        }
    }

    /// Heap bytes held by the palette.
    pub fn heap_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<BlockStateId>()
            + self.props.capacity() * std::mem::size_of::<BlockProperties>()
            + self.indices.heap_bytes()
    }

    /// Reassemble a palette from its serialized parts.
    pub fn from_parts(
        entries: Vec<BlockStateId>,
        props: Vec<BlockProperties>,
        indices: PackedArray,
    ) -> Self {
        assert_eq!(entries.len(), props.len(), "entries/props length mismatch");
        assert!(!entries.is_empty(), "palette must hold at least one entry");
        Self {
            entries,
            props,
            indices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_array_round_trips_all_widths() {
        for &bits in &[1u8, 2, 4, 8, 16] {
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
        assert_eq!(p.bits_per_entry(), 0, "all air needs no indices");
        p.set(0, BlockStateId(1), BlockProperties::SOLID);
        assert_eq!(p.bits_per_entry(), 1);
        p.set(1, BlockStateId(2), BlockProperties::SOLID);
        assert_eq!(p.bits_per_entry(), 2);
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
        assert_eq!(p.bits_per_entry(), 2);
    }

    #[test]
    fn compact_drops_unused_entries_and_narrows() {
        let mut p = Palette::new(4096);
        for i in 0..4096 {
            p.set(i, BlockStateId((i % 20) as u32 + 1), BlockProperties::SOLID);
        }
        assert_eq!(p.bits_per_entry(), 8, "air + 20 states");
        // Everything becomes stone: one state left in use.
        for i in 0..4096 {
            p.set(i, BlockStateId(1), BlockProperties::SOLID);
        }
        assert_eq!(p.uniform_block(), Some(BlockStateId(1)));
        p.compact();
        assert_eq!(p.distinct(), 1);
        assert_eq!(p.bits_per_entry(), 0);
        assert_eq!(p.indices().words().len(), 0);
        assert_eq!(p.get(1234), BlockStateId(1));
        assert_eq!(p.props_at(1234), BlockProperties::SOLID);

        // Two states in use: one bit, values intact.
        p.set(7, BlockStateId::AIR, BlockProperties::AIR);
        p.compact();
        assert_eq!(p.bits_per_entry(), 1);
        assert_eq!(p.get(7), BlockStateId::AIR);
        assert_eq!(p.get(8), BlockStateId(1));
    }

    #[test]
    fn compact_keeps_a_minimal_palette_as_it_is() {
        let mut p = Palette::new(4096);
        p.set(3, BlockStateId(5), BlockProperties::SOLID);
        let before = p.clone();
        p.compact();
        assert_eq!(p.entries(), before.entries());
        assert_eq!(p.indices(), before.indices());
    }
}
