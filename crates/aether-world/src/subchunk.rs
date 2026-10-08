//! The 16×16×16 Sub-Chunk: palette-compressed block ids, addressed in Morton
//! (Z-order) so neighbouring blocks stay close in memory, with
//! Structure-of-Arrays bit masks derived from the palette on request.

use crate::block::{BlockProperties, BlockStateId};
use crate::palette::Palette;
use aether_core::morton::morton_encode_16;

/// Edge length of a sub-chunk.
pub const DIM: usize = 16;
/// Blocks in a sub-chunk (`16³`).
pub const VOLUME: usize = DIM * DIM * DIM; // 4096
/// `u64` words needed for one 4096-bit SoA mask.
pub const MASK_WORDS: usize = VOLUME / 64; // 64

/// One Structure-of-Arrays property mask over a sub-chunk: 4096 bits.
///
/// Indexed by Morton position; word `m>>6`, bit `m&63`. Combining masks is
/// what [`aether_core::simd`] accelerates.
#[derive(Clone, PartialEq, Eq)]
pub struct Mask(pub [u64; MASK_WORDS]);

impl Mask {
    /// An all-zero mask.
    pub const fn zeroed() -> Self {
        Mask([0; MASK_WORDS])
    }

    /// Read the bit at Morton index `m`.
    #[inline]
    pub fn get(&self, m: u16) -> bool {
        let m = m as usize;
        (self.0[m >> 6] >> (m & 63)) & 1 != 0
    }

    /// Write the bit at Morton index `m`.
    #[inline]
    pub fn set(&mut self, m: u16, value: bool) {
        let m = m as usize;
        let bit = 1u64 << (m & 63);
        if value {
            self.0[m >> 6] |= bit;
        } else {
            self.0[m >> 6] &= !bit;
        }
    }

    /// Number of set bits.
    #[inline]
    pub fn count(&self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }
}

impl std::fmt::Debug for Mask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mask({} set)", self.count())
    }
}

/// A 16×16×16 volume of blocks in the engine's SoA memory layout.
///
/// Only the palette is stored. The property masks are derived from it on
/// request: each palette entry carries its block's properties, so a mask is
/// one pass over the indices, while storing three of them cost 1.5 KiB per
/// sub-chunk — more than the blocks themselves for most of a world.
#[derive(Debug, Clone)]
pub struct SubChunk {
    palette: Palette,
}

impl SubChunk {
    /// An empty (all-air) sub-chunk.
    pub fn new() -> Self {
        Self {
            palette: Palette::new(VOLUME),
        }
    }

    /// A sub-chunk filled with one block, which costs no index storage.
    pub fn filled(id: BlockStateId, props: BlockProperties) -> Self {
        Self {
            palette: Palette::uniform(VOLUME, id, props),
        }
    }

    /// Morton index of local `(x, y, z)`, each `0..16`.
    #[inline]
    pub fn index(x: usize, y: usize, z: usize) -> u16 {
        debug_assert!(x < DIM && y < DIM && z < DIM);
        morton_encode_16(x as u8, y as u8, z as u8)
    }

    /// Read the block state at local `(x, y, z)`.
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockStateId {
        self.palette.get(Self::index(x, y, z) as usize)
    }

    /// The properties of the block at local `(x, y, z)`.
    #[inline]
    pub fn props(&self, x: usize, y: usize, z: usize) -> BlockProperties {
        self.palette.props_at(Self::index(x, y, z) as usize)
    }

    /// Set the block at local `(x, y, z)` to `id` with the given `props`.
    pub fn set(&mut self, x: usize, y: usize, z: usize, id: BlockStateId, props: BlockProperties) {
        let m = Self::index(x, y, z);
        self.palette.set(m as usize, id, props);
    }

