//! Entity batch-integration throughput: the SoA `pos += vel * dt` hot loop
//! over a dense population.

use aether_core::math::Vec3;
use aether_entity::{EntityStore, Spawn};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn populated(n: usize) -> EntityStore {
    let mut s = EntityStore::with_capacity(n);
    for i in 0..n {
        s.spawn(Spawn {
            position: Vec3::new(i as f64, 64.0, 0.0),
            velocity: Vec3::new(0.02, -0.01, 0.03),
            size: (0.6, 1.8),
            health: 20.0,
        });
    }
    s
}

fn bench_entities(c: &mut Criterion) {
    let mut store = populated(100_000);
    c.bench_function("integrate_100k", |b| {
        b.iter(|| {
            store.integrate(black_box(1.0));
            black_box(store.positions().len())
        })
    });

    c.bench_function("spawn_despawn_10k_cycle", |b| {
        b.iter(|| {
            let mut s = EntityStore::with_capacity(10_000);
            let mut ids = Vec::with_capacity(10_000);
            for i in 0..10_000 {
                ids.push(s.spawn(Spawn {
                    position: Vec3::new(i as f64, 0.0, 0.0),
                    ..Default::default()
                }));
            }
            for id in &ids {
                s.despawn(*id);
            }
            black_box(s.len())
        })
    });
}

criterion_group!(benches, bench_entities);
criterion_main!(benches);
