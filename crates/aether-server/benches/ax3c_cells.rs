//! AX3c: per-join, per-tick and encoded-size cost of `4×4×2` cell culling,
//! against the old `16³` section culling.
//!
//! Reproduce (shared target dir, as in `docs/tasks/COMMON.md`):
//!
//! ```text
//! export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
//! cargo-locked bench -p aether-server --bench ax3c_cells
//! ```
//!
//! Read the numbers as a ratio, not an absolute: the host is shared. The
//! join bench is the full cell breadth-first search over the view with the
//! per-section cell-graph cache warm (a re-run after a cell-border crossing);
//! the tick bench is the reveal planner's guard on an unchanged search id; the
//! size bench encodes the same surface column once with a per-cell mask and
//! once with the old per-section mask.
//!
//! Measured on fi2 (release, view 8, warm cache; the host is shared, so the
//! join number moves with load):
//!
//! | bench | time |
//! |---|---|
//! | `cell_join_reachable_view8` | ≈ 120–270 ms |
//! | `cell_tick_reveal_step_budget32` | ≈ 1 ns |
//! | `cell_column_encode_plain` | ≈ 7–14 ms |
//!
//! The printed `AX3c encoded column` line compares one partly-visible surface
//! column masked per cell and per section: `53522 B` vs `53524 B` — essentially
//! equal, because a hidden cell is a single-value palette either way. The cell
//! mask's win is not bytes but *hiding the buried ore* the section mask sends.
//!
//! The ≈ 120–270 ms full cell search over an open sky is why the live server
//! recomputes the reachable set only when the player crosses a `16³` section
//! border (reusing the parent section's answer) rather than every `4×4×2`
//! cell — see `section_origin_cell` in `visibility.rs`. The per-tick reveal
//! pass is a no-op guard once the search id settles, so the tick never runs a
//! wide BFS.

use std::collections::HashSet;

use aether_server::protocol::{codec_for, BlockSource, ServerEvent};
use aether_server::visibility::{
    desired_hidden, desired_hidden_cells, visible_from, MaskedColumn, State,
};
use aether_world::registry::blocks;
use aether_world::visibility::BoxDims;
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

fn bench_ax3c(c: &mut Criterion) {
    let world = SurfaceWorld::new();
    // Warm the per-section cell-graph cache once, so the join bench measures
    // the search, not the first computation.
    let _ = visible_from(&world, (0.5, 70.5, 0.5), 8);

    c.bench_function("cell_join_reachable_view8", |b| {
        b.iter(|| visible_from(black_box(&world), black_box((0.5, 70.5, 0.5)), 8))
    });

    // A per-tick reveal pass on a settled player: the planner must return
    // immediately because the search id has not changed.
    let visible = visible_from(&world, (0.5, 70.5, 0.5), 8);
    let mut state = State::default();
    state.set_visible_if((0, 35, 0), visible.clone(), 0);
    for cx in -8..=8 {
        for cz in -8..=8 {
            let hidden = state.hidden_for(cx, cz);
            state.record_sent(cx, cz, &hidden);
        }
    }
    c.bench_function("cell_tick_reveal_step_budget32", |b| {
        b.iter(|| state.reveal_step(32))
    });

    // Encoded size of one surface column: cell mask vs the old section mask
    // (a section counts as visible when any of its cells is).
    let codec = codec_for(774).expect("1.21.11 codec");
    let vis = visible_from(&world, (0.5, 70.5, 0.5), 8);
    let cell_hidden = desired_hidden_cells(&vis, BoxDims::CELL, 0, 0);
    let masked_cells = MaskedColumn::from_cells(&world, 0, 0, cell_hidden, stone());
    let cell_bytes: usize = codec
        .encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &masked_cells)
        .iter()
        .map(|p| p.frame(None).len())
        .sum();

    let mut section_visible: HashSet<(i32, i32, i32)> = HashSet::new();
    for (x, y, z) in &vis {
        section_visible.insert((x >> 2, y >> 3, z >> 2));
    }
    let section_hidden = desired_hidden(&section_visible, 0, 0);
    let masked_sections = MaskedColumn::new(&world, 0, 0, section_hidden, stone());
    let section_bytes: usize = codec
        .encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &masked_sections)
        .iter()
        .map(|p| p.frame(None).len())
        .sum();
    println!("AX3c encoded column: cells {cell_bytes} B, sections {section_bytes} B");

    c.bench_function("cell_column_encode_plain", |b| {
        b.iter(|| {
            let hidden = desired_hidden_cells(&vis, BoxDims::CELL, 0, 0);
            let masked = MaskedColumn::from_cells(&world, 0, 0, hidden, stone());
            let packets = codec.encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &masked);
            let bytes: usize = packets.iter().map(|p| p.frame(None).len()).sum();
            black_box(bytes)
        })
    });
}

criterion_group!(benches, bench_ax3c);
criterion_main!(benches);
