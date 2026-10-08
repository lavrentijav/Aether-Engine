//! # aether-worldgen
//!
//! Chunk generation producing engine [`SubChunk`]s. Two generators ship:
//!
//! * [`FlatGenerator`] — configurable superflat layers (bedrock/dirt/grass).
//! * [`NoiseGenerator`] — a Beta/1.8-era-shaped 3D density terrain (see
//!   [`beta`]): stone/dirt/grass strata following the density surface
//!   (including overhangs and caves), bedrock floor and sea-level water fill.
//!
//! A generator returns a [`GeneratedColumn`]: the non-empty vertical sub-chunks
//! for one `(cx, cz)` chunk column, ready to hand to `WorldStorage` or the
//! engine's chunk cache.

pub mod beta;
pub mod noise;
pub mod vanilla;

use aether_world::registry::{ids, BlockRegistry};
use aether_world::{BlockProperties, BlockStateId, SubChunk};
use std::collections::BTreeMap;

pub use beta::BetaTerrain;
pub use noise::ValueNoise;

/// The generated sub-chunks for one chunk column, keyed by sub-chunk `Y`.
#[derive(Debug, Default)]
pub struct GeneratedColumn {
    /// `(cy, sub-chunk)` pairs, ascending in `cy`. Empty (all-air) sections are
    /// omitted.
    pub sections: Vec<(i8, SubChunk)>,
}

/// A source of freshly generated chunk columns.
pub trait ChunkGenerator: Send + Sync {
    /// Generate the column at chunk coordinates `(cx, cz)`.
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn;
}

/// Accumulates blocks for a column across sub-chunk boundaries, then emits the
/// non-empty sections. Handles the world-Y → `(cy, local y)` split.
struct ColumnBuilder<'a> {
    registry: &'a BlockRegistry,
    sections: BTreeMap<i8, SubChunk>,
}

impl<'a> ColumnBuilder<'a> {
    fn new(registry: &'a BlockRegistry) -> Self {
        Self {
            registry,
            sections: BTreeMap::new(),
        }
    }

    /// Set a block at local column coordinates `(lx, world_y, lz)` (`lx,lz` in
    /// `0..16`). Air is a no-op (sections default to air).
    fn set(&mut self, lx: usize, world_y: i32, lz: usize, id: BlockStateId) {
        if id == BlockStateId::AIR {
            return;
        }
        let cy_i32 = world_y.div_euclid(16);
        // Guard the i8 sub-chunk index so an out-of-range layer can't wrap into
        // an unrelated `cy` (mirrors aether-api's `key_of`).
        if cy_i32 < i8::MIN as i32 || cy_i32 > i8::MAX as i32 {
            return;
        }
        let cy = cy_i32 as i8;
        let ly = world_y.rem_euclid(16) as usize;
        let props = self.registry.props_of(id);
        self.sections
            .entry(cy)
            .or_default()
            .set(lx, ly, lz, id, props);
    }

    fn finish(self) -> GeneratedColumn {
        GeneratedColumn {
            sections: self
                .sections
                .into_iter()
                .filter(|(_, sc)| !sc.is_empty())
                .collect(),
        }
    }
}

/// One layer of a [`FlatGenerator`]: a block id spanning an inclusive Y range.
#[derive(Debug, Clone, Copy)]
pub struct FlatLayer {
    /// Block id to fill.
    pub block: BlockStateId,
    /// Inclusive minimum world Y.
    pub y_min: i32,
    /// Inclusive maximum world Y.
    pub y_max: i32,
}

/// A superflat generator: the same stack of [`FlatLayer`]s in every column.
pub struct FlatGenerator {
    registry: BlockRegistry,
    layers: Vec<FlatLayer>,
}

