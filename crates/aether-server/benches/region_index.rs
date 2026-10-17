//! Query cost of the region index as the region set grows.
//!
//! The brief's edge case is explicit: "query cost stays flat as region count
//! grows (bench 10 / 1k / 100k regions)". Each fixture spreads single-chunk
//! regions over a grid, so the queried point falls in one chunk whose bucket
//! holds exactly one region at every size. If the query scanned the region set
//! or walked a tree, the time would rise with `n`; a per-chunk bucket stays
//! within noise of a single hash lookup plus one containment test.
//!
//! The second group benches the rebuild: an update (add/remove/move) builds a
//! fresh index off the tick. It is `O(n)` in regions, which is why it is not on
//! the hot path.
//!
//! Reproduce (shared target dir, as in `docs/tasks/COMMON.md`):
//!
//! ```text
//! export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
//! cargo-locked bench -p aether-server --bench region_index
//! ```

use aether_server::region::{BlockPos, Region, RegionId, RegionIndex, RegionStore};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

/// Single-chunk regions spread over a grid, `n` of them. The queried point
/// `(0, 64, 0)` is covered by exactly one region at any `n`.
fn regions(n: usize) -> Vec<Region> {
    (0..n as u64)
        .map(|id| {
            let x = (id as i32 % 1_000) * 32;
            let z = (id as i32 / 1_000) * 32;
            Region::new(
                RegionId(id),
                "overworld",
                BlockPos::new(x, 0, z),
                BlockPos::new(x + 15, 255, z + 15),
            )
        })
        .collect()
}

fn bench_region_index(c: &mut Criterion) {
    let query = BlockPos::new(0, 64, 0);

    let mut query_group = c.benchmark_group("region_index/overlapping");
    for n in [10usize, 1_000, 100_000] {
        let index = RegionIndex::from_regions(regions(n));
        query_group.bench_with_input(BenchmarkId::from_parameter(n), &index, |b, index| {
            b.iter(|| {
                let hits = index.overlapping(black_box(query)).count();
                black_box(hits)
            })
        });
    }
    query_group.finish();

    let mut build_group = c.benchmark_group("region_index/build");
    for n in [10usize, 1_000, 100_000] {
        let handles: Vec<_> = regions(n).into_iter().map(Region::handle).collect();
        build_group.bench_with_input(BenchmarkId::from_parameter(n), &handles, |b, handles| {
            b.iter(|| {
                let index = RegionIndex::build(black_box(handles.iter().cloned()));
                black_box(index.len())
            })
        });
    }
    build_group.finish();

    // An update is a rebuild plus a swap; measure it end to end at the top size.
    let handles: Vec<_> = regions(100_000).into_iter().map(Region::handle).collect();
    c.bench_function("region_index/replace_all_100k", |b| {
        let store = RegionStore::new();
        b.iter(|| {
            store.replace_all(black_box(handles.iter().cloned()));
            black_box(store.snapshot().len())
        })
    });
}

criterion_group!(benches, bench_region_index);
criterion_main!(benches);
