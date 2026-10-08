//! A chunk in the middle of being generated — vanilla's `ProtoChunk`, cut
//! down to what the generator reads and writes: a dense block array, the
//! quart biome grid, and a heightmap kept up to date as blocks are set.
//!
//! A finished chunk the generator keeps around for its neighbours is
//! [frozen](ProtoChunk::freeze): its blocks are packed per section against a
//! palette, a tenth of the dense array's 192 KiB, and still read in place.

use std::borrow::Cow;

use aether_world::{BlockStateId, PackedArray};

use super::biome::BiomeId;
use super::blockinfo;

/// One chunk's blocks and biomes.
#[derive(Clone)]
pub struct ProtoChunk {
    /// Chunk X.
    pub cx: i32,
    /// Chunk Z.
    pub cz: i32,
    /// Lowest block Y.
    pub min_y: i32,
    /// Number of block layers.
    pub height: i32,
    /// `((y - min_y) * 16 + z) * 16 + x`, local `x`/`z`.
    blocks: Blocks,
    /// `(qy * 4 + qz) * 4 + qx`, local quarts from the floor up.
    biomes: Vec<BiomeId>,
    /// `WORLD_SURFACE_WG`: the highest non-air Y per column (`min_y - 1` when
    /// empty), `z * 16 + x`.
    world_surface: [i32; 256],
}

/// A chunk's blocks: dense while being generated, packed once frozen.
#[derive(Clone)]
enum Blocks {
    Dense(Vec<u16>),
    /// One per 16 layers, from the floor up.
    Packed(Vec<PackedSection>),
}

/// 4096 blocks as indices into a palette of the states they use.
#[derive(Clone)]
struct PackedSection {
    palette: Vec<u16>,
    indices: PackedArray,
}

impl PackedSection {
    fn pack(blocks: &[u16]) -> Self {
        let mut palette: Vec<u16> = Vec::new();
        let mut idx = vec![0u16; blocks.len()];
        for (slot, &b) in idx.iter_mut().zip(blocks) {
            *slot = match palette.iter().position(|&p| p == b) {
                Some(i) => i as u16,
                None => {
                    palette.push(b);
                    (palette.len() - 1) as u16
                }
            };
        }
        let bits = match palette.len() {
            1 => 0,
            2 => 1,
            3..=4 => 2,
            5..=16 => 4,
            17..=256 => 8,
            _ => 16,
        };
        let mut indices = PackedArray::new(bits, blocks.len());
        if bits != 0 {
            for (i, &v) in idx.iter().enumerate() {
                indices.set(i, v);
            }
        }
        Self { palette, indices }
    }

    #[inline]
    fn get(&self, i: usize) -> u16 {
        self.palette[self.indices.get(i) as usize]
    }

    /// All 4096 blocks, appended to `out` a word of indices at a time.
    fn unpack_into(&self, out: &mut Vec<u16>) {
        let bits = self.indices.bits_per_entry() as usize;
        if bits == 0 {
            out.extend(std::iter::repeat(self.palette[0]).take(SECTION));
            return;
        }
        let per_word = 64 / bits;
        let mask = (1u64 << bits) - 1;
        let mut left = SECTION;
        for &w in self.indices.words() {
            for k in 0..per_word.min(left) {
                out.push(self.palette[((w >> (k * bits)) & mask) as usize]);
            }
            left = left.saturating_sub(per_word);
        }
    }
}

/// Blocks in one section.
const SECTION: usize = 4096;

impl ProtoChunk {
    /// An empty (all-air) chunk.
    pub fn new(cx: i32, cz: i32, min_y: i32, height: i32) -> Self {
        Self {
            cx,
            cz,
            min_y,
            height,
            blocks: Blocks::Dense(vec![0; 256 * height as usize]),
            biomes: vec![0; 16 * (height as usize / 4)],
            world_surface: [min_y - 1; 256],
        }
    }

    /// Lowest X of the chunk.
    pub fn min_x(&self) -> i32 {
        self.cx * 16
    }

    /// Lowest Z of the chunk.
    pub fn min_z(&self) -> i32 {
        self.cz * 16
    }

