//! Turning the density field into a column of blocks — vanilla's noise stage.
//!
//! This is the step between a proven density graph and something a player can
//! stand on. Vanilla does three things here, in order, and they are separable:
//!
//! 1. **Cell interpolation.** `final_density` is evaluated only at the corners
//!    of a 4×8×4 cell and interpolated inside it. This is not an optimization
//!    that happens to be invisible — the terrain vanilla ships *is* the
//!    interpolated field.
//! 2. **Aquifers.** Where the density says "not solid", something has to decide
//!    between air, water and lava. Below the surface that decision is its own
//!    little noise system.
//! 3. **Ore veins.** A late pass that swaps some stone for ore.
//!
//! All three are implemented.
//!
//! It is worth saying what (2) turned out *not* to be. The obvious assumption
//! is that an aquifer only chooses which non-solid block fills a gap, and so
//! cannot move the solid/not-solid boundary. That is wrong, and measuring it
//! is how we found out: aquifers add a pressure term to the density and place
//! stone barriers between bodies of water at different levels. See
//! [`super::aquifer`].
//!
//! # Status
//!
//! Measured against the game's own `NoiseBasedChunkGenerator.getBaseColumn`,
//! which returns precisely this stage's output. See [`super`] for the numbers.

use std::path::Path;
use std::sync::Arc;

use super::aquifer::{AquiferConfig, ChunkAquifer};
use super::density::{BuildError, Builder, Ctx, DataPack, Mode, NoiseRegistry, Node};
use super::json::Json;
use super::ore_veins::OreVeins;

pub use super::aquifer::Fill;
pub use super::ore_veins::NoiseBlock;

/// The `noise` block of a `noise_settings` file, plus the sea level beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoiseSettings {
    /// Lowest block Y in the dimension.
    pub min_y: i32,
    /// Total build height.
    pub height: i32,
    /// Cell size on X and Z, in blocks.
    pub cell_width: i32,
    /// Cell size on Y, in blocks.
    pub cell_height: i32,
    /// Y at and below which vanilla's global fluid picker uses water.
    pub sea_level: i32,
}

impl NoiseSettings {
    /// Highest block Y, exclusive.
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height
    }

    fn from_json(settings: &Json) -> Result<Self, BuildError> {
        let noise = settings
            .get("noise")
            .ok_or_else(|| BuildError::new("noise settings: no `noise` block"))?;
        let num = |o: &Json, k: &str| -> Result<i32, BuildError> {
            o.get(k)
                .and_then(Json::as_f64)
                .map(|v| v as i32)
                .ok_or_else(|| BuildError::new(format!("noise settings: no `{k}`")))
        };
        // `size_horizontal` and `size_vertical` are counts of quarts, not
        // blocks — vanilla multiplies both by 4 to get the cell size.
        Ok(Self {
            min_y: num(noise, "min_y")?,
            height: num(noise, "height")?,
            cell_width: 4 * num(noise, "size_horizontal")?,
            cell_height: 4 * num(noise, "size_vertical")?,
            sea_level: num(settings, "sea_level")?,
        })
    }
}

/// A seeded overworld terrain shape.
pub struct Terrain {
    settings: NoiseSettings,
    final_density: Arc<Node>,
    aquifer: AquiferConfig,
    ore_veins: OreVeins,
    _noises: Box<NoiseRegistry>,
}

impl Terrain {
    /// Build from an unpacked data pack and a world seed.
    ///
    /// `pack_root` is a directory containing `data/minecraft/worldgen/`. The
    /// graph is built in [`Mode::Chunk`], because the terrain is the
    /// interpolated field and not the raw one.
    pub fn load(pack_root: impl AsRef<Path>, seed: u64) -> Result<Self, BuildError> {
        let pack = DataPack::open(pack_root)?;
        let settings_json = pack.noise_settings("minecraft:overworld")?;
        let settings = NoiseSettings::from_json(&settings_json)?;
        let router = settings_json
            .get("noise_router")
            .ok_or_else(|| BuildError::new("overworld noise settings: no `noise_router`"))?;
        let final_json = router
            .get("final_density")
            .ok_or_else(|| BuildError::new("noise router: no `final_density`"))?;

        let noises = Box::new(NoiseRegistry::new(pack.clone(), seed));
        let (final_density, aquifer, ore_veins) = {
            let mut b = Builder::with_mode(
                &pack,
                &noises,
                Mode::Chunk {
                    cell_width: settings.cell_width,
                    cell_height: settings.cell_height,
                },
            );
            let mut entry = |name: &str| -> Result<Arc<Node>, BuildError> {
                let v = router
                    .get(name)
                    .ok_or_else(|| BuildError::new(format!("noise router: no `{name}`")))?;
                b.build(v)
            };
            let final_density = entry("final_density")?;
            let cfg = AquiferConfig {
                barrier: entry("barrier")?,
                floodedness: entry("fluid_level_floodedness")?,
                spread: entry("fluid_level_spread")?,
                lava: entry("lava")?,
                erosion: entry("erosion")?,
                depth: entry("depth")?,
                preliminary_surface_level: entry("preliminary_surface_level")?,
                // Vanilla's `aquiferRandom`.
                random: noises.forked_factory("minecraft:aquifer"),
                sea_level: settings.sea_level,
            };
            let veins = OreVeins {
                toggle: entry("vein_toggle")?,
                ridged: entry("vein_ridged")?,
                gap: entry("vein_gap")?,
                // Vanilla's `oreRandom`.
                random: noises.forked_factory("minecraft:ore"),
            };
            (final_density, cfg, veins)
        };
        let _ = final_json;
        Ok(Self {
            settings,
            final_density,
            aquifer,
            ore_veins,
            _noises: noises,
        })
    }

