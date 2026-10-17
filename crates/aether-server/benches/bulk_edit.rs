//! Bulk-edit throughput: blocks written and committed per second.
//!
//! One revision of a `32×16×32` fill (16,384 blocks) is staged and committed
//! each iteration, flipping the block between runs so every commit is a real
//! change rather than a no-op. The columns are generated once up front, so the
//! number is the write + journal cost the bulk path owns, not worldgen.
//!
//! This is CMD-30a's half of the performance story — the batched write and the
//! one-revision commit — plus CMD-30b's tick budget: one `BulkPublisher::step`
//! over a large queued edit, with the default per-tick caps. The step's setup
//! (cloning the change list into the queue) is excluded by `iter_batched`, so
//! the number is the publish work a tick actually pays.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::time::Duration;

use aether_api::{block_ids, BlockStateId, JournalActor, MemStore, World};
use aether_server::bulk::{BulkEdit, BulkPublisher, PublishBudget, PublishSink};
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

/// A sink that does no I/O but keeps the calls from being optimised away.
#[derive(Default)]
struct CountingSink {
    columns: u64,
    hooks: u64,
}

impl PublishSink for CountingSink {
    fn resend_column(&mut self, _cx: i32, _cz: i32) {
        self.columns += 1;
    }
    fn after_block_break(
        &mut self,
        _actor: JournalActor,
        _x: i32,
        _y: i32,
        _z: i32,
        _was: BlockStateId,
    ) {
        self.hooks += 1;
    }
    fn after_block_place(
        &mut self,
        _actor: JournalActor,
        _x: i32,
        _y: i32,
        _z: i32,
        _now: BlockStateId,
    ) {
        self.hooks += 1;
    }
}

fn bench_bulk_publish(c: &mut Criterion) {
    let world = World::new(MemStore::new(), FlatGenerator::classic());
    // A 64x16x64 edit: 65,536 changed blocks over 16 columns. Small enough to
    // build the committed change list once, large enough that one default
    // step (16,384 cells) is a real slice rather than the whole job.
    let mut edit = BulkEdit::new(JournalActor(1));
    edit.fill(&world, (0, 64, 0), (63, 79, 63), block_ids::STONE)
        .unwrap();
    let committed = edit.commit_with_changes(&world).unwrap();
    let blocks = committed.result.changed as u64;

    let mut group = c.benchmark_group("bulk_publish");
    group.throughput(Throughput::Elements(blocks));
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("step_default_tick_budget", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(committed.actor, committed.changes.clone());
                publisher
            },
            |mut publisher| {
                let mut sink = CountingSink::default();
                let progress = publisher.step(&world, &mut sink, PublishBudget::default());
                black_box((progress, sink.columns, sink.hooks))
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_bulk_commit, bench_bulk_publish);
criterion_main!(benches);
