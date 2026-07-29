//! Spec §16 QA fixture 3 — World Edit / Explosion Test: rewrite ~500k blocks in
//! one burst and re-light the affected column, checking the engine keeps up.

use aether_api::{block_ids, BlockProperties, World};
use aether_api::{FlatGenerator, MemStore};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Carve an `n×n×n` cube to air, then refill it with stone — 2 writes/block.
fn blast(world: &World<MemStore, FlatGenerator>, n: i32) {
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                world.set_block_id(x, 64 + y, z, block_ids::AIR, BlockProperties::AIR);
            }
        }
    }
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                world.set_block_id(x, 64 + y, z, block_ids::STONE, BlockProperties::SOLID);
            }
        }
    }
}

fn bench_worldedit(c: &mut Criterion) {
    // 63^3 ≈ 250k cells × 2 writes ≈ 500k block edits. The world (and its
    // columns) are built once; the benchmark measures only the burst of edits,
    // which is what has to fit inside a tick.
    let n = 63;
    let world = World::new(MemStore::new(), FlatGenerator::classic());
    blast(&world, n); // warm the columns into the cache
    c.bench_function("qa_explosion_500k_edits", |b| {
        b.iter(|| {
            blast(&world, black_box(n));
            black_box(world.resident_sections())
        })
    });
}

criterion_group!(benches, bench_worldedit);
criterion_main!(benches);
