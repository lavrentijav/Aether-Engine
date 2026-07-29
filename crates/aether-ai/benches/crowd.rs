//! Spec §16 QA fixture 2 — Entity Density Test: 5,000 entities each steering
//! along a shared flow field and integrating one tick. Target: stay well inside
//! the 50 ms / 20 TPS budget.

use aether_ai::{FlowField, NavGrid};
use aether_core::math::Vec3;
use aether_entity::{EntityStore, Spawn};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_crowd(c: &mut Criterion) {
    // One shared field toward the centre of a 128×128 arena.
    let grid = NavGrid::new(128, 128);
    let field = FlowField::toward(&grid, (64, 64));

    // 5,000 entities scattered across the arena.
    let mut store = EntityStore::with_capacity(5_000);
    let mut cells = Vec::with_capacity(5_000);
    for i in 0..5_000usize {
        let x = (i * 7) % 128;
        let z = (i * 13) % 128;
        cells.push((x, z));
        store.spawn(Spawn {
            position: Vec3::new(x as f64, 64.0, z as f64),
            ..Default::default()
        });
    }

    c.bench_function("qa_entity_density_5k_with_ai", |b| {
        b.iter(|| {
            // AI decision: each entity reads its flow-field direction O(1) and
            // sets velocity toward the goal, then physics integrates the batch.
            let ids: Vec<_> = store.ids().collect();
            for (id, &(x, z)) in ids.iter().zip(&cells) {
                if let Some((sx, sz)) = field.steer(x, z) {
                    store.set_velocity(*id, Vec3::new(sx * 0.2, 0.0, sz * 0.2));
                }
            }
            store.integrate(black_box(1.0));
            black_box(store.len())
        })
    });
}

criterion_group!(benches, bench_crowd);
criterion_main!(benches);
