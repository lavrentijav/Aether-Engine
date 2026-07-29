//! Parallel map throughput: a CPU-bound workload across the pool vs sequential.

use aether_sched::Scheduler;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A deliberately CPU-heavy per-item kernel so parallelism can show.
fn kernel(seed: &u64) -> u64 {
    let mut acc = *seed;
    for _ in 0..2_000 {
        acc = acc
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        acc ^= acc >> 17;
    }
    acc
}

fn bench_scheduler(c: &mut Criterion) {
    let items: Vec<u64> = (0..4_096).collect();

    c.bench_function("sched_map_sequential", |b| {
        let pool = Scheduler::new(1);
        b.iter(|| black_box(pool.map(black_box(&items), kernel)))
    });

    c.bench_function("sched_map_parallel", |b| {
        let pool = Scheduler::with_available_parallelism();
        b.iter(|| black_box(pool.map(black_box(&items), kernel)))
    });
}

criterion_group!(benches, bench_scheduler);
criterion_main!(benches);