    /// The terrain of one chunk.
    ///
    /// Per chunk rather than per column because the aquifer is: one of its
    /// cut-offs is derived from the whole chunk's surface, so the same column
    /// can fill differently depending on which chunk asked.
    pub fn chunk(&self, chunk_x: i32, chunk_z: i32) -> ChunkTerrain<'_> {
        ChunkTerrain {
            terrain: self,
            aquifer: ChunkAquifer::new(&self.aquifer, chunk_x, chunk_z),
        }
    }

    /// The dimension's noise settings.
    pub fn settings(&self) -> NoiseSettings {
        self.settings
    }

    /// The named-noise registry the density graph draws from — surface rules
    /// draw their own noises (`minecraft:surface`, …) from the same one.
    pub fn noises(&self) -> &NoiseRegistry {
        &self._noises
    }

    /// `preliminarySurfaceLevel`: vanilla's cheap terrain-height estimate,
    /// used to decide how deep below the surface the rule tree still runs.
    pub fn preliminary_surface_level(&self, x: i32, z: i32) -> i32 {
        self.aquifer.preliminary_surface_level(x, z)
    }

    /// The interpolated `final_density` at a block position.
    pub fn density_at(&self, x: i32, y: i32, z: i32) -> f64 {
        self.final_density.compute(Ctx::new(x, y, z))
    }

    /// Whether the density says this block is solid.
    ///
    /// Strictly greater than zero — a density of exactly zero is *not* solid,
    /// and that boundary is reachable, because the interpolation lands on it
    /// whenever both bracketing corners are zero.
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.density_at(x, y, z) > 0.0
    }

    /// The solid/not-solid column at `(x, z)`, from `min_y` upward.
    pub fn solid_column(&self, x: i32, z: i32) -> Vec<bool> {
        let s = self.settings;
        (0..s.height)
            .map(|i| self.is_solid(x, s.min_y + i, z))
            .collect()
    }
}

/// The terrain of one chunk: density plus the aquifer that fills its gaps.
pub struct ChunkTerrain<'a> {
    terrain: &'a Terrain,
    aquifer: ChunkAquifer<'a>,
}

impl ChunkTerrain<'_> {
    /// Solid, water, lava or air — the aquifer's answer, before ore veins.
    pub fn fill_at(&self, x: i32, y: i32, z: i32) -> Fill {
        self.aquifer
            .substance(x, y, z, self.terrain.density_at(x, y, z))
    }

    /// The block the noise stage puts at a position.
    ///
    /// Ore veins run only where the aquifer left the default block, which is
    /// why they are applied here rather than folded into [`Self::fill_at`]:
    /// a vein can never appear in water.
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> NoiseBlock {
        match self.fill_at(x, y, z) {
            Fill::Air => NoiseBlock::Air,
            Fill::Water => NoiseBlock::Water,
            Fill::Lava => NoiseBlock::Lava,
            Fill::Solid => self
                .terrain
                .ore_veins
                .block_at(Ctx::new(x, y, z))
                .unwrap_or(NoiseBlock::Stone),
        }
    }

    /// The full column at `(x, z)`, from `min_y` upward.
    pub fn column(&self, x: i32, z: i32) -> Vec<NoiseBlock> {
        let s = self.terrain.settings;
        (0..s.height)
            .map(|i| self.block_at(x, s.min_y + i, z))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_sizes_come_from_the_settings_not_from_here() {
        let j = Json::parse(
            r#"{"sea_level":63,"noise":{"min_y":-64,"height":384,
                "size_horizontal":1,"size_vertical":2}}"#,
        )
        .unwrap();
        let s = NoiseSettings::from_json(&j).unwrap();
        assert_eq!(s.cell_width, 4);
        assert_eq!(s.cell_height, 8);
        assert_eq!(s.min_y, -64);
        assert_eq!(s.max_y(), 320);
        assert_eq!(s.sea_level, 63);

        // A different dimension really does use different cells; nothing here
        // may assume 4×8.
        let j = Json::parse(
            r#"{"sea_level":32,"noise":{"min_y":0,"height":128,
                "size_horizontal":2,"size_vertical":1}}"#,
        )
        .unwrap();
        let s = NoiseSettings::from_json(&j).unwrap();
        assert_eq!((s.cell_width, s.cell_height), (8, 4));
    }
}
