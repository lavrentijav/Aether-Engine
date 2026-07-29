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

    /// Force the block at local column coordinates `(lx, world_y, lz)` to air,
    /// overwriting whatever terrain was there. Used to hollow out structures
    /// (plain [`ColumnBuilder::set`] ignores air, so it cannot carve).
    fn clear(&mut self, lx: usize, world_y: i32, lz: usize) {
        let cy_i32 = world_y.div_euclid(16);
        if cy_i32 < i8::MIN as i32 || cy_i32 > i8::MAX as i32 {
            return;
        }
        let cy = cy_i32 as i8;
        let ly = world_y.rem_euclid(16) as usize;
        if let Some(sc) = self.sections.get_mut(&cy) {
            sc.set(lx, ly, lz, BlockStateId::AIR, BlockProperties::AIR);
        }
        // A section that doesn't exist yet is already all air — nothing to do.
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
    /// Independent 3D field used to carve caves.
    cave_noise: ValueNoise,
    /// Average surface height.
    base_height: i32,
    /// Peak-to-trough amplitude added around `base_height`.
    amplitude: f64,
    /// Horizontal scale of the noise (smaller = smoother).
    scale: f64,
    /// Water fills up to this Y where terrain is lower.
    sea_level: i32,
    octaves: u32,
    /// Whether to carve caves into the generated terrain.
    caves: bool,
    /// Whether to stamp clean-room procedural structures.
    structures: bool,
}

/// A clean-room procedural structure kind (no Vanilla code or assets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Structure {
    /// An underground cobblestone room with a spawner and chests.
    Dungeon,
    /// A small surface hut of planks and logs with a doorway.
    Hut,
}

