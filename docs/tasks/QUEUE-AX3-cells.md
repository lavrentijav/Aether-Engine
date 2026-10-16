# AX3 — visibility culling at AVX-cell granularity (4×4×2)

Read `docs/tasks/COMMON.md`, `docs/design/visibility-culling.md` (the design — follow its intent),
`crates/aether-world/src/cell.rs` (the `AvxCell`: 4×4×2 = 32 blocks, one cache line).

## Why
The shipped `culling` mode (AX1/AX2) works on **16×16×16 sub-chunks**: a whole section a player
cannot see is sent as stone, a section it can see *at all* is sent whole. The owner reports it is
"slow and unstable, and leaks": a partially visible section (the surface, a wall the player stands
next to, their own base) is sent entirely real, so every ore in it is visible — and the section-level
reveal/re-hide churn causes flicker. The atomic unit of this engine's memory model is the
**AVX-Cell (4×4×2)**, not the section; culling must use it.

## Goal
Send, per player, a column whose **every 4×4×2 cell** is either its real blocks (the cell is
visible) or wholly `hidden_fill` (stone). A cell is visible only when it can actually be seen from
the player's eyes (ray/reachability), so:
- a cell behind a wall, inside solid rock, or in an unseen flooded cave → stone;
- a cell on the surface / in an open space the player can see → real;
- **granularity is the cell**, so a partially visible section no longer leaks the ores in its
  hidden cells. Surface and sky stay real; underground the player cannot see is stone.
- Everything hidden, not only ores: ores, cave air, water, lava, dirt — the whole cell is stone.

Whole-cell invariant: a cell is never split — real or `hidden_fill`, on every send (join burst,
streaming, reveal re-send, `/visibility off` re-send).

## Approach (do not guess; verify against the code)
- The section face-graph BFS lives in `crates/aether-world/src/visibility.rs` (`compute_section_graph`,
  `reachable_sections`, `SectionGraph`). It assumes a cubic 16³ section. Generalise the flood fill so
  the same logic runs on a **4×4×2 cell** (dims not equal — parameterise the extents) and produces a
  per-player set of visible **cells**, seeded from the player's cell with the same margin idea and
  bounded by view distance (horizontal) and world height (vertical). Reuse the existing rules: an
  opaque cell is never visible; only passable (air) lets the search continue; a fully-clear cell
  (all air / all water) passes visibility through.
- `crates/aether-server/src/visibility.rs`: replace the section mask (`desired_hidden`, the `u32`
  bitmask over 28 sections, `MaskedColumn::copy_section` filling whole 16³ sections) with a
  **cell mask**: a per-column set of hidden cells, and a `MaskedColumn` (or a new wrapper) whose
  `copy_section` fills each 4×4×2 cell of the section as real or `fill`. Keep `block_at` correct for
  the single-block path too. The wrapper is still a `BlockSource`, so no codec changes.
- The per-player visible-cell set must stay cheap to recompute and must **not** run a wide BFS per
  column or per tick. Cache the per-cell graph the way the section graph is cached (with the same
  `EDIT_EPOCH` guard); recompute the player's set only when they cross a cell border in a way that
  changes the answer, when a visible cell's graph changes, or on reveal. Commit to a **budgeted**
  per-tick reveal like today. If a full per-cell BFS across the view is too heavy, say so in the
  report with numbers and propose the bounded variant you implemented — but the owner's target is
  "faster and stable", so profile it: report the per-join and per-tick cost and the chunk-send size
  versus the old section culling.

## Wire points (verify each)
- `crates/aether-server/src/players.rs::render` — `mask_for_render` path for `ChunkColumn`.
- `crates/aether-server/src/session.rs` — the join burst, which precomputes `visible_from` and masks
  the initial columns before a handle exists; seed the handle so the reveal planner knows what the
  client holds.
- `crates/aether-server/src/visibility.rs` — `reveal_step`/`plan_reveals`, `forget_column`,
  `reset`, `/visibility off` re-send; `record_sent`/`mask_for_render` bookkeeping.
- `game/commands.rs::visibility` and the `/spectate xray` override must keep working.

## Config & docs
- Keep `[anti_xray] mode = "culling"` meaning this cell-level culling (update the `mode` doc comment
  and `anti_xray_summary`/startup line to say "cells 4×4×2"). Do not add a new mode unless you must;
  if you do, document it.
- Update `docs/design/visibility-culling.md` (granularity is the AVX-cell now), the EN+RU
  `KNOWN_ISSUES` A.16 and `STATUS`.

## Edge cases to test explicitly
- a far buried ore behind solid stone is stone (fail-before/pass-after);
- an unseen underwater cave is stone; the water surface the player sees is real;
- a **partially visible section** — the visible cells real, the cells behind the wall stone (this is
  the leak the owner saw);
- a one-cell move does not change the visible set (no flicker), and crossing a cell border changes it
  only by the cells that actually changed;
- whole-cell invariant: every send of a column is per-cell all-real or all-`fill`, stable across
  re-sends;
- `/visibility off` reveals the real cells again; `/visibility status` reports the mode.

## Done criteria
- Unit tests for the cell graph/visibility and the whole-cell mask; the leak tests above.
- `cargo-locked test --workspace`, `fmt --all -- --check`,
  `clippy --workspace --all-targets -j4 -- -D warnings` clean.
- A short report: what changed, the measured per-join/per-tick cost and chunk-send size, and any
  bound/limitation left (e.g. what a very fast-moving player sees). Note anything you deliberately
  did not do.
