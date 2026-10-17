# Visibility culling — do not send what the player cannot see (planned, optional module)

Status: ✅ implemented, default **off** (2026-10-14; cell granularity 2026-10-16). The `16³`
section face graph and the reachable-set BFS live in `aether_world::visibility`; the cache, the
per-player state, the per-cell stone masking and the per-tick reveal budget live in
`crates/aether-server/src/visibility.rs`, wired through `players.rs::render` and the game tick. This
is the `culling` mode of the optional anti-xray module (AX1); the default `engine` mode lives in the
same file and masks individual buried ores instead of whole cells. Config is `[anti_xray]` below.
Not yet done: the optional ray refinement (point 4), re-hiding a cell that becomes hidden again, and
the real light of a hidden cell (the codec reads one `BlockSource` for both blocks and light). See
[Known Issues A.16](../KNOWN_ISSUES.md).

## Granularity: the `4×4×2` AVX-cell (AX3)

The atomic unit is the **cell** (`4×4×2` = 32 blocks, one cache line), not the `16³` section. A
column is sent with **every cell** either real, if the player can see it, or wholly `hidden_fill`
(stone by default). This closes the leak of the old section culling: a partly-visible section (the
surface, a wall the player stands next to) is no longer sent whole, so the ores in its hidden cells
never reach the client.

To keep this cheap, visibility is **two-level**. The coarse search is the same cheap **section**
BFS it always was (`view_distance` is a view distance in sections/chunks, so the radius is
`view_distance × 16` blocks). Then cell granularity is applied **locally, per visible section**: a
section's own face openings decide which of its 128 cells are visible (a flood from the section's
non-opaque boundary cells through its air, plus the opaque cells touching that), computed once and
cached with the section under the same global `EDIT_EPOCH`. There is **no global cell search** — a
full per-cell BFS at a correct radius was measured at ~3.5 s, which is exactly the stall this
rework removes. The reachable set is a set of sections, re-searched only when the player crosses a
**section** border or an in-view section graph changes; the per-cell mask of a column is rebuilt on
demand from the cached section masks.

## Staleness

