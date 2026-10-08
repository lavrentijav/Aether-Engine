//! Wiring: a data pack plus a seed plus a biome table becomes a biome source.
//!
//! Everything shape-related is read from the game's files; this module only
//! decides which router entries feed which climate axis. That mapping is the
//! one piece of the biome pipeline that is neither data nor obvious, because
//! the router's names and the climate axes' names disagree:
//!
//! | router entry | climate axis    |
//! |--------------|-----------------|
//! | `temperature`| temperature     |
//! | `vegetation` | humidity        |
//! | `continents` | continentalness |
//! | `erosion`    | erosion         |
//! | `depth`      | depth           |
//! | `ridges`     | weirdness       |

use std::path::Path;

use super::climate::{ParameterList, Sampler, SearchCache, TargetPoint};
use super::density::{BuildError, Builder, DataPack, NoiseRegistry};
use super::json::Json;

/// A seeded overworld biome source.
pub struct OverworldBiomeSource {
    sampler: Sampler,
    biomes: ParameterList,
    /// Kept alive because the graph's noises borrow nothing but do live here.
    _noises: Box<NoiseRegistry>,
}

impl OverworldBiomeSource {
    /// Build from an unpacked data pack and a world seed, with the biome
    /// table rebuilt in code ([`super::biome_table`]) — no report needed.
    pub fn new(pack_root: impl AsRef<Path>, seed: u64) -> Result<Self, BuildError> {
        Self::with_table(
            pack_root,
            ParameterList::from_points(super::biome_table::overworld()),
            seed,
        )
    }

    fn with_table(
        pack_root: impl AsRef<Path>,
        biomes: ParameterList,
        seed: u64,
    ) -> Result<Self, BuildError> {
        let pack = DataPack::open(pack_root)?;
        let settings = pack.noise_settings("minecraft:overworld")?;
        let router = settings
            .get("noise_router")
            .ok_or_else(|| BuildError::new("overworld noise settings: no `noise_router`"))?;
        let noises = Box::new(NoiseRegistry::new(pack.clone(), seed));
        let sampler = {
            let mut b = Builder::new(&pack, &noises);
            let mut entry = |name: &str| -> Result<_, BuildError> {
                let v = router
                    .get(name)
                    .ok_or_else(|| BuildError::new(format!("noise router: no `{name}`")))?;
                b.build(v)
            };
            Sampler {
                temperature: entry("temperature")?,
                humidity: entry("vegetation")?,
                continentalness: entry("continents")?,
                erosion: entry("erosion")?,
                depth: entry("depth")?,
                weirdness: entry("ridges")?,
            }
        };
        Ok(Self {
            sampler,
            biomes,
            _noises: noises,
        })
    }

    /// The table row nearest the climate at a quart position — an index into
    /// [`ParameterList::entries`].
    pub fn entry_at(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> usize {
        self.biomes
            .find_entry(&self.sampler.sample(quart_x, quart_y, quart_z))
    }

    /// Build from an unpacked data pack, a `--reports` biome-parameter dump
    /// and a world seed.
    ///
    /// `pack_root` is a directory containing `data/minecraft/worldgen/`;
    /// `biome_report` is the path to
    /// `reports/biome_parameters/minecraft/overworld.json`. Neither is
    /// vendored: both come from the operator's own copy of the game.
    pub fn load(
        pack_root: impl AsRef<Path>,
        biome_report: impl AsRef<Path>,
        seed: u64,
    ) -> Result<Self, BuildError> {
        let pack = DataPack::open(pack_root)?;
        let settings = pack.noise_settings("minecraft:overworld")?;
        let router = settings
            .get("noise_router")
            .ok_or_else(|| BuildError::new("overworld noise settings: no `noise_router`"))?;

        let noises = Box::new(NoiseRegistry::new(pack.clone(), seed));
        let sampler = {
            let mut b = Builder::new(&pack, &noises);
            let mut entry = |name: &str| -> Result<_, BuildError> {
                let v = router
                    .get(name)
                    .ok_or_else(|| BuildError::new(format!("noise router: no `{name}`")))?;
                b.build(v)
            };
            Sampler {
                temperature: entry("temperature")?,
                humidity: entry("vegetation")?,
                continentalness: entry("continents")?,
                erosion: entry("erosion")?,
                depth: entry("depth")?,
                weirdness: entry("ridges")?,
            }
        };

        let report_text = std::fs::read_to_string(biome_report.as_ref()).map_err(|e| {
            BuildError::new(format!(
                "cannot read {}: {e}",
                biome_report.as_ref().display()
            ))
        })?;
        let report = Json::parse(&report_text)
            .map_err(|e| BuildError::new(format!("{}: {e}", biome_report.as_ref().display())))?;
        let biomes = ParameterList::from_report(&report)?;

        Ok(Self {
            sampler,
            biomes,
            _noises: noises,
        })
    }

    /// The climate vector at a quart (4×4×4 cell) position.
    pub fn sample(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> TargetPoint {
        self.sampler.sample(quart_x, quart_y, quart_z)
    }

    /// The biome id at a quart position.
    pub fn biome_at(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> &str {
        self.biomes
            .find(&self.sampler.sample(quart_x, quart_y, quart_z))
    }

    /// The biome id at a quart position, remembering the answer in `cache`.
    ///
    /// Reproduces vanilla's stateful search: see [`SearchCache`]. Use this only
    /// to replay a specific sequence of lookups; [`Self::biome_at`] is the
    /// order-independent one.
    pub fn biome_at_cached(
        &self,
        quart_x: i32,
        quart_y: i32,
        quart_z: i32,
        cache: &mut SearchCache,
    ) -> &str {
        self.biomes
            .find_cached(&self.sampler.sample(quart_x, quart_y, quart_z), cache)
    }

    /// The biome id at a quart position, plus how many biomes tied for
    /// nearest. See [`ParameterList::find_with_ties`].
    pub fn biome_at_with_ties(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> (&str, usize) {
        self.biomes
            .find_with_ties(&self.sampler.sample(quart_x, quart_y, quart_z))
    }

    /// The underlying climate sampler.
    pub fn sampler(&self) -> &Sampler {
        &self.sampler
    }

    /// The biome table.
    pub fn biomes(&self) -> &ParameterList {
        &self.biomes
    }
}
