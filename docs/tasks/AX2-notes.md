# AX2 notes — culling leak fixes

Branch `oc/AX2` from `main` (`decfe1d`). Fixes the live report on `fi2` with
`[anti_xray] enabled = true, mode = "culling"`:

1. ores visible, especially far away;
2. underwater / water caves visible;
3. coal disappears, then reappears on a small position change.

## Root causes

- **The whole "shell" of stone next to open space was marked visible.** The BFS marked a
  neighbour section visible whenever a face touched the visible region, even if that section was
  fully opaque. Since masking is per section, the entire first shell of stone around every cave and
  around the surface was sent real — every ore in it leaked. Far away, above ground, that shell is
  the dominant visible thing, hence "especially far away". Removed: a fully opaque face now marks
  nothing.
- **Water was see-through *and* passable.** `default_is_opaque` only checked `solid`, so the BFS
  walked through water. A pool (or an ocean) the player sees flooded every flooded cave connected
  to it, revealing systems behind rock. Fixed by splitting the graph: `open` faces (non-opaque:
  air, water, glass) decide *visibility*, `air` faces decide whether the search *continues*.
  Water/glass are visible but opaque to the search.
- **The two above were also the flicker.** Whether a shell/water section was a neighbour of the
  reached set toggled as the search re-ran on chunk crossings, so the reveal planner fired
  spurious re-sends. Both are now stable.

## What changed

- `aether_world::visibility`: `SectionGraph` now carries `open`, `air`, `adj` and `full_clear`.
  `compute_section_graph` takes `is_opaque` *and* `is_air`; a second pass marks the visible faces
  so a fluid window is seen but not walked through. `reachable_sections` only propagates through
  air faces, never marks a fully opaque neighbour, and runs a visibility wave through fully-clear
  sections so an ocean stays water while its stone floor and cave walls stop the wave.
- `default_is_air` added (air-like: not solid, not colliding, light opacity 0).
- Startup log line `anti-xray : <mode> …` via `anti_xray_summary` (mode, worlds, bypass list).

## Reproduction tests (fail before, pass after)

- `visibility::tests::a_far_buried_ore_behind_solid_stone_is_hidden` — a far ore one section under
  the surface (the shell) and another deeper must both be hidden.
- `visibility::tests::an_unseen_water_cave_is_hidden` — a water shaft from the surface down into a
  flooded cave; the cave must be hidden.
- `visibility::tests::a_one_block_move_does_not_change_the_visible_set` — two positions in the same
  section give the same reachable set, so the reveal plan is empty.
- `aether_world::visibility::tests::the_visibility_wave_passes_through_water_but_stops_at_stone` —
  a chain of all-water sections is visible, but a section with one stone cell stops the wave.
- Updated `a_sealed_neighbour_blocks_the_search` and `a_tunnel_of_open_sections_is_fully_reached`
  to the new (correct) expectation: a sealed section is hidden.

## Not fixed / documented

- `culling` is section-level: an ore in a section the player can see part of (a surface section) is
  still sent. That is `engine` mode's job. See KNOWN_ISSUES A.16 (EN+RU).
- A fully-water flooded cave connected by fully-water sections is still visible (no ores).

## Follow-up (owner clarification): whole-section fill, no selective masking

The owner confirmed the rule: in `culling`, a hidden section is replaced **entirely** by
`hidden_fill` (stone) — ores, cave air, water, lava, all of it — and there is no selective
block-by-block replacement. Also: verify every sent section is either wholly visible or wholly
stone, and that this holds on re-sends.

### What was found

- The fill was already whole-section (`MaskedColumn`), and every `ChunkColumn` send (join burst,
  streaming, reveal) goes through `mask_for_render`, so no column went out unmasked and no
  selective fill existed. The remaining leak was **classification**: the margin seeded *every*
  section within `margin_blocks` visible, including fully opaque ones. So the solid shell next to
  the player (and its sealed caves) was sent whole and leaked, and — because the seed box shifts by
  a whole section as the player crosses a border — it flickered in and out on a small move.

### Fix

- `reachable_sections` no longer seeds a fully opaque section; only the section the player actually
  stands in may be sent sealed. Sections that hold air are still seeded by the margin (so turning
  does not pop them in), but a sealed section anywhere else stays stone.
- Tests: `the_margin_never_reveals_a_sealed_section`,
  `the_margin_keeps_air_sections_near_the_player_visible`,
  `a_player_sealed_in_stone_sends_only_its_own_section` (aether-world);
  `a_masked_column_is_whole_sections_stone_or_whole_sections_real`,
  `re_rendering_a_column_keeps_the_mask_the_client_holds` (aether-server).
- INFO line `anti-xray : <mode> …` was already added (mode, worlds, bypass list).

### Invariant now guaranteed and tested

For every sent column, each section is either entirely its real blocks (it is in the reachable set)
or entirely `hidden_fill`. Verified per cell in `a_masked_column_is_whole_sections_stone_or_whole_sections_real`
and made stable across re-renders by `re_rendering_a_column_keeps_the_mask_the_client_holds`.

