//! A [`ChunkGenerator`](crate::ChunkGenerator) over the verified noise stage.
//!
//! This is where the parity work stops being a measurement and starts being
//! terrain: the cell-interpolated density, the aquifer and the ore veins, fed
//! into engine sub-chunks — topped with the surface rule pass
//! ([`super::surface`]) that turns the bare stone into grass, dirt, sand,
//! gravel, sandstone, deepslate and bedrock.
//!
//! # Ids versus names
//!
//! The generator names its blocks and resolves them through the registry
//! ([`blocks::default_state`]). It cannot use numeric ids, and not only for
//! tidiness: the worldgen data is read from whatever version the operator has
//! on disk, while [`BlockStateId`] is pinned to the engine's own version. A
//! name is the only thing that means the same in both. The surface rule tree
//! resolves its own block names the same way, once, at load time — see
//! [`super::surface::SurfaceRuleSet`].

use std::sync::Arc;

use aether_world::registry::{blocks, props, BlockRegistry};
use aether_world::BlockStateId;

use super::density::BuildError;
use super::ore_veins::NoiseBlock;
use super::overworld::OverworldBiomeSource;
use super::surface::SurfaceRuleSet;
use super::terrain::Terrain;
use crate::{ChunkGenerator, ColumnBuilder, GeneratedColumn};

/// The block ids the noise stage can emit, resolved once.
///
/// Resolution happens at construction so a missing or renamed block is a
/// start-up error rather than a hole in the terrain a thousand blocks away.
#[derive(Debug, Clone)]
struct Palette {
    air: BlockStateId,
    water: BlockStateId,
    lava: BlockStateId,
    stone: BlockStateId,
    granite: BlockStateId,
    tuff: BlockStateId,
    copper_ore: BlockStateId,
    raw_copper_block: BlockStateId,
    deepslate_iron_ore: BlockStateId,
    raw_iron_block: BlockStateId,
}

impl Palette {
    fn resolve() -> Result<Self, BuildError> {
        let one = |name: &str| -> Result<BlockStateId, BuildError> {
            blocks::default_state(name)
                .ok_or_else(|| BuildError::new(format!("block registry has no `{name}`")))
        };
        // Water is the one block whose *state* matters here: a source block is
        // `level=0`, and the default state of `minecraft:water` is not
        // guaranteed to be it.
        let water_block = blocks::block_id_of("minecraft:water")
            .ok_or_else(|| BuildError::new("block registry has no `minecraft:water`"))?;
        let water = props::state_with(water_block, &[("level", "0")])
            .map(BlockStateId)
            .ok_or_else(|| BuildError::new("minecraft:water has no `level=0` state"))?;
        Ok(Self {
            air: one("minecraft:air")?,
            water,
            lava: one("minecraft:lava")?,
            stone: one("minecraft:stone")?,
            granite: one("minecraft:granite")?,
            tuff: one("minecraft:tuff")?,
            copper_ore: one("minecraft:copper_ore")?,
            raw_copper_block: one("minecraft:raw_copper_block")?,
            deepslate_iron_ore: one("minecraft:deepslate_iron_ore")?,
            raw_iron_block: one("minecraft:raw_iron_block")?,
        })
    }

    fn id_of(&self, b: NoiseBlock) -> BlockStateId {
        match b {
            NoiseBlock::Air => self.air,
            NoiseBlock::Water => self.water,
            NoiseBlock::Lava => self.lava,
            NoiseBlock::Stone => self.stone,
            NoiseBlock::Granite => self.granite,
            NoiseBlock::Tuff => self.tuff,
            NoiseBlock::CopperOre => self.copper_ore,
            NoiseBlock::RawCopperBlock => self.raw_copper_block,
            NoiseBlock::DeepslateIronOre => self.deepslate_iron_ore,
            NoiseBlock::RawIronBlock => self.raw_iron_block,
        }
    }
}

