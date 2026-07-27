//! Throughput of the SoA mask operations: scalar oracle vs. the runtime-dispatched
//! backend, over a full 64-word (4096-bit) sub-chunk mask.

use aether_core::simd::{dispatch, MaskOps, Scalar};
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

fn bench_masks(c: &mut Criterion) {
    let a = [0x0123_4567_89ab_cdefu64; 64];
    let b = [0xfedc_ba98_7654_3210u64; 64];
    let mut out = [0u64; 64];

    let mut group = c.benchmark_group("mask_and_64words");
    group.throughput(Throughput::Bytes(64 * 8));

    let scalar = Scalar;
    group.bench_function("scalar", |be| {
        be.iter(|| scalar.and_into(black_box(&mut out), black_box(&a), black_box(&b)))
    });

    let ops = dispatch();
    group.bench_function(ops.backend().name(), |be| {
        be.iter(|| ops.and_into(black_box(&mut out), black_box(&a), black_box(&b)))
    });
    group.finish();
}

criterion_group!(benches, bench_masks);
criterion_main!(benches);
