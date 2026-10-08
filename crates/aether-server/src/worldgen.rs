//! Which generator the world is built from.
//!
//! Two of them, chosen at startup:
//!
//! * **Vanilla** — `aether_worldgen::vanilla::VanillaGenerator`, driven by the
//!   operator's own copy of the game's `data/minecraft/worldgen/`. Its noise
//!   stage is verified bit-exact against the game: 1,179,648 of 1,179,648
//!   blocks across twelve chunks.
//! * **Noise** — the engine's own value-noise terrain, which needs no data and
//!   always works.
//!
//! An enum rather than a boxed trait object so that [`crate::session::DemoWorld`]
//! stays a concrete type and the hot `generate_column` call is a single
//! predictable branch instead of a virtual dispatch per column.
//!
//! # What the vanilla generator does not do yet
//!
//! Surface rules are interpreted from the pack, so the world has soil,
//! deepslate and a bedrock floor. Four condition sources still evaluate to
//! false — `temperature`, `steep`, `hole` and the badlands banding rule — so
//! snow patches, the cave-badlands variant and banded terracotta are missing.
//! They gate decoration, never the ground itself; see `KNOWN_ISSUES`.

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
    /// Load the vanilla generator from `pack_root`, falling back to the noise
    /// terrain if it cannot be read.
    ///
    /// Falling back rather than refusing to start: the data is the operator's
    /// own copy of the game and lives outside the repository, so "it is not
    /// there" is an ordinary state of affairs on a fresh machine — and a world
    /// that generates is more useful than a server that will not boot.
    pub fn load(pack_root: &str, biome_report: &str, seed: u64) -> Generator {
        if pack_root.is_empty() {
            return Generator::Noise(NoiseGenerator::new(seed));
        }
        if biome_report.is_empty() {
            eprintln!(
                "warning: `worldgen_data` is set but `biome_data` is not — the vanilla \
                 generator needs the biome-parameter report to place biomes; using noise terrain"
            );
            return Generator::Noise(NoiseGenerator::new(seed));
        }
        match aether_worldgen::vanilla::generator::VanillaGenerator::load(
            pack_root,
            biome_report,
            seed,
        ) {
            Ok(g) => {
                println!("worldgen   : vanilla (from {pack_root})");
                Generator::Vanilla(Box::new(g))
            }
            Err(e) => {
                eprintln!("warning: vanilla worldgen unavailable ({e}); using noise terrain");
                Generator::Noise(NoiseGenerator::new(seed))
            }
        }
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