/// Generates the overworld's noise stage plus surface rules, for a given seed.
pub struct VanillaGenerator {
    terrain: Arc<Terrain>,
    biomes: OverworldBiomeSource,
    surface: SurfaceRuleSet,
    palette: Palette,
    registry: BlockRegistry,
}

impl VanillaGenerator {
    /// Build from an unpacked copy of the game's worldgen data, a
    /// `--reports` biome-parameter dump, and a seed.
    ///
    /// `pack_root` is a directory containing `data/minecraft/worldgen/` — the
    /// operator's own copy of the game, read at run time and never vendored.
    /// `biome_report` is `reports/biome_parameters/minecraft/overworld.json`
    /// from the same copy: the overworld's biome table is code in the game,
    /// not data, so it exists on disk only in that report (see
    /// [`OverworldBiomeSource::load`]).
    pub fn load(
        pack_root: impl AsRef<std::path::Path>,
        biome_report: impl AsRef<std::path::Path>,
        seed: u64,
    ) -> Result<Self, BuildError> {
        let terrain = Terrain::load(&pack_root, seed)?;
        let biomes = OverworldBiomeSource::load(&pack_root, biome_report, seed)?;

        let pack = super::density::DataPack::open(&pack_root)?;
        let settings_json = pack.noise_settings("minecraft:overworld")?;
        let surface_rule_json = settings_json
            .get("surface_rule")
            .ok_or_else(|| BuildError::new("overworld noise settings: no `surface_rule`"))?;
        let s = terrain.settings();
        let surface = SurfaceRuleSet::load(surface_rule_json, terrain.noises(), s.min_y, s.height)?;

        Ok(Self {
            terrain: Arc::new(terrain),
            biomes,
            surface,
            palette: Palette::resolve()?,
            registry: BlockRegistry::new(),
        })
    }

    /// The terrain this generator draws from.
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    /// The surface rule tree's condition/rule types that are read but not
    /// evaluated faithfully. See [`SurfaceRuleSet::unsupported`].
    pub fn unsupported_surface_rules(&self) -> &[String] {
        self.surface.unsupported()
    }
}

impl ChunkGenerator for VanillaGenerator {
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        let settings = self.terrain.settings();
        let chunk = self.terrain.chunk(cx, cz);
        let mut b = ColumnBuilder::new(&self.registry);
        for lz in 0..16usize {
            for lx in 0..16usize {
                let x = cx * 16 + lx as i32;
                let z = cz * 16 + lz as i32;
                let mut raw = chunk.column(x, z);
                let prelim = self.terrain.preliminary_surface_level(x, z);
                let painted = self.surface.paint(
                    x,
                    z,
                    &mut raw,
                    |nb| self.palette.id_of(nb),
                    &self.biomes,
                    prelim,
                );
                for (i, id) in painted.into_iter().enumerate() {
                    if id == self.palette.air {
                        continue;
                    }
                    let y = settings.min_y + i as i32;
                    b.set(lx, y, lz, id);
                }
            }
        }
        b.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_block_the_noise_stage_emits_resolves() {
        // A missing name here would silently become air in the world, so the
        // whole palette is resolved up front and checked here.
        let p = Palette::resolve().expect("palette resolves against the block registry");
        for b in [
            NoiseBlock::Air,
            NoiseBlock::Water,
            NoiseBlock::Lava,
            NoiseBlock::Stone,
            NoiseBlock::Granite,
            NoiseBlock::Tuff,
            NoiseBlock::CopperOre,
            NoiseBlock::RawCopperBlock,
            NoiseBlock::DeepslateIronOre,
            NoiseBlock::RawIronBlock,
        ] {
            let id = p.id_of(b);
            if b != NoiseBlock::Air {
                assert_ne!(id, BlockStateId::AIR, "{} resolved to air", b.name());
            }
        }
        // Water must be the source state, not merely the block's default.
        assert_eq!(
            p.water,
            BlockStateId(
                props::state_with(blocks::block_id_of("minecraft:water").unwrap(), &[("level", "0")])
                    .unwrap()
            )
        );
    }
}
