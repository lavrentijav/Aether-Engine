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