Because the search is re-run on a **section** border, not a cell border, the reachable section set
(and therefore a column's cell mask) can lag the player by up to the width of a section — at most
~15 blocks of movement — until the next border crossing or a nearby block change. The lag is
conservative (a cell may stay masked for that window, so it under-reveals, never over-reveals): a
cell that becomes visible is revealed on the next re-search, and the `margin_blocks` seed keeps a
whole sphere around the player visible ahead of the exact position so nothing within it can pop in
late. Any block write within the view radius also marks every player whose view covers it stale for
the next tick.


## Two modes (`[anti_xray] mode`)

AX1 ships two strategies behind one config section, `[anti_xray]`:

- **`engine`** (default, cheap): inside *visible* sections, an ore-like block with all six
  neighbours opaque is sent as the surrounding stone/deepslate/netherrack and re-sent for real the
  moment a neighbour changes (break, explosion, fluid) so it is on the client before the player can
  mine to it. This is the design's point 6, `mask_buried_ores`, promoted to the default.
- **`culling`** (this document's design): whole `4×4×2` cells the player cannot see, even after
  turning or moving a little, are sent as stone. Points 1–5 below.

The configurable `hidden_blocks` list (default: every ore, ancient debris and monster spawner)
feeds `engine` directly and is the block set the section graph is not asked about in `culling`
(there the whole section is masked).

## Goal

A `4×4×2` cell the player cannot see — even after turning the head or moving a little — is not sent
as it is. It is sent as **solid stone** (default) or **omitted/air** (option). The unit is the cell,
not the section: a partly-visible `16³` section keeps its visible cells real and its hidden cells
stone.

Why:
- **Anti-xray.** Caves, ores and bases behind solid rock are never on the client, so x-ray mods,
  transparent resource packs and cave-finder tools see only stone. This is the main benefit.
- **Bandwidth and client load.** A hidden section encodes as a single-valued palette (stone): a
  few bytes instead of kilobytes, on 1.18+ protocols. Underground-heavy columns shrink a lot.

Switchable: `[anti_xray] enabled = false` by default until it is proven; per world (`worlds`).

## Design (recommended)

1. **Section visibility graph (cheap, precomputed).** For every `16³` section, compute which of its 6
   faces are connected to each other through non-opaque blocks (a 6×6 bitset, 15 useful bits) —
   the same idea as the vanilla client's own `VisGraph`. Recompute only when a block in the section
   changes from/to opaque (journal events already tell us). Cost: one flood fill over the section's
   blocks; zero per tick. From the same snapshot, derive the section-local visible-cell mask (the
   cell graph) so cell granularity costs nothing extra at search time.
2. **Per player: reachable set.** Breadth-first search over **sections** starting at the player's
   section, entering a neighbour only through connected faces (direction-aware, so a cave behind a
   wall is not entered). Bounded by view distance (in sections). Re-run when the player crosses a
   **section** border or a section's graph changes in range — not every tick, and never per column.
   Cell granularity is *not* a global search: each visible section's cached cell mask says which of
   its cells are visible, applied per column on demand.
3. **Margin ("can see if he turns or moves a bit").** Seed the BFS not only from the player's
   section but from every section within `margin_blocks` (default 4–6, in blocks) of the player, and
   ignore view direction entirely (direction culling is what makes turning the head show holes). So
   the whole sphere around the player that is reachable by sight is sent.

### Reachability rules (AX2 — closed leaks; cells since AX3)

The reachable set must not mistake "the player can see this section's face" for "the player can see
this section". The rules the search follows:

- **A fully opaque section is never visible**, even when its face touches open space. Marking it
  visible sends the whole section real, leaking any ore in it. The player sees only the opaque
  surface, and a stone fill looks the same. This includes the **margin**: the margin seeds every
  section within `margin_blocks` as a *candidate*, but a sealed section stays hidden — only the
  section the player physically stands in is sent even when sealed. The margin only keeps sections
  that actually hold air visible, so it cannot reveal the solid shell around the player or a
  neighbouring cave.
- **Only air lets the search continue.** A face with a non-opaque cell (stone, water, glass) is
  *visible*, but the search enters the section only through an air face. A pool the player looks at
  therefore cannot reveal the flooded caves it drains into; a window is visible but opaque to the
  search.
- **Visibility (not reachability) passes through fully-clear sections** — an all-air or
  fully-flooded section. So an ocean stays water section after section, while its stone floor and the
  stone walls of a flooded cave stop the wave.
- **A sub-section move does not change the answer**: the search is seeded by section, so two
  positions in the same section give the same set and the reveal planner has nothing to re-send.
- **Within a visible section, only the cells its openings reach are real.** The section's own cell
  mask floods from its non-opaque boundary cells through its air and marks those cells (and the
  opaque cells touching them); a cell buried in the section's rock stays hidden.

**Whole-cell invariant.** The send layer never replaces individual blocks inside a hidden cell. For
every column, a cell is either sent **entirely** as its real blocks (it is in the reachable set) or
**entirely** as `hidden_fill` (stone by default — ores, cave air, water, lava, everything).
`MaskedColumn` enforces this per cell, on every send: the first burst, streaming, and every reveal
re-send all go through the same mask, so a re-sent column cannot drop or add masking. See
`a_hidden_cell_is_whole_fill_and_a_visible_cell_is_whole_real`,
`a_partly_visible_section_sends_the_ore_in_a_hidden_cell_as_fill` and
`re_rendering_a_column_keeps_the_mask_the_client_holds`.

4. **Optional ray refinement.** For cells the BFS marks visible only through long thin paths, cast a
   few rays from the eye (+ margin points) to the cell's corners; if every ray is blocked by opaque
   blocks, keep it hidden. Off by default: rays are the expensive and leaky part (diagonal cracks,
   glass, leaves), the graph is the robust part.
5. **Reveal.** When a hidden cell becomes visible (player moved, block broken, explosion, piston),
   send the real column before the client can look at it. Budget re-sends per tick (priority:
   nearest first) to avoid spikes when a player digs into a big cave.
6. **Block-level mode (shipped as the default `engine` mode).** Inside *visible* cells, ores not
   adjacent to any transparent block are sent as the surrounding stone/deepslate/netherrack and
   revealed on exposure (like Paper's anti-xray engine-mode 1). Config
   `[anti_xray] mode = "engine"` and `hidden_blocks`; no separate `mask_buried_ores` flag — engine
   mode *is* that mode. The per-block reveal runs from the same game tick as the cell reveal, at
   block granularity.

## What must keep working

- Collision and movement prediction: hidden = stone keeps the client from falling into "void";
  "omit/air" mode is only for spectators/flying and is riskier. Engine mode replaces an ore with
  the rock that already surrounded it, so collision never changes at all.
- Light: a hidden cell is sent with the stone's light, not the real cell's — the codec asks
  one `BlockSource` for both blocks and light, so masking one masks the other. For the common hidden
  cell (solid rock) those coincide; the gap is recorded in [Known Issues A.16](../KNOWN_ISSUES.md)
  and needs the codec to read a second source.
- Block updates and the journal do not change; this is purely the network send layer per player.
- Every protocol version: hidden-cell encoding goes through the same per-version chunk writer
  (single-valued palette on 1.18+, a full stone section before that); engine mode needs no codec
  work at all because it substitutes whole block states, which every version already speaks.
- Fast movement (elytra, ender pearl, teleport, high speed): widen the margin with velocity, and
  send real cells ahead along the motion vector.
- Admin bypass is explicit (`/visibility off` for a player, the `aether.antixray.bypass`
  permission node granted via `[permissions] grants`, and the `bypass_players` config list);
  **operators are not bypassed automatically**, and `/visibility status` reports the state.

## Tests to write with it

- A sealed cave three sections away is never sent as real; breaking into it reveals it within N ticks.
- Turning 180° in place never shows a hole (no direction culling).
- Surface/open terrain: zero cells hidden (no false positives above ground).
- A partly-visible section keeps its visible cells real and its hidden cells stone.
- Bandwidth benchmark: a deep column with and without the module (see `benches/anti_xray.rs`).
- Reveal ordering under a burst (TNT crater) stays within the per-tick budget.
- Engine mode: an enclosed ore is sent as the surrounding rock; an exposed one is not; breaking a
  neighbour reveals the ore next to it.

## Config sketch

```toml
[anti_xray]
enabled = false
mode = "engine"           # "engine" (buried ores) | "culling" (4×4×2 cells)
hidden_blocks = []        # empty = every ore, ancient debris and monster spawner
worlds = []               # world-dir names this applies to; empty = all
bypass_players = []       # names always allowed to see everything

# culling mode only:
margin_blocks = 5
hidden_fill = "stone"     # "stone" | "air"
reveal_budget_per_tick = 32
```