    /// A mask of the blocks whose properties satisfy `pick`.
    pub fn mask_of(&self, pick: impl Fn(&BlockProperties) -> bool) -> Mask {
        let props = self.palette.entry_props();
        let mut mask = Mask::zeroed();
        if self.palette.bits_per_entry() == 0 {
            if pick(&props[0]) {
                mask.0 = [u64::MAX; MASK_WORDS];
            }
            return mask;
        }
        let picked: Vec<bool> = props.iter().map(&pick).collect();
        if !picked.iter().any(|&p| p) {
            return mask;
        }
        let indices = self.palette.indices();
        for m in 0..VOLUME {
            if picked[indices.get(m) as usize] {
                mask.0[m >> 6] |= 1 << (m & 63);
            }
        }
        mask
    }

    /// The `SolidMask`.
    pub fn solid_mask(&self) -> Mask {
        self.mask_of(|p| p.solid)
    }
    /// The collision mask.
    pub fn collision_mask(&self) -> Mask {
        self.mask_of(|p| p.collision)
    }
    /// The redstone-flags mask.
    pub fn redstone_mask(&self) -> Mask {
        self.mask_of(|p| p.redstone)
    }

    /// The palette (block ids + packed indices) — used by the serializer.
    #[inline]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Whether the sub-chunk contains only air.
    pub fn is_empty(&self) -> bool {
        self.palette.uniform_block() == Some(BlockStateId::AIR)
    }

    /// Shrink to the minimal palette and index width: see
    /// [`Palette::compact`]. A sub-chunk of one block then holds no indices.
    pub fn compact(&mut self) {
        self.palette.compact();
    }

    /// Bytes this sub-chunk occupies, inline and on the heap.
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.palette.heap_bytes()
    }

    /// Reassemble a sub-chunk from a palette. Used by the storage
    /// deserializer.
    pub fn from_palette(palette: Palette) -> Self {
        Self { palette }
    }
}

impl Default for SubChunk {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_subchunk_is_all_air() {
        let sc = SubChunk::new();
        assert!(sc.is_empty());
        assert_eq!(sc.get(0, 0, 0), BlockStateId::AIR);
        assert_eq!(sc.solid_mask().count(), 0);
    }

    #[test]
    fn set_updates_block_and_masks() {
        let mut sc = SubChunk::new();
        let stone = BlockStateId(1);
        sc.set(1, 2, 3, stone, BlockProperties::SOLID);
        assert_eq!(sc.get(1, 2, 3), stone);
        let m = SubChunk::index(1, 2, 3);
        assert!(sc.solid_mask().get(m));
        assert!(sc.collision_mask().get(m));
        assert!(!sc.redstone_mask().get(m));
        assert_eq!(sc.solid_mask().count(), 1);
        assert!(!sc.is_empty());
    }

    #[test]
    fn overwriting_with_air_clears_masks() {
        let mut sc = SubChunk::new();
        sc.set(5, 5, 5, BlockStateId(1), BlockProperties::SOLID);
        assert_eq!(sc.solid_mask().count(), 1);
        sc.set(5, 5, 5, BlockStateId::AIR, BlockProperties::AIR);
        assert_eq!(sc.solid_mask().count(), 0);
    }

    #[test]
    fn a_uniform_sub_chunk_holds_no_indices_and_masks_still_work() {
        let mut sc = SubChunk::new();
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    sc.set(x, y, z, BlockStateId(1), BlockProperties::SOLID);
                }
            }
        }
        sc.compact();
        assert_eq!(sc.palette().indices().words().len(), 0);
        assert_eq!(sc.solid_mask().count(), 4096);
        assert_eq!(sc.redstone_mask().count(), 0);
        assert!(sc.memory_bytes() < 200, "{} bytes", sc.memory_bytes());
        assert_eq!(
            SubChunk::filled(BlockStateId(1), BlockProperties::SOLID).get(3, 4, 5),
            BlockStateId(1)
        );
    }

    #[test]
    fn redstone_property_maps_to_redstone_mask_only() {
        let mut sc = SubChunk::new();
        let wire = BlockStateId(55);
        let props = BlockProperties {
            solid: false,
            collision: false,
            redstone: true,
            light_emission: 0,
            light_opacity: 15,
        };
        sc.set(0, 0, 0, wire, props);
        let m = SubChunk::index(0, 0, 0);
        assert!(sc.redstone_mask().get(m));
        assert!(!sc.solid_mask().get(m));
    }
}
