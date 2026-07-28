//! The 16×16×16 Sub-Chunk: palette-compressed block ids plus Structure-of-Arrays
//! bit masks, all addressed in Morton (Z-order) so neighbouring blocks stay
//! close in memory.

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
#[derive(Debug, Clone)]
pub struct SubChunk {
    palette: Palette,
    solid: Mask,
    collision: Mask,
    redstone: Mask,
}

impl SubChunk {
    /// An empty (all-air) sub-chunk.
    pub fn new() -> Self {
        Self {
            palette: Palette::new(VOLUME),
            solid: Mask::zeroed(),
            collision: Mask::zeroed(),
            redstone: Mask::zeroed(),
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

    /// Set the block at local `(x, y, z)` to `id` with the given `props`,
    /// updating every SoA mask accordingly.
    pub fn set(&mut self, x: usize, y: usize, z: usize, id: BlockStateId, props: BlockProperties) {
        let m = Self::index(x, y, z);
        self.palette.set(m as usize, id, props);
        self.solid.set(m, props.solid);
        self.collision.set(m, props.collision);
        self.redstone.set(m, props.redstone);
    }

    /// The `SolidMask`.
    #[inline]
    pub fn solid_mask(&self) -> &Mask {
        &self.solid
    }
    /// The collision mask.
    #[inline]
    pub fn collision_mask(&self) -> &Mask {
        &self.collision
    }
    /// The redstone-flags mask.
    #[inline]
    pub fn redstone_mask(&self) -> &Mask {
        &self.redstone
    }

    /// The palette (block ids + packed indices) — used by the serializer.
    #[inline]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Whether the sub-chunk contains only air.
    pub fn is_empty(&self) -> bool {
        self.palette.distinct() == 1 && self.solid.count() == 0
    }

    /// Reassemble a sub-chunk from a palette, recomputing masks from its
    /// per-block properties. Used by the storage deserializer.
    pub fn from_palette(palette: Palette) -> Self {
        let mut solid = Mask::zeroed();
        let mut collision = Mask::zeroed();
        let mut redstone = Mask::zeroed();
        for m in 0..VOLUME {
            let p = palette.props_at(m);
            solid.set(m as u16, p.solid);
            collision.set(m as u16, p.collision);
            redstone.set(m as u16, p.redstone);
        }
        Self {
            palette,
            solid,
            collision,
            redstone,
        }
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
    fn redstone_property_maps_to_redstone_mask_only() {
        let mut sc = SubChunk::new();
        let wire = BlockStateId(55);
        let props = BlockProperties {
            solid: false,
            collision: false,
            redstone: true,
        };
        sc.set(0, 0, 0, wire, props);
        let m = SubChunk::index(0, 0, 0);
        assert!(sc.redstone_mask().get(m));
        assert!(!sc.solid_mask().get(m));
    }
}
