//! Cost of an `/rtp`-sized safe-spot search.
//!
//! A random teleport samples a handful of columns far from the player and, for
//! each, looks for two clear cells over solid ground. This measures that on a
//! superflat world: the surface path (a cheap hint wins immediately) and the
//! scan path (no hint, so the search walks down from the spawn height). The
//! budgets match what a `/rtp` plugin would call with, so a regression in the
//! column or height scan shows up as a per-teleport cost an operator can feel.

use aether_api::{FlatGenerator, MemStore, World};
use aether_server::game::tables::Rng;
use aether_server::teleport::{find_safe_spot, random_safe_spot, BlockPos, SafeSpotConfig};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A classic superflat world: bedrock at 0, dirt, grass at 3, air above.
fn flat_world() -> World<MemStore, FlatGenerator> {
    let world = World::new(MemStore::new(), FlatGenerator::classic());
    // Materialise the spawn column once, outside the timed region.
    let _ = world.get_block(0, 4, 0);
    world
}

fn bench_rtp(c: &mut Criterion) {
    let world = flat_world();
    // `/rtp` distances: at least 100 blocks out, up to 1000.
    let (min_r, max_r) = (100u32, 1000u32);

    let mut group = c.benchmark_group("rtp");
    group.sample_size(50);

    // With the surface hint, each sampled column resolves to the ground
    // instead of being scanned from the spawn height.
    let surface_cfg = SafeSpotConfig::default();
    let mut rng = Rng::new(0x5EED);
    group.bench_function("random_safe_spot/surface_radius_1000", |b| {
        b.iter(|| {
            let spot = random_safe_spot(
                &world,
                BlockPos::new(0, 64, 0),
                min_r,
                max_r,
                surface_cfg,
                &mut rng,
            );
            black_box(spot);
        })
    });

    // Without a hint, the same columns are scanned from the spawn height.
    let scan_cfg = SafeSpotConfig {
        prefer_surface: false,
        ..SafeSpotConfig::default()
    };
    let mut rng = Rng::new(0x5EED);
    group.bench_function("random_safe_spot/scan_radius_1000", |b| {
        b.iter(|| {
            let spot = random_safe_spot(
                &world,
                BlockPos::new(0, 64, 0),
                min_r,
                max_r,
                scan_cfg,
                &mut rng,
            );
            black_box(spot);
        })
    });

    // The deterministic search a `/spawn` or `/top` uses.
    group.bench_function("find_safe_spot/radius_16", |b| {
        b.iter(|| {
            let spot = find_safe_spot(&world, BlockPos::new(0, 64, 0), 16, scan_cfg);
            black_box(spot);
        })
    });

    group.finish();
}

criterion_group!(benches, bench_rtp);
criterion_main!(benches);