impl NoiseGenerator {
    /// A generator with sensible overworld-ish defaults for `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            registry: BlockRegistry::new(),
            noise: ValueNoise::new(seed),
            // Decorrelate caves from the surface with a mixed seed.
            cave_noise: ValueNoise::new(seed ^ 0xcafe_d00d_5eed_1357),
            base_height: 64,
            amplitude: 24.0,
            scale: 1.0 / 96.0,
            sea_level: 62,
            octaves: 4,
            caves: true,
            structures: true,
        }
    }

    /// Enable or disable cave carving (on by default).
    pub fn with_caves(mut self, caves: bool) -> Self {
        self.caves = caves;
        self
    }

    /// Enable or disable procedural structures (on by default).
    pub fn with_structures(mut self, structures: bool) -> Self {
        self.structures = structures;
        self
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

    /// Whether block `(wx, wy, wz)` sits inside a cave (should be carved to air).
    ///
    /// A thin winding band of a stretched 3D noise field produces spaghetti-like
    /// tunnels; the very bottom of the world is never carved so the bedrock
    /// floor stays intact.
    pub fn is_cave(&self, wx: i32, wy: i32, wz: i32) -> bool {
        if !self.caves || wy <= 1 {
            return false;
        }
        // Stretch the vertical axis so tunnels trend horizontal.
        let s = 1.0 / 26.0;
        let n = self
            .cave_noise
            .fbm3(wx as f64 * s, wy as f64 * s * 2.2, wz as f64 * s, 3);
        (n - 0.5).abs() < 0.055
    }

    /// Deterministically decide whether chunk `(cx, cz)` hosts a structure, and
    /// where within its footprint. Returns `(kind, anchor_lx, anchor_lz)`.
    ///
    /// Roughly 1 chunk in 24 gets a structure; the anchor is kept clear of the
    /// chunk border so the whole structure fits inside this column's `16×16`.
    fn structure_at(&self, cx: i32, cz: i32) -> Option<(Structure, usize, usize)> {
        if !self.structures {
            return None;
        }
        // SplitMix64 over (cx, cz, seed).
        let mut h = self
            .noise
            .seed()
            .wrapping_add((cx as u64).wrapping_mul(0xff51_afd7_ed55_8ccd))
            .wrapping_add((cz as u64).wrapping_mul(0xc4ce_b9fe_1a85_ec53))
            ^ 0x5372_7563_7455_7265;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        if h % 24 != 0 {
            return None;
        }
        // Anchor in 2..=8 so a 7-wide footprint stays within 0..16.
        let ax = 2 + ((h >> 8) % 7) as usize;
        let az = 2 + ((h >> 16) % 7) as usize;
        let kind = if (h >> 24) & 1 == 0 {
            Structure::Dungeon
        } else {
            Structure::Hut
        };
        Some((kind, ax, az))
    }

    /// Stamp this chunk's structure (if any) into the column builder.
    fn stamp_structure(&self, cx: i32, cz: i32, b: &mut ColumnBuilder) {
        let Some((kind, ax, az)) = self.structure_at(cx, cz) else {
            return;
        };
        // Surface height at the structure's anchor block.
        let surface = self.height_at(cx * 16 + ax as i32, cz * 16 + az as i32);
        match kind {
            Structure::Dungeon => self.stamp_dungeon(ax, az, surface, b),
            Structure::Hut => self.stamp_hut(ax, az, surface, b),
        }
    }

    /// A 7×5×7 hollow cobblestone room buried below the surface, with a spawner
    /// and two chests on the floor.
    fn stamp_dungeon(&self, ax: usize, az: usize, surface: i32, b: &mut ColumnBuilder) {
        let ceiling = surface - 6;
        let floor = ceiling - 4;
        if floor < 2 {
            return; // not enough room above bedrock
        }
        for dx in 0..7usize {
            for dz in 0..7usize {
                for dy in 0..5usize {
                    let (lx, lz, wy) = (ax + dx, az + dz, floor + dy as i32);
                    let shell = dx == 0 || dx == 6 || dz == 0 || dz == 6 || dy == 0 || dy == 4;
                    if shell {
                        // A patchy mix of cobblestone and mossy cobblestone.
                        let mossy = (lx.wrapping_mul(31) ^ lz.wrapping_mul(17) ^ dy) & 3 == 0;
                        let id = if mossy {
                            ids::MOSSY_COBBLESTONE
                        } else {
                            ids::COBBLESTONE
                        };
                        b.set(lx, wy, lz, id);
                    } else {
                        b.clear(lx, wy, lz);
                    }
                }
            }
        }
        // Spawner in the centre, chests beside it.
        b.set(ax + 3, floor + 1, az + 3, ids::SPAWNER);
        b.set(ax + 1, floor + 1, az + 1, ids::CHEST);
        b.set(ax + 5, floor + 1, az + 5, ids::CHEST);
    }

    /// A small 5×5 plank hut with log corners, a plank roof and a doorway.
    fn stamp_hut(&self, ax: usize, az: usize, surface: i32, b: &mut ColumnBuilder) {
        let base = surface + 1;
        // Floor.
        for dx in 0..5usize {
            for dz in 0..5usize {
                b.set(ax + dx, base, az + dz, ids::OAK_PLANKS);
            }
        }
        // Walls, height 3, with log corners and a doorway on the -Z face.
        for dy in 1..4i32 {
            for dx in 0..5usize {
                for dz in 0..5usize {
                    let perimeter = dx == 0 || dx == 4 || dz == 0 || dz == 4;
                    if !perimeter {
                        continue;
                    }
                    // Doorway: a 1-wide, 2-high gap in the middle of the -Z wall.
                    let doorway = dz == 0 && dx == 2 && dy <= 2;
                    if doorway {
                        continue;
                    }
                    let corner = (dx == 0 || dx == 4) && (dz == 0 || dz == 4);
                    let id = if corner {
                        ids::OAK_LOG
                    } else {
                        ids::OAK_PLANKS
                    };
                    b.set(ax + dx, base + dy, az + dz, id);
                }
            }
        }
        // Flat plank roof.
        for dx in 0..5usize {
            for dz in 0..5usize {
                b.set(ax + dx, base + 4, az + dz, ids::OAK_PLANKS);
            }
        }
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
                    // Carve caves out of the interior (never the bedrock floor
                    // or the surface block itself).
                    if y > 0 && y < surface && self.is_cave(wx, y, wz) {
                        continue;
                    }
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
        // Stamp any procedural structure last so it overrides terrain/water.
        self.stamp_structure(cx, cz, &mut b);
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

    /// Count underground air pockets (carved caves) below the surface across a
    /// chunk column, and confirm the bedrock floor survives.
    fn underground_air_and_bedrock(col: &GeneratedColumn) -> (usize, bool) {
        let mut air = 0;
        let mut bedrock_ok = true;
        for (lx, lz) in [(0usize, 0usize), (4, 11), (8, 8), (15, 3)] {
            // Bedrock present at y=0.
            let mut has_bottom = false;
            let mut top = None;
            for (cy, sc) in &col.sections {
                for ly in 0..16 {
                    let wy = *cy as i32 * 16 + ly as i32;
                    let id = sc.get(lx, ly, lz);
                    if wy == 0 && id == ids::BEDROCK {
                        has_bottom = true;
                    }
                    if id != BlockStateId::AIR {
                        top = Some(top.map_or(wy, |t: i32| t.max(wy)));
                    }
                }
            }
            if !has_bottom {
                bedrock_ok = false;
            }
            // Count air strictly between bedrock and the surface.
            if let Some(surface) = top {
                for wy in 1..surface {
                    let cy = wy.div_euclid(16) as i8;
                    let ly = wy.rem_euclid(16) as usize;
                    if let Some((_, sc)) = col.sections.iter().find(|(c, _)| *c == cy) {
                        if sc.get(lx, ly, lz) == BlockStateId::AIR {
                            air += 1;
                        }
                    } else {
                        air += 1; // an empty section below the surface is carved air
                    }
                }
            }
        }
        (air, bedrock_ok)
    }

    #[test]
    fn caves_carve_interior_but_keep_bedrock() {
        let g = NoiseGenerator::new(2024);
        // Scan a few columns; caves are sparse per-column, so aggregate.
        let mut total_air = 0;
        for (cx, cz) in [(0, 0), (1, 0), (0, 1), (2, -1), (-1, 2)] {
            let col = g.generate_column(cx, cz);
            let (air, bedrock_ok) = underground_air_and_bedrock(&col);
            assert!(bedrock_ok, "bedrock floor carved away at ({cx},{cz})");
            total_air += air;
        }
        assert!(total_air > 0, "no caves were carved anywhere");
    }

    #[test]
    fn caves_can_be_disabled() {
        let g = NoiseGenerator::new(2024).with_caves(false);
        for (cx, cz) in [(0, 0), (1, 0), (2, -1)] {
            let col = g.generate_column(cx, cz);
            let (air, bedrock_ok) = underground_air_and_bedrock(&col);
            assert!(bedrock_ok);
            assert_eq!(air, 0, "no interior air expected with caves off");
        }
    }

    /// Distinct block ids present anywhere in a generated column.
    fn ids_in_column(col: &GeneratedColumn) -> Vec<BlockStateId> {
        let mut out = Vec::new();
        for (_, sc) in &col.sections {
            for &id in sc.palette().entries() {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    #[test]
    fn structures_are_stamped() {
        let g = NoiseGenerator::new(2024);
        let mut found_dungeon = false;
        let mut found_hut = false;
        'outer: for cx in 0..60 {
            for cz in -2..3 {
                let Some((kind, _, _)) = g.structure_at(cx, cz) else {
                    continue;
                };
                let present = ids_in_column(&g.generate_column(cx, cz));
                match kind {
                    Structure::Dungeon => {
                        assert!(
                            present.contains(&ids::COBBLESTONE)
                                || present.contains(&ids::MOSSY_COBBLESTONE),
                            "dungeon at ({cx},{cz}) has no cobblestone"
                        );
                        assert!(present.contains(&ids::SPAWNER), "dungeon has no spawner");
                        assert!(present.contains(&ids::CHEST), "dungeon has no chest");
                        found_dungeon = true;
                    }
                    Structure::Hut => {
                        assert!(present.contains(&ids::OAK_PLANKS), "hut has no planks");
                        assert!(present.contains(&ids::OAK_LOG), "hut has no log corners");
                        found_hut = true;
                    }
                }
                if found_dungeon && found_hut {
                    break 'outer;
                }
            }
        }
        assert!(found_dungeon, "no dungeon stamped in the scanned range");
        assert!(found_hut, "no hut stamped in the scanned range");
    }

    #[test]
    fn structures_can_be_disabled() {
        let g = NoiseGenerator::new(2024).with_structures(false);
        for cx in 0..60 {
            for cz in -2..3 {
                let present = ids_in_column(&g.generate_column(cx, cz));
                assert!(!present.contains(&ids::SPAWNER));
                assert!(!present.contains(&ids::COBBLESTONE));
                assert!(!present.contains(&ids::OAK_PLANKS));
            }
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