impl FlatGenerator {
    /// A classic flat world: bedrock at y=0, dirt 1..=2, grass at y=3.
    pub fn classic() -> Self {
        Self {
            registry: BlockRegistry::new(),
            layers: vec![
                FlatLayer {
                    block: ids::BEDROCK,
                    y_min: 0,
                    y_max: 0,
                },
                FlatLayer {
                    block: ids::DIRT,
                    y_min: 1,
                    y_max: 2,
                },
                FlatLayer {
                    block: ids::GRASS_BLOCK,
                    y_min: 3,
                    y_max: 3,
                },
            ],
        }
    }

    /// A flat generator from explicit layers.
    pub fn with_layers(layers: Vec<FlatLayer>) -> Self {
        Self {
            registry: BlockRegistry::new(),
            layers,
        }
    }
}

impl ChunkGenerator for FlatGenerator {
    fn generate_column(&self, _cx: i32, _cz: i32) -> GeneratedColumn {
        let mut b = ColumnBuilder::new(&self.registry);
        for layer in &self.layers {
            for y in layer.y_min..=layer.y_max {
                for lz in 0..16 {
                    for lx in 0..16 {
                        b.set(lx, y, lz, layer.block);
                    }
                }
            }
        }
        b.finish()
    }
}

/// Terrain generator driven by a [`ValueNoise`] heightmap.
/// Terrain generator shaped like the classic Minecraft Beta/1.8 world: a real
/// 3D density field (see [`beta`]) rather than a 2D heightmap, so overhangs
/// and caves emerge from the same noise that shapes the hills — not bolted on
/// separately. See [`beta::terrain`] for exactly what is and isn't faithful
/// to the original algorithm.
pub struct NoiseGenerator {
    registry: BlockRegistry,
    terrain: BetaTerrain,
    /// Water fills air up to and including this Y.
    sea_level: i32,
}

/// Horizontal step of the coarse density grid, in blocks — matches vanilla's
/// `4` (a chunk's 16 blocks become 5 grid columns: `0, 4, 8, 12, 16`, the last
/// shared with the neighboring chunk so interpolation is seamless).
const GRID_XZ_STEP: i32 = 4;
/// Vertical step of the coarse density grid, in blocks — vanilla's `8`.
const GRID_Y_STEP: i32 = 8;
const GRID_XZ_POINTS: usize = 16 / GRID_XZ_STEP as usize + 1; // 5
const GRID_Y_POINTS: usize = beta::WORLD_HEIGHT as usize / GRID_Y_STEP as usize + 1; // 17

impl NoiseGenerator {
    /// A generator with sensible overworld-ish defaults for `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            registry: BlockRegistry::new(),
            terrain: BetaTerrain::new(seed),
            sea_level: 63,
        }
    }

    /// Water fills air up to and including this Y.
    pub fn sea_level(&self) -> i32 {
        self.sea_level
    }

    /// Blocks generated per column, from bedrock upwards.
    pub fn world_height(&self) -> i32 {
        beta::WORLD_HEIGHT
    }

    /// Sample density on the coarse grid for one chunk column, trilinearly
    /// interpolated to full block resolution. Evaluating the noise stacks at
    /// grid resolution (5×17×5 = 425 points) rather than every block
    /// (16×128×16 ≈ 33k) is what actually makes this affordable per chunk.
    fn density_grid(&self, cx: i32, cz: i32) -> [[[f64; GRID_XZ_POINTS]; GRID_Y_POINTS]; GRID_XZ_POINTS] {
        let mut grid = [[[0.0; GRID_XZ_POINTS]; GRID_Y_POINTS]; GRID_XZ_POINTS];
        for gx in 0..GRID_XZ_POINTS {
            let wx = cx * 16 + gx as i32 * GRID_XZ_STEP;
            for gz in 0..GRID_XZ_POINTS {
                let wz = cz * 16 + gz as i32 * GRID_XZ_STEP;
                let control = self.terrain.column_control(wx, wz);
                for gy in 0..GRID_Y_POINTS {
                    let wy = gy as i32 * GRID_Y_STEP;
                    grid[gx][gy][gz] = self.terrain.density(wx, wy, wz, control);
                }
            }
        }
        grid
    }

    /// Trilinear interpolation of `grid` at local column `(lx, lz)` and world
    /// `wy`, all in `0..16` / `0..WORLD_HEIGHT`.
    fn interpolate(
        grid: &[[[f64; GRID_XZ_POINTS]; GRID_Y_POINTS]; GRID_XZ_POINTS],
        lx: i32,
        wy: i32,
        lz: i32,
    ) -> f64 {
        let gx0 = (lx / GRID_XZ_STEP) as usize;
        let gz0 = (lz / GRID_XZ_STEP) as usize;
        let gy0 = (wy / GRID_Y_STEP) as usize;
        let fx = (lx % GRID_XZ_STEP) as f64 / GRID_XZ_STEP as f64;
        let fz = (lz % GRID_XZ_STEP) as f64 / GRID_XZ_STEP as f64;
        let fy = (wy % GRID_Y_STEP) as f64 / GRID_Y_STEP as f64;

        let at = |dx: usize, dy: usize, dz: usize| grid[gx0 + dx][gy0 + dy][gz0 + dz];
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;

        let x00 = lerp(at(0, 0, 0), at(1, 0, 0), fx);
        let x10 = lerp(at(0, 1, 0), at(1, 1, 0), fx);
        let x01 = lerp(at(0, 0, 1), at(1, 0, 1), fx);
        let x11 = lerp(at(0, 1, 1), at(1, 1, 1), fx);
        let y0 = lerp(x00, x10, fy);
        let y1 = lerp(x01, x11, fy);
        lerp(y0, y1, fz)
    }
}

