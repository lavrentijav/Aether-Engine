//! Chunk-send cost of engine-mode anti-xray.
//!
//! Engine mode wraps the column in an [`EngineColumn`] before the codec reads
//! it. For every cell it does one array-index membership test; only a cell
//! that is already in the hidden set pays for the six-neighbour scan. This bench measures that
//! wrapper over a full 16³ section against reading the same section directly,
//! which is the per-section overhead the target ("< 15 % in engine mode")
//! talks about. The hidden set is the built-in ore list and the section holds
//! a scatter of ores, the worst realistic case.

use aether_server::protocol::BlockSource;
use aether_server::visibility::{build_hidden_states, EngineColumn};
use aether_world::registry::blocks;
use aether_world::BlockStateId;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A deep overworld section: stone above y=-10, deepslate below, every block
/// with a coordinate hash that lands on an ore one time in 256.
struct DeepColumn;

fn ore_table() -> Vec<BlockStateId> {
    [
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
    .collect()
}

impl BlockSource for DeepColumn {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let h =
            (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ z.wrapping_mul(83492791)) as u32;
        if y < 0 && h % 256 == 0 {
            let table = ore_table();
            return table[(h as usize / 256) % table.len()];
        }
        if y < -10 {
            blocks::default_state("minecraft:deepslate").unwrap()
        } else {
            blocks::default_state("minecraft:stone").unwrap()
        }
    }
}

fn bench_anti_xray(c: &mut Criterion) {
    let world = DeepColumn;
    let names = aether_server::config::default_hidden_blocks();
    let hidden = build_hidden_states(&names);
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
}

criterion_group!(benches, bench_anti_xray);
criterion_main!(benches);
