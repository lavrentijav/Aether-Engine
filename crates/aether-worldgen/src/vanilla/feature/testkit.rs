//! Test-only helpers: place one configured feature into a hand-built 3×3
//! region so a unit test can check it without generating a whole world.
//!
//! Everything here needs the operator's unpacked copy of the game (the pack
//! the generator reads its biome/tag/noise data from), so tests that use it
//! skip quietly when `AETHER_VANILLA_PACK` is unset.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use aether_world::BlockStateId;

use super::super::chunk::ProtoChunk;
use super::super::density::DataPack;
use super::super::generator::VanillaGenerator;
use super::super::json::Json;
use super::super::rng::WorldgenRandom;
use super::super::tags::Tags;
use super::blocks::LegacyNoises;
use super::features::{self, Configured};
use super::level::{Level, Pos};
use super::{Ctx, Loader};

/// The pack root from the environment, or `None` when absent.
pub fn pack() -> Option<String> {
    std::env::var("AETHER_VANILLA_PACK").ok()
}

/// The block the test world is solid with, resolved once.
pub fn stone() -> BlockStateId {
    aether_world::registry::blocks::default_state("minecraft:stone").expect("stone state")
}

/// A 3×3 region of hand-built chunks around `(cx, cz)`, with every block air.
pub struct World {
    region: [[Arc<ProtoChunk>; 3]; 3],
    cx: i32,
    cz: i32,
}

impl World {
    /// An all-air region around `(cx, cz)`.
    pub fn new(gen: &VanillaGenerator, cx: i32, cz: i32) -> Self {
        let core = gen.core_handle();
        let (min_y, height) = (core.min_y, core.height);
        let region = std::array::from_fn(|dz| {
            std::array::from_fn(|dx| {
                Arc::new(ProtoChunk::new(
                    cx - 1 + dx as i32,
                    cz - 1 + dz as i32,
                    min_y,
                    height,
                ))
            })
        });
        Self { region, cx, cz }
    }

    /// Set a block by world coordinate, keeping the column's heightmap.
    pub fn set(&mut self, x: i32, y: i32, z: i32, s: BlockStateId) {
        let dx = (x >> 4) - self.cx + 1;
        let dz = (z >> 4) - self.cz + 1;
        if !(0..3).contains(&dx) || !(0..3).contains(&dz) {
            return;
        }
        let c = Arc::make_mut(&mut self.region[dz as usize][dx as usize]);
        c.set((x & 15) as usize, y, (z & 15) as usize, s);
    }

    /// Fill a solid box, inclusive, by world coordinate.
    pub fn fill(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, s: BlockStateId) {
        for y in y0..=y1 {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    self.set(x, y, z, s);
                }
            }
        }
    }
}

/// Parse a configured feature from inline JSON against `gen`'s pack.
pub fn parse_configured(gen: &VanillaGenerator, pack: &str, j: &Json) -> Arc<Configured> {
    let dp = DataPack::open(pack).expect("open pack");
    let tags = Tags::new(&dp);
    let (min_y, height) = {
        let c = gen.core_handle();
        (c.min_y, c.height)
    };
    let mut loader = Loader {
        pack: &dp,
        tags: &tags,
        noises: LegacyNoises {
            _noises: gen.terrain().noises(),
        },
        min_y,
        height,
        placed: HashMap::new(),
        configured: HashMap::new(),
        unsupported: BTreeSet::new(),
    };
    loader.configured(j).expect("parse configured feature")
}

/// Place `cfg` at `at` in `world` with a fresh xoroshiro random, returning the
/// blocks written anywhere in the region.
pub fn place(
    gen: &VanillaGenerator,
    world: &World,
    cfg: &Configured,
    at: Pos,
    seed: i64,
) -> Vec<(i32, i32, i32, BlockStateId)> {
    let mut lv = Level::new(gen.core_handle(), world.cx, world.cz, &world.region);
    let mut r = WorldgenRandom::xoroshiro(seed);
    let mut ctx = Ctx {
        lv: &mut lv,
        r: &mut r,
        d: gen.decorator_handle(),
    };
    features::place_configured(&mut ctx, cfg, at);
    lv.into_writes()
}

/// The count of written blocks whose name matches `name`.
pub fn count_name(writes: &[(i32, i32, i32, BlockStateId)], name: &str) -> usize {
    writes
        .iter()
        .filter(|(_, _, _, s)| super::super::blockinfo::name(*s) == name)
        .count()
}
