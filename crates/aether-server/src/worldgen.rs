//! Which generator the world is built from.
//!
//! Two of them, chosen at startup:
//!
//! * **Vanilla** — `aether_worldgen::vanilla::VanillaGenerator`, driven by the
//!   operator's own copy of the game's `data/minecraft/`. Terrain shape,
//!   biomes, surface and carvers are verified against the game itself; the
//!   decoration step (ores, trees, plants, lakes, springs…) is data-driven
//!   and vanilla-like but not position-exact. This is the generator used
//!   whenever `worldgen_data` is set and readable.
//! * **Noise** — the engine's own value-noise terrain, which needs no data and
//!   always works.
//!
//! An enum rather than a boxed trait object so that [`crate::session::DemoWorld`]
//! stays a concrete type and the hot `generate_column` call is a single
//! predictable branch instead of a virtual dispatch per column.
//!
//! # What the vanilla generator does not do yet
//!
//! Structures (villages, mineshafts, strongholds…), geodes, dungeons, fossils
//! and dripstone are not generated; see `KNOWN_ISSUES`. The biome table is
//! built into the crate, so `biome_data` is optional: when it is set the
//! report is used instead of the built-in table.

use aether_worldgen::{ChunkGenerator, GeneratedColumn, NoiseGenerator};

/// The world's terrain source.
#[allow(clippy::large_enum_variant)]
pub enum Generator {
    /// Vanilla's own terrain, from the operator's copy of the game data.
    Vanilla(Box<aether_worldgen::vanilla::generator::VanillaGenerator>),
    /// The engine's built-in value-noise terrain.
    Noise(NoiseGenerator),
}

impl Generator {
    /// The generator the configuration asks for: noise terrain when no data
    /// pack is set, vanilla terrain from it when one is.
    ///
    /// A data pack that is set but unreadable is an error, not a quiet switch
    /// to noise terrain: a world generated with the wrong generator is
    /// written to disk as players edit it and cannot be taken back, while a
    /// server that refuses to start says exactly what is missing.
    pub fn load(pack_root: &str, biome_report: &str, seed: u64) -> Result<Generator, String> {
        if pack_root.is_empty() {
            return Ok(Generator::Noise(NoiseGenerator::new(seed)));
        }
        let built = if biome_report.is_empty() {
            aether_worldgen::vanilla::generator::VanillaGenerator::new(pack_root, seed)
        } else {
            aether_worldgen::vanilla::generator::VanillaGenerator::load(
                pack_root,
                biome_report,
                seed,
            )
        };
        let g = built.map_err(|e| e.to_string())?;
        crate::log::info(&format!("worldgen   : vanilla (from {pack_root})"));
        Ok(Generator::Vanilla(Box::new(g)))
    }

    /// A one-line description for the console banner.
    pub fn describe(&self, seed: u64) -> String {
        match self {
            Generator::Vanilla(_) => format!("vanilla terrain (seed {seed})"),
            Generator::Noise(_) => format!("noise terrain (seed {seed})"),
        }
    }
}

impl ChunkGenerator for Generator {
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        match self {
            Generator::Vanilla(g) => g.generate_column(cx, cz),
            Generator::Noise(g) => g.generate_column(cx, cz),
        }
    }
}
