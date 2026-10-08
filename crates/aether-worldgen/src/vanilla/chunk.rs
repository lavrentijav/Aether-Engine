//! A chunk in the middle of being generated — vanilla's `ProtoChunk`, cut
//! down to what the generator reads and writes: a dense block array, the
//! quart biome grid, and a heightmap kept up to date as blocks are set.

use aether_world::BlockStateId;

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
    blocks: Vec<u16>,
    /// `(qy * 4 + qz) * 4 + qx`, local quarts from the floor up.
    biomes: Vec<BiomeId>,
    /// `WORLD_SURFACE_WG`: the highest non-air Y per column (`min_y - 1` when
    /// empty), `z * 16 + x`.
    world_surface: [i32; 256],
}

impl ProtoChunk {
    /// An empty (all-air) chunk.
    pub fn new(cx: i32, cz: i32, min_y: i32, height: i32) -> Self {
        Self {
            cx,
            cz,
            min_y,
            height,
            blocks: vec![0; 256 * height as usize],
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
        BlockStateId(self.blocks[self.index(lx, y, lz)] as u32)
    }

    /// Set a block, keeping the surface heightmap current. Out-of-height
    /// writes are dropped.
    #[inline]
    pub fn set(&mut self, lx: usize, y: i32, lz: usize, s: BlockStateId) {
        if y < self.min_y || y >= self.max_y() {
            return;
        }
        let i = self.index(lx, y, lz);
        self.blocks[i] = s.0 as u16;
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
        self.blocks[i] = s.0 as u16;
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
    pub fn raw_blocks(&self) -> &[u16] {
        &self.blocks
    }
}
