//! AX3c: per-join, per-tick and encoded-size cost of `4×4×2` cell culling.
//!
//! Reproduce (shared target dir, as in `docs/tasks/COMMON.md`):
//!
//! ```text
//! export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
//! cargo-locked bench -p aether-server --bench ax3c_cells
//! ```
//!
//! `view8` below is a **view distance of 8 sections** (chunks), as the server
//! passes it — the coarse search is over `16³` sections, not `4×4×2` cells, so
//! the radius is 8 × 16 = 128 blocks. Cell granularity is applied afterwards,
//! per visible section, by `column_hidden`, from each section's cached mask;
//! there is no global cell search any more.
//!
//! The benches to read, and what they are:
//!
//! - `section_join_reachable_view8` — the search the tick thread runs when a
//!   player crosses a section border or a nearby block changes (the non-fresh
//!   path). Measured ~41 ms in the all-air worst case (every one of ~8 000
//!   sections in the view is clear); a real underground view visits far
//!   fewer. It must stay well below the 50 ms tick, not the seconds the old
//!   per-cell BFS took at a correct radius (~3.5 s).
//! - `cell_mask_all_columns_view8` — building every view column's per-cell
//!   mask from the section set (the join burst and the reveal pass do this),
//!   ~8 ms for ~290 columns.
//! - `cell_tick_reveal_step_budget32` — the per-tick reveal planner on a
//!   settled player: a ~3 ns guard, so an unchanged tick costs nothing.
//! - `cell_column_encode_plain` — encoding one surface column with the cell
//!   mask (~8 ms, the codec dominates).
//!
//! The printed `AX3c encoded column` line compares one partly-visible surface
//! column masked per cell and per section: equal bytes (53 522 vs 53 524 — a
//! hidden cell is a single-value palette either way). The cell mask's win is
//! not bytes but *hiding the buried ore* the section mask would have sent.

use aether_server::protocol::{codec_for, BlockSource, ServerEvent};
use aether_server::visibility::{column_hidden, visible_from, MaskedColumn, State};
use aether_world::registry::blocks;
use aether_world::BlockStateId;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Ground height: stone below `GROUND`, air above. It sits *inside* a section
/// so that section is partly visible — the case the cell mask fixes and the
/// section mask sends whole.
const GROUND: i32 = 8;

/// A partly-visible surface column: air above `GROUND`, stone below, with one
/// ore buried inside the surface section. The section that contains `GROUND`
/// is half air, half rock, so the cell mask hides only the rock cells (ore
/// included) while the section mask sends the whole section real, ore and all.
struct SurfaceWorld {
    stone: BlockStateId,
    ore: BlockStateId,
}

impl SurfaceWorld {
    fn new() -> Self {
        Self {
            stone: blocks::default_state("minecraft:stone").unwrap(),
            ore: blocks::default_state("minecraft:diamond_ore").unwrap(),
        }
    }
}

impl BlockSource for SurfaceWorld {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        if (x, y, z) == (2, 2, 2) {
            return self.ore;
        }
        if y < GROUND {
            self.stone
        } else {
            BlockStateId::AIR
        }
    }
}

/// The stone fill a hidden cell is sent as.
fn stone() -> BlockStateId {
    blocks::default_state("minecraft:stone").unwrap()
}

/// A view of `r` sections around the origin.
fn view_columns(r: i32) -> Vec<(i32, i32)> {
    (-r..=r)
        .flat_map(|cz| (-r..=r).map(move |cx| (cx, cz)))
        .collect()
}

fn bench_ax3c(c: &mut Criterion) {
    let world = SurfaceWorld::new();
    // Warm the section cache once, so the search bench measures the search,
    // not the first flood fill of the view.
    let _ = visible_from(&world, (0.5, 70.5, 0.5), 8);

    c.bench_function("section_join_reachable_view8", |b| {
        b.iter(|| visible_from(black_box(&world), black_box((0.5, 70.5, 0.5)), 8))
    });

    let visible = visible_from(&world, (0.5, 70.5, 0.5), 8);
    let columns = view_columns(8);
    c.bench_function("cell_mask_all_columns_view8", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for &(cx, cz) in &columns {
                total += column_hidden(black_box(&world), &visible, cx, cz).len();
            }
            black_box(total)
        })
    });

    // A per-tick reveal pass on a settled player: the planner must return
    // immediately because the search id has not changed.
    let mut state = State::default();
    state.set_visible_if((0, 4, 0), visible.clone(), 0);
    for &(cx, cz) in &columns {
        let hidden = state.hidden_for(&world, cx, cz);
        state.record_sent(cx, cz, hidden);
    }
    c.bench_function("cell_tick_reveal_step_budget32", |b| {
        b.iter(|| state.reveal_step(black_box(&world), 32))
    });

    // Encoded size of one surface column: cell mask vs the section mask (a
    // section hidden when it is not in the reachable set).
    let codec = codec_for(774).expect("1.21.11 codec");
    let vis = visible_from(&world, (0.5, 70.5, 0.5), 8);
    let cell_hidden = column_hidden(&world, &vis, 0, 0);
    let masked_cells = MaskedColumn::from_mask(&world, 0, 0, cell_hidden, stone());
    let cell_bytes: usize = codec
        .encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &masked_cells)
        .iter()
        .map(|p| p.frame(None).len())
        .sum();

    let mut section_hidden = 0u32;
    for i in 0..aether_server::visibility::SECTION_COUNT {
        let cy = aether_server::visibility::SECTION_MIN_Y + i as i32;
        if !vis.contains(&(0, cy, 0)) {
            section_hidden |= 1 << i;
        }
    }
    let masked_sections = MaskedColumn::new(&world, 0, 0, section_hidden, stone());
    let section_bytes: usize = codec
        .encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &masked_sections)
        .iter()
        .map(|p| p.frame(None).len())
        .sum();
    println!("AX3c encoded column: cells {cell_bytes} B, sections {section_bytes} B");

    c.bench_function("cell_column_encode_plain", |b| {
        b.iter(|| {
            let hidden = column_hidden(&world, &vis, 0, 0);
            let masked = MaskedColumn::from_mask(&world, 0, 0, hidden, stone());
            let packets = codec.encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &masked);
            let bytes: usize = packets.iter().map(|p| p.frame(None).len()).sum();
            black_box(bytes)
        })
    });
}

criterion_group!(benches, bench_ax3c);
criterion_main!(benches);