    /// One past the highest Y.
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height
    }

    #[inline]
    fn index(&self, lx: usize, y: i32, lz: usize) -> usize {
        (((y - self.min_y) as usize * 16) + lz) * 16 + lx
    }

    /// The block at local `(lx, lz)` and world `y`; air outside the height.
    #[inline]
    pub fn get(&self, lx: usize, y: i32, lz: usize) -> BlockStateId {
        if y < self.min_y || y >= self.max_y() {
            return BlockStateId::AIR;
        }
        let i = self.index(lx, y, lz);
        BlockStateId(match &self.blocks {
            Blocks::Dense(b) => b[i],
            Blocks::Packed(s) => s[i / SECTION].get(i % SECTION),
        } as u32)
    }

    /// Pack the blocks for keeping: a frozen chunk reads as before, and a
    /// write unpacks it again first.
    pub fn freeze(&mut self) {
        if let Blocks::Dense(b) = &self.blocks {
            if b.len() % SECTION == 0 {
                self.blocks = Blocks::Packed(b.chunks(SECTION).map(PackedSection::pack).collect());
            }
        }
    }

    /// The dense array, unpacking a frozen chunk.
    fn dense(&mut self) -> &mut Vec<u16> {
        if let Blocks::Packed(_) = self.blocks {
            let raw = self.raw_blocks().into_owned();
            self.blocks = Blocks::Dense(raw);
        }
        match &mut self.blocks {
            Blocks::Dense(b) => b,
            Blocks::Packed(_) => unreachable!("unpacked just above"),
        }
    }

    /// Bytes the block storage holds.
    pub fn block_bytes(&self) -> usize {
        match &self.blocks {
            Blocks::Dense(b) => b.capacity() * 2,
            Blocks::Packed(s) => s
                .iter()
                .map(|p| p.palette.capacity() * 2 + p.indices.heap_bytes())
                .sum(),
        }
    }

    /// Set a block, keeping the surface heightmap current. Out-of-height
    /// writes are dropped.
    #[inline]
    pub fn set(&mut self, lx: usize, y: i32, lz: usize, s: BlockStateId) {
        if y < self.min_y || y >= self.max_y() {
            return;
        }
        let i = self.index(lx, y, lz);
        self.dense()[i] = s.0 as u16;
        let h = self.world_surface[lz * 16 + lx];
        if !blockinfo::is_air(s) {
            if y > h {
                self.world_surface[lz * 16 + lx] = y;
            }
        } else if y == h {
            // The top was removed: walk down to the next non-air block.
            let mut yy = y - 1;
            while yy >= self.min_y && blockinfo::is_air(self.get(lx, yy, lz)) {
                yy -= 1;
            }
            self.world_surface[lz * 16 + lx] = yy;
        }
    }

    /// Set without heightmap upkeep; the caller rebuilds it with
    /// [`Self::recompute_heightmap`]. For the noise fill, which writes every
    /// block once from the top down.
    #[inline]
    pub fn set_raw(&mut self, lx: usize, y: i32, lz: usize, s: BlockStateId) {
        let i = self.index(lx, y, lz);
        self.dense()[i] = s.0 as u16;
    }

    /// Rebuild the surface heightmap from the blocks.
    pub fn recompute_heightmap(&mut self) {
        for lz in 0..16 {
            for lx in 0..16 {
                let mut y = self.max_y() - 1;
                while y >= self.min_y && blockinfo::is_air(self.get(lx, y, lz)) {
                    y -= 1;
                }
                self.world_surface[lz * 16 + lx] = y;
            }
        }
    }

    /// `getHeight(WORLD_SURFACE_WG, lx, lz)`: the highest non-air Y.
    #[inline]
    pub fn world_surface(&self, lx: usize, lz: usize) -> i32 {
        self.world_surface[lz * 16 + lx]
    }

    /// The highest Y (plus one) at which `pred` holds, scanning down;
    /// `min_y` when nowhere — vanilla's `Heightmap` convention.
    pub fn height_where(&self, lx: usize, lz: usize, pred: impl Fn(BlockStateId) -> bool) -> i32 {
        let mut y = self.world_surface(lx, lz);
        while y >= self.min_y {
            if pred(self.get(lx, y, lz)) {
                return y + 1;
            }
            y -= 1;
        }
        self.min_y
    }

    /// The biome at a local quart; `qy` counts quarts from the floor and is
    /// clamped to the chunk.
    #[inline]
    pub fn biome(&self, qx: usize, qy: i32, qz: usize) -> BiomeId {
        let qy = qy.clamp(0, self.height / 4 - 1) as usize;
        self.biomes[(qy * 4 + qz) * 4 + qx]
    }

    /// Set the biome at a local quart.
    #[inline]
    pub fn set_biome(&mut self, qx: usize, qy: usize, qz: usize, b: BiomeId) {
        self.biomes[(qy * 4 + qz) * 4 + qx] = b;
    }

    /// The raw quart grid, `(qy * 4 + qz) * 4 + qx`.
    pub fn biomes(&self) -> &[BiomeId] {
        &self.biomes
    }

    /// The raw block array.
    pub fn raw_blocks(&self) -> Cow<'_, [u16]> {
        match &self.blocks {
            Blocks::Dense(b) => Cow::Borrowed(b),
            Blocks::Packed(s) => {
                let mut out = Vec::with_capacity(s.len() * SECTION);
                for p in s {
                    p.unpack_into(&mut out);
                }
                Cow::Owned(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frozen_chunk_reads_the_same_and_is_a_fraction_of_the_size() {
        let mut c = ProtoChunk::new(0, 0, -64, 384);
        for y in -64..40 {
            for z in 0..16 {
                for x in 0..16 {
                    let id = if y < 0 {
                        1
                    } else if (x + z) % 7 == 0 {
                        3
                    } else {
                        2
                    };
                    c.set_raw(x, y, z, BlockStateId(id));
                }
            }
        }
        c.recompute_heightmap();
        let before = c.raw_blocks().into_owned();
        let dense = c.block_bytes();
        c.freeze();
        assert_eq!(c.raw_blocks().as_ref(), &before[..]);
        assert_eq!(c.get(5, 10, 7), BlockStateId(2));
        assert_eq!(c.get(0, -30, 0), BlockStateId(1));
        assert!(
            c.block_bytes() * 10 < dense,
            "{} of {dense}",
            c.block_bytes()
        );
        // A write thaws it, and the rest stays.
        let mut d = c.clone();
        d.set(1, 100, 1, BlockStateId(9));
        assert_eq!(d.get(1, 100, 1), BlockStateId(9));
        assert_eq!(d.get(5, 10, 7), BlockStateId(2));
        assert_eq!(
            c.get(1, 100, 1),
            BlockStateId::AIR,
            "the frozen original is untouched"
        );
    }
}