impl ChunkGenerator for NoiseGenerator {
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        let mut b = ColumnBuilder::new(&self.registry);
        let grid = self.density_grid(cx, cz);

        for lz in 0..16i32 {
            for lx in 0..16i32 {
                // How many consecutive solid blocks we've descended through
                // since the last exposed surface (air or water above);
                // resets on every air gap, so overhangs and cave ceilings
                // each get their own grass/dirt skin instead of just the
                // column's topmost run.
                let mut depth_below_surface: i32 = -1;

                for wy in (0..beta::WORLD_HEIGHT).rev() {
                    let solid = Self::interpolate(&grid, lx, wy, lz) > 0.0;
                    if !solid {
                        depth_below_surface = -1;
                        if wy <= self.sea_level {
                            b.set(lx as usize, wy, lz as usize, ids::WATER);
                        }
                        continue;
                    }
                    depth_below_surface += 1;
                    let block = match depth_below_surface {
                        0 if wy < self.sea_level => ids::SAND,
                        0 => ids::GRASS_BLOCK,
                        1..=3 => ids::DIRT,
                        _ => ids::STONE,
                    };
                    b.set(lx as usize, wy, lz as usize, block);
                }
                // Flat bedrock floor — vanilla's is itself noise-perturbed,
                // but that's cosmetic at y=0 and not worth another octave
                // stack.
                b.set(lx as usize, 0, lz as usize, ids::BEDROCK);
            }
        }
        b.finish()
    }
}

