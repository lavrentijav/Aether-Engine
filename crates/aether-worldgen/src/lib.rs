//! # aether-worldgen
//!
//! Chunk generation producing engine [`SubChunk`]s. Two generators ship:
//!
//! * [`FlatGenerator`] — configurable superflat layers (bedrock/dirt/grass).
//! * [`NoiseGenerator`] — a value-noise heightmap with stone/dirt/grass strata,
//!   bedrock floor and sea-level water fill.
//!
//! A generator returns a [`GeneratedColumn`]: the non-empty vertical sub-chunks
//! for one `(cx, cz)` chunk column, ready to hand to `WorldStorage` or the
//! engine's chunk cache.

pub mod noise;

use aether_world::registry::{ids, BlockRegistry};
use aether_world::{BlockProperties, BlockStateId, SubChunk};
use std::collections::BTreeMap;

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
        let cy = world_y.div_euclid(16) as i8;
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
pub struct NoiseGenerator {
    registry: BlockRegistry,
    noise: ValueNoise,
    /// Average surface height.
    base_height: i32,
    /// Peak-to-trough amplitude added around `base_height`.
    amplitude: f64,
    /// Horizontal scale of the noise (smaller = smoother).
    scale: f64,
    /// Water fills up to this Y where terrain is lower.
    sea_level: i32,
    octaves: u32,
}

impl NoiseGenerator {
    /// A generator with sensible overworld-ish defaults for `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            registry: BlockRegistry::new(),
            noise: ValueNoise::new(seed),
            base_height: 64,
            amplitude: 24.0,
            scale: 1.0 / 96.0,
            sea_level: 62,
            octaves: 4,
        }
    }

    /// The surface height (world Y of the topmost solid block) at world column
    /// `(wx, wz)`.
    pub fn height_at(&self, wx: i32, wz: i32) -> i32 {
        let n = self
            .noise
            .fbm(wx as f64 * self.scale, wz as f64 * self.scale, self.octaves);
        // Map [0,1] -> [-amp/2, +amp/2] around base_height.
        self.base_height + ((n - 0.5) * self.amplitude).round() as i32
    }
}

impl ChunkGenerator for NoiseGenerator {
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        let mut b = ColumnBuilder::new(&self.registry);
        for lz in 0..16usize {
            for lx in 0..16usize {
                let wx = cx * 16 + lx as i32;
                let wz = cz * 16 + lz as i32;
                let surface = self.height_at(wx, wz);

                for y in 0..=surface {
                    let block = if y == 0 {
                        ids::BEDROCK
                    } else if y == surface {
                        // Grass on land, sand just underwater.
                        if surface < self.sea_level {
                            ids::SAND
                        } else {
                            ids::GRASS_BLOCK
                        }
                    } else if y >= surface - 3 {
                        ids::DIRT
                    } else {
                        ids::STONE
                    };
                    b.set(lx, y, lz, block);
                }

                // Fill water from just above the surface up to sea level.
                for y in (surface + 1)..=self.sea_level {
                    b.set(lx, y, lz, ids::WATER);
                }
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
    fn height_within_expected_band() {
        let g = NoiseGenerator::new(99);
        for x in -50..50 {
            let h = g.height_at(x, x * 2);
            assert!((40..=90).contains(&h), "height {h} out of band at x={x}");
        }
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
