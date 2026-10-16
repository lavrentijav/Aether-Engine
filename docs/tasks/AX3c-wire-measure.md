# AX3c — wire the cell visibility into the tick/join, measure, document

**STEP 3 of 3 of AX3.** Needs STEP 1 (AX3a) and STEP 2 (AX3b) in `main`.

## Goal
Make the server actually use the **cell** visibility end to end (join burst, streaming, reveal, tick,
`/visibility off|status`, `/spectate xray`), replacing the section BFS call, and prove it is
**faster and stable** (the reason for the change: section culling dropped 2 players to 13–16 TPS with
5–13 s tick stalls).

## What to change
- `crates/aether-server/src/visibility.rs`: `ensure_visible`/`visible_from`/`visible_from_margin`
  call `reachable_boxes(BoxDims::CELL, …)` with a cell-graph cache (same `EDIT_EPOCH` guard as the
  section cache; a key is a cell coord). `desired_hidden`/`mask_for_render`/`reveal_step`/
  `forget_column`/`reset`/`record_sent` operate on cell masks.
- `crates/aether-server/src/players.rs::render` and `crates/aether-server/src/session.rs` join burst:
  build the per-column cell mask (the join burst already precomputes `visible_from` before a handle
  exists; keep that flow, seed the handle).
- Keep the reveal budget (`reveal_budget_per_tick`) and nearest-first order.
- Keep `[anti_xray] mode = "culling"` meaning cell culling; update the startup line / `anti_xray_summary`
  to say "cells 4×4×2". Update `docs/design/visibility-culling.md` (granularity is the AVX-cell now),
  `KNOWN_ISSUES` A.16 and `STATUS` (EN+RU).

## Performance (the acceptance test)
- Measure and **report**: per-join cost, per-tick cost, and encoded chunk size, cell-culling vs the
  old section-culling (a bench or a logged timing run; the repo has `benches/`). The target is that
  cell culling is not slower per tick and does not stall (no `game tick stalled` under a 2-player
  smoke test on fi2).
- If a full per-cell BFS across the view is too heavy, bound it (seed margin, cap the search, reuse
  the parent cell's answer) and say exactly what you bounded.

## Tests
- Every test from the original brief: far buried ore hidden; unseen underwater cave hidden; the
  *partially visible section* leak (visible cells real, hidden cells stone); a one-cell move does not
  change the set (no flicker); whole-cell invariant on every send; `/visibility off` reveals; `/visibility
  status` reports the mode.
- fmt / clippy `-D warnings` / `cargo-locked test --workspace` clean.
- Then the orchestrator deploys to fi2 and checks TPS with 2 players.
