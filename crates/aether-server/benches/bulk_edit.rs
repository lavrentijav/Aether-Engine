//! Bulk-edit throughput: blocks written and committed per second.
//!
//! One revision of a `32×16×32` fill (16,384 blocks) is staged and committed
//! each iteration, flipping the block between runs so every commit is a real
//! change rather than a no-op. The columns are generated once up front, so the
//! number is the write + journal cost the bulk path owns, not worldgen.
//!
//! This is CMD-30a's half of the performance story — the batched write and the
//! one-revision commit. Chunk resend and lighting are CMD-30b.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use std::time::Duration;

use aether_api::{block_ids, JournalActor, MemStore, World};
use aether_server::bulk::BulkEdit;
use aether_worldgen::FlatGenerator;

const SIDE: i32 = 32;
const HEIGHT: i32 = 16;
const BASE_Y: i32 = 64;

fn bench_bulk_commit(c: &mut Criterion) {
    let world = World::new(MemStore::new(), FlatGenerator::classic());
    // Materialize the target columns so the benchmark measures the bulk write,
    // not generator time (which the baseline model makes a one-off).
    for x in 0..SIDE {
        for z in 0..SIDE {
            let _ = world.get_block(x, BASE_Y, z);
        }
    }

    let blocks = (SIDE as u64) * (SIDE as u64) * (HEIGHT as u64);
    let mut group = c.benchmark_group("bulk_edit");
    group.throughput(Throughput::Elements(blocks));
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(5));

    let mut flip = false;
    group.bench_function("commit_fill_32x16x32", |b| {
        b.iter(|| {
            let block = if flip {
                block_ids::STONE
            } else {
                block_ids::DIRT
            };
            flip = !flip;
            let mut edit = BulkEdit::new(JournalActor(1));
            edit.fill(
                &world,
                (0, BASE_Y, 0),
                (SIDE - 1, BASE_Y + HEIGHT - 1, SIDE - 1),
                block,
            )
            .unwrap();
            black_box(edit.commit(&world).unwrap());
        })
    });
    group.finish();
}

criterion_group!(benches, bench_bulk_commit);
criterion_main!(benches);
