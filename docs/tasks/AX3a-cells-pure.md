# AX3a — cell visibility, *pure* half only (aether-world), no server changes

**This is STEP 1 of 3 of AX3 (`docs/tasks/QUEUE-AX3-cells.md`). Do ONLY this step.** Do not touch
`crates/aether-server/**`. Do not run a full-workspace build. Read at most the two files below.

## Files to read (only these)
- `crates/aether-world/src/visibility.rs` (the whole file — it is ~800 lines).
- `crates/aether-world/src/cell.rs` (the `AvxCell`: `DIM_X=4, DIM_Y=2, DIM_Z=4`, 32 blocks).

Do **not** read the server visibility, `players.rs`, `session.rs`, or the codecs. They are STEP 2/3.

## Why
The anti-xray `culling` works on 16×16×16 **sections**. The owner wants it at the **AVX-cell 4×4×2**
granularity (finer: no leak from a partially-visible section; cheaper: less data to reveal). The
pure graph/BFS logic is already in this file and is dimension-agnostic in spirit — only the hardcoded
`16` and the `>> 4` / `& 15` mapping assume a cubic 16³ box. Make it work for a 4×4×2 box too.

Current API in the file (do not break it):
- `SectionGraph` + `compute_section_graph(&[BlockStateId; 4096], is_opaque, is_air)`.
- `reachable_sections(player, margin_blocks, view_distance, y_min, y_max, graph)` → `HashSet<(i32,i32,i32)>`.
- helpers `face_offset`, `opposite`, `default_is_opaque`, `default_is_air`, `SECTION_SIZE`,
  `SECTION_VOLUME`, `FACE_*`, `ALL_FACES`.

## Exact task
1. **Parameterise the box.**
   - Add a small `#[derive(Clone,Copy)] pub struct BoxDims { pub x: usize, pub y: usize, pub z: usize }`
     with `pub const SECTION: BoxDims = BoxDims{x:16,y:16,z:16}` and
     `pub const CELL: BoxDims = BoxDims{x:4,y:2,z:4}` (matching `AvxCell`'s dims — add a test that
     `CELL.x == AvxCell::DIM_X && CELL.y == AvxCell::DIM_Y && CELL.z == AvxCell::DIM_Z`).
   - Volume = `x*y*z`; local index mapping must stay the **existing** one for a section
     (`x | z<<4 | y<<8`) so nothing changes; for a 4×4×2 box use the same shape (`x | z<<2 | y<<4`,
     since z has 4 values → 2 bits, y has 2 values → 1 bit). Provide the mapping as a helper
     (`fn local_index(dims, x,y,z)`), not hardcoded shifts.
2. **Generic graph.** `pub fn compute_box_graph(dims: BoxDims, blocks: &[BlockStateId], is_opaque,
   is_air) -> SectionGraph` — the same algorithm as `compute_section_graph`, using `dims` instead of
   the literal 16. Keep `compute_section_graph` as a thin wrapper:
   `compute_box_graph(BoxDims::SECTION, blocks, is_opaque, is_air)`. `SectionGraph` stays as is (its
   six faces and `adj` are the same for any box).
3. **Generic BFS.** `pub fn reachable_boxes(dims: BoxDims, player: (f64,f64,f64), margin_blocks: f64,
   view_distance: i32, y_min: i32, y_max: i32, graph: impl FnMut(i32,i32,i32)->Option<SectionGraph>)
   -> HashSet<(i32,i32,i32)>` — the same algorithm as `reachable_sections`, but:
   - the player's box coord is `floor(player.x) as i32 >> log2(dims.x)` etc. (write a
     `fn floordiv(v: f64, d: usize) -> i32` = `(v.floor() as i32).div_euclid(d as i32)`),
   - the margin seed box converts the block-space margin to box coords with the **per-axis** dims,
   - `in_view` bounds the horizontal axes by `view_distance` boxes and Y by `[y_min,y_max]`,
   - face stepping is one **box** per step (the graph only knows faces; the caller's `graph` closure
     maps box-coord → graph, so no change beyond the coord math).
   Keep `reachable_sections` as a thin wrapper over `reachable_boxes(BoxDims::SECTION, …)`.
   **Do not change the rules**: a fully-opaque box is not visible (except the player's own box), only
   air continues the search, the full-clear wave passes visibility through. Copy them verbatim.
4. **Tests** (in this file's `#[cfg(test)] mod`, deterministic, no network):
   - `compute_box_graph(BoxDims::CELL, …)` on a 4×4×2 all-air box is `SectionGraph::open()`.
   - a solid-stone 4×4×2 box is `SectionGraph::sealed()`.
   - a box with one air column on the −Y face: `air_faces()` has `FACE_NEG_Y`, `open_faces()` too,
     and `reach` from `FACE_NEG_Y` does not reach `FACE_POS_Y` (blocked by stone).
   - `reachable_boxes` on a hand-built 4×4×2 world: a sealed box adjacent to the player is hidden;
     the player's own box is always visible; a clear box beyond a clear box is visible (the wave).
   - `reachable_boxes(CELL, …)` and `reachable_boxes(SECTION, …)` return the same shape of answer on
     an all-air world (every in-range box visible) — a sanity check that the coord math is right.
   - keep the existing section tests passing unchanged.

## Constraints
- Rust 2021, no new dependencies. Keep `SectionGraph` and every existing public name working.
- `export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0` before any cargo.
- Com**mit** as `AX3a: ...` in the worktree `git -C /srv/dev/projects/Aether-Engine worktree add
  /srv/dev/projects/Aether-Engine-wt/AX3 -b oc/AX3 main` (create it first; work only there).

## Checks (only these — no full-workspace test)
```
export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
cargo-locked test -p aether-world --lib visibility
cargo-locked clippy -p aether-world --all-targets -- -D warnings
cargo fmt -p aether-world -- --check
```
Report: `DONE`, the commit hash, and one line on what STEP 2 must do (the server mask). Nothing else.
