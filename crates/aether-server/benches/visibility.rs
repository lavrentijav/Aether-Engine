//! Block-write cost of the visibility invalidation hook.
//!
//! Culling is off in this process — no config is installed, so
//! `visibility_enabled()` is false — which is the configuration every default
//! deployment runs. The hook must then cost the same as doing nothing: a read
//! of a `OnceLock` and a branch, no lock and no allocation. The two benches
//! below are meant to be compared against each other; a gap would be the
//! regression the round-2 review flagged (N1).

use aether_server::visibility::invalidate_block;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Block writes per iteration of the inner loop.
const WRITES: i32 = 1024;

/// The same call shape as [`invalidate_block`] with the body removed, so the
/// two benches differ only in the hook's actual cost.
#[inline]
fn no_op(_x: i32, _y: i32, _z: i32) {}

fn bench_visibility(c: &mut Criterion) {
    c.bench_function("invalidate_block_culling_off", |b| {
        b.iter(|| {
            for i in 0..WRITES {
                invalidate_block(black_box(i), black_box(64), black_box(i));
            }
        })
    });

    c.bench_function("no_op_baseline", |b| {
        b.iter(|| {
            for i in 0..WRITES {
                no_op(black_box(i), black_box(64), black_box(i));
            }
        })
    });
}

criterion_group!(benches, bench_visibility);
criterion_main!(benches);