/// Convenience: the [`BlockProperties`] the well-known ids use, for callers that
/// want to mirror generation into a different structure.
pub fn well_known_props(id: BlockStateId) -> BlockProperties {
    BlockRegistry::new().props_of(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::subchunk::SubChunk as Sc;

    fn top_block_of_column(
        col: &GeneratedColumn,
        lx: usize,
        lz: usize,
    ) -> Option<(i32, BlockStateId)> {
        let mut best: Option<(i32, BlockStateId)> = None;
        for (cy, sc) in &col.sections {
            for ly in 0..16 {
                let id = sc.get(lx, ly, lz);
                if id != BlockStateId::AIR {
                    let world_y = *cy as i32 * 16 + ly as i32;
                    if best.map(|(by, _)| world_y > by).unwrap_or(true) {
                        best = Some((world_y, id));
                    }
                }
            }
        }
        best
    }

    #[test]
    fn flat_generator_has_grass_on_top() {
        let g = FlatGenerator::classic();
        let col = g.generate_column(0, 0);
        assert!(!col.sections.is_empty());
        let (y, id) = top_block_of_column(&col, 0, 0).unwrap();
        assert_eq!(y, 3);
        assert_eq!(id, ids::GRASS_BLOCK);
    }

    #[test]
    fn noise_generator_is_deterministic() {
        let a = NoiseGenerator::new(42).generate_column(1, -1);
        let b = NoiseGenerator::new(42).generate_column(1, -1);
        assert_eq!(a.sections.len(), b.sections.len());
        for ((ya, sca), (yb, scb)) in a.sections.iter().zip(&b.sections) {
            assert_eq!(ya, yb);
            // Compare a sampling of blocks.
            for (lx, lz) in [(0, 0), (7, 8), (15, 15)] {
                assert_eq!(sca.get(lx, 5, lz), scb.get(lx, 5, lz));
            }
        }
    }

    #[test]
    fn noise_surface_is_grass_or_sand_and_bedrock_floor() {
        let g = NoiseGenerator::new(7);
        let col = g.generate_column(0, 0);
        // Bedrock at the very bottom.
        let bottom = col.sections.iter().find(|(cy, _)| *cy == 0).unwrap();
        assert_eq!(bottom.1.get(0, 0, 0), ids::BEDROCK);
        // Surface block is grass (land) or sand (shallow).
        let (_, top) = top_block_of_column(&col, 0, 0).unwrap();
        assert!(
            top == ids::GRASS_BLOCK || top == ids::SAND || top == ids::WATER,
            "unexpected surface block {top:?}"
        );
    }

    #[test]
    fn terrain_height_varies_across_columns() {
        // Sample several chunk columns and confirm the surface height isn't
        // flat everywhere — a real sign the noise is actually shaping terrain,
        // not just producing a uniform slab.
        let g = NoiseGenerator::new(99);
        let mut heights = Vec::new();
        for cx in -5..5 {
            let col = g.generate_column(cx, 0);
            if let Some((y, _)) = top_block_of_column(&col, 0, 0) {
                heights.push(y);
            }
        }
        let min = *heights.iter().min().unwrap();
        let max = *heights.iter().max().unwrap();
        assert!(max > min, "terrain is perfectly flat across columns: {heights:?}");
        for &h in &heights {
            assert!(
                (0..beta::WORLD_HEIGHT).contains(&h),
                "height {h} out of the world band"
            );
        }
    }

    #[test]
    fn density_field_is_not_a_simple_heightmap() {
        // Real 3D density (unlike a 2D heightmap) can flip solid/air more
        // than once along a vertical line — the signature of a cave or an
        // overhang. A plain heightmap can never do this.
        let t = BetaTerrain::new(2024);
        let mut found = false;
        'search: for wx in (-128..128).step_by(4) {
            for wz in (-128..128).step_by(4) {
                let control = t.column_control(wx, wz);
                let mut prev_solid = true; // the bedrock floor is always solid
                let mut transitions = 0;
                for wy in (1..beta::WORLD_HEIGHT).step_by(2) {
                    let solid = t.density(wx, wy, wz, control) > 0.0;
                    if solid && !prev_solid {
                        transitions += 1;
                    }
                    prev_solid = solid;
                }
                if transitions >= 2 {
                    found = true;
                    break 'search;
                }
            }
        }
        assert!(
            found,
            "density field never re-entered solid after air in the sampled area \
             — no overhangs/caves found"
        );
    }

    #[test]
    fn column_builder_splits_sections() {
        // A tall pillar spanning two sub-chunks must land in two sections.
        let reg = BlockRegistry::new();
        let mut b = ColumnBuilder::new(&reg);
        b.set(0, 14, 0, ids::STONE);
        b.set(0, 18, 0, ids::STONE); // crosses into cy=1
        let col = b.finish();
        assert_eq!(col.sections.len(), 2);
        let _ = Sc::new(); // keep the import used
    }
}
