//! Bulk-edit throughput and the CMD-30b per-tick publish budget.
//!
//! Three groups:
//!
//! * `bulk_edit/commit_fill_32x16x32` — the synchronous write half (CMD-30a):
//!   stage and commit one revision of 16,384 blocks. This is the half the
//!   design note says must run **off** the tick thread.
//! * `bulk_publish/*` — the tick-visible half, split by the phase a real tick
//!   can be in when the budget ends:
//!   - `settle_step`: one default settle slice;
//!   - `last_settle_slice`: the final full seed slice of a dense edit;
//!   - `frontier_to_resend_step_real_encoder`: the worst combined step — a
//!     full fixed-point frontier slice followed by `columns_per_tick` encodes;
//!   - `hooks_step`: a step that only fires `hooks_per_tick` **sink calls**
//!     (not plugin dispatch — that cost lives in the plugin host and is
//!     unbounded in plugin code);
//!   - `sparse_settle_step` and `fence_sparse_settle_step`: isolated changed
//!     cells, with unchanged neighbours that are stone (reshape is a no-op) and
//!     fences (four-six neighbour reads plus `with_props` each), the latter
//!     being the worst shape case.
//!
//! Setup (building the committed change list, and for the phase benches the
//! earlier slices) is excluded by `iter_batched`, so each number is the work a
//! tick actually pays in that phase. Read absolute times as load-sensitive on
//! the shared builder; the group is meaningful as a ratio to the 50 ms tick.
//!
//! ```text
//! export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
//! cargo-locked bench -p aether-server --bench bulk_edit
//! ```

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::time::Duration;

use aether_api::{block_ids, BlockStateId, JournalActor, MemStore, World};
use aether_server::bulk::{BulkEdit, BulkPublisher, PublishBudget, PublishSink};
use aether_server::protocol::{codec_for, BlockSource, ColumnView, ServerEvent};
use aether_worldgen::FlatGenerator;

const SIDE: i32 = 32;
const HEIGHT: i32 = 16;
const BASE_Y: i32 = 64;

type BenchWorld = World<MemStore, FlatGenerator>;

/// The bench world seen as a `BlockSource`, the exact view a codec reads. The
/// whole-column snapshot makes the encode see one state, as on a live server.
struct BenchSource<'a>(&'a BenchWorld);

impl BlockSource for BenchSource<'_> {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        self.0.get_block(x, y, z)
    }
    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        self.0.copy_section(cx, cy, cz, out);
    }
    fn section_y_range(&self) -> (i32, i32) {
        (-4, 23)
    }
    fn copy_column(
        &self,
        cx: i32,
        cz: i32,
        cy_min: i32,
        cy_max: i32,
    ) -> Option<Vec<Box<[BlockStateId; 4096]>>> {
        Some(self.0.copy_column_sections(cx, cz, cy_min, cy_max))
    }
}

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

/// A sink that encodes every re-sent column with the real codec, so the number
/// includes palette packing, the light pass and framing — what a viewer's tick
/// actually costs.
struct WireSink<'a> {
    source: &'a dyn BlockSource,
    codec: &'static dyn aether_server::protocol::ProtocolCodec,
    columns: u64,
    bytes: u64,
}

impl PublishSink for WireSink<'_> {
    fn resend_column(&mut self, cx: i32, cz: i32) {
        // The exact shape the render path uses: one whole-column snapshot,
        // then the codec reads every block and the light pass from it.
        let view = ColumnView::new(self.source, cx, cz);
        let packets = self
            .codec
            .encode(&ServerEvent::ChunkColumn { cx, cz }, &view);
        self.columns += 1;
        self.bytes += packets
            .iter()
            .map(|p| p.frame(None).len() as u64)
            .sum::<u64>();
    }
    fn after_block_break(
        &mut self,
        _actor: JournalActor,
        _x: i32,
        _y: i32,
        _z: i32,
        _was: BlockStateId,
    ) {
    }
    fn after_block_place(
        &mut self,
        _actor: JournalActor,
        _x: i32,
        _y: i32,
        _z: i32,
        _now: BlockStateId,
    ) {
    }
}

fn committed_fill(
    world: &BenchWorld,
    x1: i32,
    y1: i32,
    z1: i32,
    x2: i32,
    y2: i32,
    z2: i32,
) -> aether_server::bulk::BulkCommit {
    let mut edit = BulkEdit::new(JournalActor(1));
    edit.fill(world, (x1, y1, z1), (x2, y2, z2), block_ids::STONE)
        .unwrap();
    edit.commit_with_changes(world).unwrap()
}

