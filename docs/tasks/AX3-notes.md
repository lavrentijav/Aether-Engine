# AX3 — cell visibility: two-level search, measurements

This note records the rework the `AX3-review-1` blockers forced, and the
measured numbers. It replaces the earlier "global per-cell BFS" design.

## What was wrong (reviewer blockers)

1. **The view radius was fed to a cell search as if it were cells.** The
   `visible_from_margin` path passed `server_view_radius()` / `view_radius` (=8,
   in chunks/sections) straight into `reachable_boxes(BoxDims::CELL, …)`, whose
   `in_view` bound is in *boxes*. So the effective view was 8 cells = 32 blocks,
   not 8 sections = 128 blocks: everything past 32 blocks arrived as stone — a
   wall of stone in the open sky.
2. **A full global cell BFS ran on the tick thread**, on every section-border
   crossing or block write. Measured (release, all-air): ~123 ms at a *wrong*
   radius, and ~3.5 s at the correct radius — 2–70 ticks of stall, the opposite
   of the goal.

## The design now: cheap coarse search + local cell refinement

Visibility is **two-level**:

- **Coarse: the section BFS, unchanged.** `visible_from_margin` runs the
  `16³` section face-graph BFS (`reachable_sections`) with `view_distance` in
  **sections** (so the radius is `view_distance × 16` blocks, as it always
  was). The graph callback is memoised per search, so the process-wide cache is
  touched once per section, not once per edge.
- **Fine: per-section cell masks, cached, no search.** The 128 cells of a
  section are decided **inside that section** from its own blocks: a flood from
  the section's non-opaque boundary cells through its air, plus the opaque
  cells touching that. Computed once per section in `compute_section_cells`
  together with the section graph, cached under the same global `EDIT_EPOCH`,
  and shared by every player. `column_hidden` builds a column's per-cell mask
  by taking each visible section's cached 128-bit mask and leaving every other
  section whole. There is **no global per-cell BFS anywhere**.

The player's own cell is always revealed (`State::own_cell`), so a player
sealed in a one-block pocket is not encased in stone (the old global search
seeded the player's own cell; the structural per-section mask cannot know the
player).

### Why not the global cell BFS

A cell is `4×4×2`; a section holds 128 of them. A full cell BFS over a view is
therefore ~128× the nodes of the section BFS, and its correct-radius cost
(~3.5 s) is the stall the rework removes. The section BFS stays the only search;
the cell decision is a bounded, cached, per-section computation — the owner
asked for exactly this ("bounded computation per section, not a global search").

### Residual: structural over-reveal within a visible section

The per-section cell mask floods from **all** of the section's openings, not
only the faces the player actually sees into. A section visible through one
face therefore reveals cells connected to another of its faces too. That is an
over-reveal of cells in a section that is already visible (never an
under-reveal and never a whole-section leak), and it is documented in
`KNOWN_ISSUES` A.16. Restricting it to the true per-face entry set would need
the BFS to carry entry bits per section; left for a follow-up.

## Measurements

`cargo-locked bench -p aether-server --bench ax3c_cells` (fi2, release, view 8
sections, warm cache, all-air worst case; the host is shared, so treat as
order-of-magnitude):

| bench | before (global cell BFS) | after (two-level) |
|---|---|---|
| per-crossing / per-edit search (`section_join_reachable_view8`) | ~3.5 s (correct radius) | ~41 ms |
| per-column cell mask, whole view (`cell_mask_all_columns_view8`) | n/a | ~8 ms |
| per-tick reveal planner, settled (`cell_tick_reveal_step_budget32`) | ~1 ns | ~3 ns |
| encoded column, cell vs section mask | — | 53 522 B vs 53 524 B |

The number that matters is the search: it is now the section BFS, which is the
per-tick/per-crossing cost. `~41 ms` is the **all-air worst case** (every one
of ~8 000 sections in the view is clear, so the BFS visits them all); a real
underground view visits far fewer. It is under one 50 ms tick and ~85× under
the ~3.5 s the global cell BFS took, so the tick no longer stalls. The
per-cell mask (whole view, ~290 columns) is another ~8 ms, paid on join and on
the reveal pass from cached data. The reveal planner is a no-op guard (~3 ns)
once the search id settles.

## Other review fixes in the same change

- **No `CellMask → HashSet → CellMask` round-trip.** `MaskedColumn` now holds a
  `CellMask` (`from_mask`); the cell mask is a `[u64; 56]`, no `HashSet` is
  built or walked on the send path.
- **Docs corrected** to say "section border" and describe the staleness window
  (`docs/design/visibility-culling.md`, `KNOWN_ISSUES` A.16).
- **Bench wording** fixed: `view8` is 8 *sections*, and the search bench is the
  non-fresh (per-crossing) path.
- **A.16 margin wording** corrected: the margin seeds sections as *candidates*;
  a sealed section stays hidden.
