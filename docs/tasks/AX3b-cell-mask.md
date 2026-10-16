# AX3b — cell mask in the server (server only, no BFS wiring yet)

**STEP 2 of 3 of AX3.** Do ONLY this step. Needs STEP 1 (AX3a) merged into `main`.
Read `docs/tasks/AX3a-cells-pure.md` for the pure API it builds on.

## Goal
Replace the *section* mask in `crates/aether-server/src/visibility.rs` with a **cell (4×4×2) mask**,
so a column is encoded with every 4×4×2 cell either wholly real or wholly `hidden_fill` — proven by
unit tests on the mask layer alone (no live BFS, no player). This is the layer the owner's leak
(partially-visible section) lives in.

## What to change
- `MaskedColumn` currently takes a `u32` bitmask over the 28 *sections* of a column and fills a whole
  16³ section via `copy_section`. Give it a **per-column cell mask** instead: for column `(cx, cz)`
  the cells are `(cx*4 + i, cy, cz*4 + j)` for `cx`-local `i∈0..4`, `z`-local `j∈0..4`, and all world
  `cy` in the vertical cell range. A block is hidden when its cell is not in the visible set.
  - Keep it a `BlockSource` (so no codec changes): `block_at` fills a hidden cell's block, and
    `copy_section` fills each 4×4×2 *cell* of the requested 16³ section as real or `fill`.
  - The representation must be cheap to build per column and to query per block. A `HashSet<(i32,i32,i32)>`
    of visible cells is the straightforward choice; if you have a clearly better one, note it.
- Add the pure helper the *wiring* step will use: `desired_hidden_cells(visible: &HashSet<(i32,i32,i32)>, dims, cx, cz) -> ...`
  returning, for one column, which cells are hidden. Keep the old `desired_hidden`/`SECTION_*` until
  the wiring step removes them, or remove them here if nothing else uses them (check with grep).

## Tests (fail-before/pass-after, mask layer only)
- `MaskedColumn` over a column hides **exactly** the requested cells and leaves every other cell
  byte-identical to the source (per-cell assertion, not per-section).
- The whole-cell invariant: a hidden cell is all `fill`, a visible cell is all real, on *every*
  `copy_section` and `block_at` — stable when the same column is re-encoded.
- The owner's leak: a 16³ section that is *partly* visible (a 4×4×2 cell visible, the rest hidden)
  encodes the visible cell real and the hidden cells `fill` — i.e. the ores in the hidden cells do
  not reach the client. A matching fail-before/pass-after pair.
- `desired_hidden_cells` on a hand-built visible set lists exactly the missing cells of that column.

## Constraints
- Do not touch the BFS, `ensure_visible`, the tick, `players.rs`/`session.rs` in this step (STEP 3).
- `export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0`; worktree
  `git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/AX3b -b oc/AX3b main`.
- Checks: `cargo-locked test -p aether-server --lib visibility`, `cargo-locked clippy -p aether-server --all-targets -- -D warnings`, `cargo fmt -p aether-server -- --check`.
- Commit `AX3b: ...`. Report `DONE` + hash + one line on STEP 3.