fn bench_bulk_publish(c: &mut Criterion) {
    let world = World::new(MemStore::new(), FlatGenerator::classic());
    // 64x16x64 = 65,536 changed blocks over 16 columns: four default settle
    // slices, then the re-sends.
    let dense = committed_fill(&world, 0, 100, 0, 63, 115, 63);
    let blocks = dense.result.changed as u64;
    let source = BenchSource(&world);
    let codec = codec_for(773).expect("the 1.21.11 codec");

    // A fence-heavy sparse edit: even coordinates are changed fences, odd
    // neighbours are pre-existing fences, so every frontier reshape does the
    // expensive connection work rather than a stone fast path.
    let fence = aether_world::registry::blocks::default_state("minecraft:oak_fence").unwrap();
    // Fill every odd coordinate in the region with a fence, so each changed
    // (even-coordinate) cell has six unchanged fence neighbours.
    for x in 0..64 {
        for z in 0..64 {
            for y in 300..332 {
                if x % 2 == 1 || y % 2 == 1 || z % 2 == 1 {
                    world.set_block(x, y, z, "minecraft:oak_fence").unwrap();
                }
            }
        }
    }
    let mut fence_edit = BulkEdit::new(JournalActor(1));
    let mut fences = 0;
    'find: for x in (0..64).step_by(2) {
        for z in (0..64).step_by(2) {
            for y in (300..332).step_by(2) {
                fence_edit.set(&world, x, y, z, fence).unwrap();
                fences += 1;
                if fences == 8_192 {
                    break 'find;
                }
            }
        }
    }
    let sparse_fence = fence_edit.commit_with_changes(&world).unwrap();
    debug_assert_eq!(sparse_fence.result.changed, 8_192);

    let mut group = c.benchmark_group("bulk_publish");
    group.throughput(Throughput::Elements(blocks));
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("settle_step", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(dense.actor, dense.changes.clone());
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

    group.bench_function("last_settle_slice", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(dense.actor, dense.changes.clone());
                let mut sink = CountingSink::default();
                // 65,536 = 16 x 4,096: leave the final full seed slice.
                for _ in 0..15 {
                    publisher.step(&world, &mut sink, PublishBudget::default());
                }
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

    group.bench_function("frontier_to_resend_step_real_encoder", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(dense.actor, dense.changes.clone());
                let mut sink = CountingSink::default();
                // Settle every seed, then drain the frontier down to one
                // default slice so the measured step does a full frontier
                // pass and enters Resend in the same tick.
                while publisher.pending_seeds() > 0 || publisher.pending_frontier() > 4_096 {
                    let p = publisher.step(&world, &mut sink, PublishBudget::default());
                    debug_assert_eq!(p.columns, 0, "still settling");
                }
                publisher
            },
            |mut publisher| {
                let mut sink = WireSink {
                    source: &source,
                    codec,
                    columns: 0,
                    bytes: 0,
                };
                let progress = publisher.step(&world, &mut sink, PublishBudget::default());
                black_box((progress, sink.columns, sink.bytes))
            },
            BatchSize::SmallInput,
        )
    });

    group.bench_function("hooks_step", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(dense.actor, dense.changes.clone());
                let mut sink = CountingSink::default();
                // Settle and re-send everything, then fire exactly one hook;
                // the measured step is the remaining hook slice.
                let first = publisher.step(
                    &world,
                    &mut sink,
                    PublishBudget {
                        cells: usize::MAX,
                        columns: usize::MAX,
                        hooks: 1,
                    },
                );
                debug_assert_eq!(first.hooks, 1);
                debug_assert_eq!(first.remaining, 1);
                publisher
            },
            |mut publisher| {
                let mut sink = CountingSink::default();
                let progress = publisher.step(&world, &mut sink, PublishBudget::default());
                black_box((progress, sink.hooks))
            },
            BatchSize::SmallInput,
        )
    });

    // Worst shape work per budgeted cell: isolated changes, so every cell
    // pays its own reshape and six unchanged-neighbour reshapes.
    let mut sparse_edit = BulkEdit::new(JournalActor(1));
    let mut placed = 0;
    'outer: for x in (0..64).step_by(2) {
        for z in (0..64).step_by(2) {
            // Every coordinate is even, so all six neighbours are odd
            // (unchanged) cells: the full cell + six-neighbour reshape cost.
            for y in (300..332).step_by(2) {
                sparse_edit.set(&world, x, y, z, block_ids::STONE).unwrap();
                placed += 1;
                if placed == 16_384 {
                    break 'outer;
                }
            }
        }
    }
    let sparse = sparse_edit.commit_with_changes(&world).unwrap();
    debug_assert_eq!(sparse.result.changed, 16_384);
    group.bench_function("sparse_settle_step", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(sparse.actor, sparse.changes.clone());
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

    group.bench_function("fence_sparse_settle_step", |b| {
        b.iter_batched(
            || {
                let mut publisher = BulkPublisher::new();
                publisher.enqueue(sparse_fence.actor, sparse_fence.changes.clone());
                let mut sink = CountingSink::default();
                // The seed slice only reshapes changed cells; the expensive
                // six-neighbour pass is the frontier step measured here.
                while publisher.pending_seeds() > 0 {
                    publisher.step(&world, &mut sink, PublishBudget::default());
                }
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
