//! Chunk-send cost of engine-mode anti-xray.
//!
//! Two pairs, same world both sides:
//!
//! - `section_copy_*`: the wrapper's own cost — copying one 16³ section with
//!   and without the [`EngineColumn`] masking loop. This isolates the part the
//!   module adds, away from the codec's palette/light/compression work.
//! - `column_encode_*`: the end-to-end cost — a whole 1.21.11 chunk column
//!   through the real codec, with and without the wrapper. This is the number
//!   the brief's "< 15 % in engine mode" target is about.
//!
//! Reproduce (shared target dir, as in `docs/tasks/COMMON.md`):
//!
//! ```text
//! export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
//! cargo-locked bench -p aether-server --bench anti_xray
//! ```
//!
//! The world is a deep, uniform deepslate column with one ore per 256 cells,
//! about the real overworld density of every ore combined. Read the *ratio*
//! between a pair, not the absolute times: the end-to-end ratio is small (the
//! codec dominates); the section-copy ratio shows the wrapper's own cost.
//!
//! Results are load-sensitive because the build/runtime machine is shared. On
//! a quiet machine the end-to-end pair measured `column_encode_plain ≈ 1.98 ms`
//! and `column_encode_engine_masked ≈ 2.19 ms`, i.e. **≈ 10 % overhead**, under
//! the 15 % target; under load the codec side moves and the ratio can read
//! anywhere from ~0 % to ~20 %. The `section_copy` pair is stable: the wrapper
//! adds roughly one L1-resident membership test per cell (≈ +1.8× on a bare
//! copy). Prefer re-running both pairs back to back.

use aether_server::protocol::{codec_for, BlockSource, ServerEvent};
use aether_server::visibility::{build_hidden_states, EngineColumn};
use aether_world::registry::blocks;
use aether_world::BlockStateId;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// One ore every `ORE_EVERY` cells of the deep column. `256` is about the
/// real overworld density of every ore combined; the nether is denser, so the
/// number below is a floor, not a ceiling.
const ORE_EVERY: u32 = 256;

/// A deep column: deepslate below y=0, stone above, an ore scattered by a
/// coordinate hash. The ore table is built once, not per `block_at` call.
struct DeepColumn {
    ores: Vec<BlockStateId>,
    stone: BlockStateId,
    deepslate: BlockStateId,
}

impl DeepColumn {
    fn new() -> Self {
        let ores = [
            "minecraft:coal_ore",
            "minecraft:iron_ore",
            "minecraft:copper_ore",
            "minecraft:gold_ore",
            "minecraft:redstone_ore",
            "minecraft:lapis_ore",
            "minecraft:diamond_ore",
            "minecraft:emerald_ore",
            "minecraft:deepslate_coal_ore",
            "minecraft:deepslate_iron_ore",
            "minecraft:deepslate_diamond_ore",
        ]
        .iter()
        .map(|n| blocks::default_state(n).unwrap())
        .collect();
        Self {
            ores,
            stone: blocks::default_state("minecraft:stone").unwrap(),
            deepslate: blocks::default_state("minecraft:deepslate").unwrap(),
        }
    }
}

impl BlockSource for DeepColumn {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let h =
            (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ z.wrapping_mul(83492791)) as u32;
        if y < 0 && h % ORE_EVERY == 0 {
            return self.ores[(h as usize / ORE_EVERY as usize) % self.ores.len()];
        }
        if y < 0 {
            self.deepslate
        } else {
            self.stone
        }
    }
}

fn bench_anti_xray(c: &mut Criterion) {
    let codec = codec_for(774).expect("1.21.11 codec");
    let world = DeepColumn::new();
    let hidden = build_hidden_states(&aether_server::config::default_hidden_blocks());
    let mut out = [BlockStateId::AIR; 4096];

    c.bench_function("section_copy_plain", |b| {
        b.iter(|| {
            world.copy_section(0, -2, 0, black_box(&mut out));
            black_box(out[0])
        })
    });

    c.bench_function("section_copy_engine_masked", |b| {
        b.iter(|| {
            let column = EngineColumn::new(&world, &hidden);
            column.copy_section(0, -2, 0, black_box(&mut out));
            black_box(column.masked_any())
        })
    });

    let column = || ServerEvent::ChunkColumn { cx: 0, cz: 0 };
    c.bench_function("column_encode_plain", |b| {
        b.iter(|| black_box(codec.encode(&column(), &world).len()))
    });

    c.bench_function("column_encode_engine_masked", |b| {
        b.iter(|| {
            let masked = EngineColumn::new(&world, &hidden);
            black_box(codec.encode(&column(), &masked).len())
        })
    });
}

criterion_group!(benches, bench_anti_xray);
criterion_main!(benches);
