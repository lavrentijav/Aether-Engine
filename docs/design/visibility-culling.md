# Visibility culling — do not send what the player cannot see (planned, optional module)

Status: ✅ implemented, default **off** (2026-10-14). The section face graph and the reachable-set
BFS live in `aether_world::visibility`; the cache, the per-player state, the stone masking and the
per-tick reveal budget live in `crates/aether-server/src/visibility.rs`, wired through
`players.rs::render` and the game tick. This is the `culling` mode of the optional anti-xray module
(AX1); the default `engine` mode lives in the same file and masks individual buried ores instead of
whole sections. Config is `[anti_xray]` below. Not yet done: the optional ray refinement (point 4),
re-hiding a section that becomes hidden again, and the real light of a hidden section (the codec
reads one `BlockSource` for both blocks and light). See
[Known Issues A.16](../KNOWN_ISSUES.md).

## Two modes (`[anti_xray] mode`)

AX1 ships two strategies behind one config section, `[anti_xray]`:

- **`engine`** (default, cheap): inside *visible* sections, an ore-like block with all six
  neighbours opaque is sent as the surrounding stone/deepslate/netherrack and re-sent for real the
  moment a neighbour changes (break, explosion, fluid) so it is on the client before the player can
  mine to it. This is the design's point 6, `mask_buried_ores`, promoted to the default.
- **`culling`** (this document's design): whole sections the player cannot see, even after turning
  or moving a little, are sent as stone. Points 1–5 below.

The configurable `hidden_blocks` list (default: every ore, ancient debris and monster spawner)
feeds `engine` directly and is the block set the section graph is not asked about in `culling`
(there the whole section is masked).

## Goal

A sub-chunk (16×16×16 section) the player cannot see — even after turning the head or moving a
little — is not sent as it is. It is sent as **solid stone** (default) or **omitted/air** (option).

Why:
- **Anti-xray.** Caves, ores and bases behind solid rock are never on the client, so x-ray mods,
  transparent resource packs and cave-finder tools see only stone. This is the main benefit.
- **Bandwidth and client load.** A hidden section encodes as a single-valued palette (stone): a
  few bytes instead of kilobytes, on 1.18+ protocols. Underground-heavy columns shrink a lot.

Switchable: `[anti_xray] enabled = false` by default until it is proven; per world (`worlds`).

## Design (recommended)

1. **Section visibility graph (cheap, precomputed).** For every section, compute which of its 6
   faces are connected to each other through non-opaque blocks (a 6×6 bitset, 15 useful bits) —
   the same idea as the vanilla client's own `VisGraph`. Recompute only when a block in the
   section changes from/to opaque (journal events already tell us). Cost: one flood fill over
   4096 cells on change; zero per tick.
2. **Per player: reachable set.** Breadth-first search over sections starting at the player's
   section, entering a neighbour only through connected faces (direction-aware, so a cave behind a
   wall is not entered). Bounded by view distance. Re-run when the player crosses a section border
   or a section's graph changes in range — not every tick.
3. **Margin ("can see if he turns or moves a bit").** Seed the BFS not only from the player's
   section but from every section within `margin_blocks` (default 4–6) of the player, and ignore
   view direction entirely (direction culling is what makes turning the head show holes). So the
   whole sphere around the player that is reachable by sight is sent.
4. **Optional ray refinement.** For sections the BFS marks visible only through long thin paths,
   cast a few rays from the eye (+ margin points) to the section's corners; if every ray is blocked
   by opaque blocks, keep it hidden. Off by default: rays are the expensive and leaky part
   (diagonal cracks, glass, leaves), the graph is the robust part.
5. **Reveal.** When a hidden section becomes visible (player moved, block broken, explosion,
   piston), send the real section before the client can look at it. Budget re-sends per tick
   (priority: nearest first) to avoid spikes when a player digs into a big cave.
6. **Block-level mode (shipped as the default `engine` mode).** Inside *visible* sections, ores
   not adjacent to any transparent block are sent as the surrounding stone/deepslate/netherrack and
   revealed on exposure (like Paper's anti-xray engine-mode 1). Config
   `[anti_xray] mode = "engine"` and `hidden_blocks`; no separate `mask_buried_ores` flag — engine
   mode *is* that mode. The per-block reveal runs from the same game tick as the section reveal, at
   block granularity.

## What must keep working

- Collision and movement prediction: hidden = stone keeps the client from falling into "void";
  "omit/air" mode is only for spectators/flying and is riskier. Engine mode replaces an ore with
  the rock that already surrounded it, so collision never changes at all.
- Light: a hidden section is sent with the stone's light, not the real section's — the codec asks
  one `BlockSource` for both blocks and light, so masking one masks the other. For the common hidden
  section (solid rock) those coincide; the gap is recorded in [Known Issues A.16](../KNOWN_ISSUES.md)
  and needs the codec to read a second source.
- Block updates and the journal do not change; this is purely the network send layer per player.
- Every protocol version: hidden-section encoding goes through the same per-version chunk writer
  (single-valued palette on 1.18+, a full stone section before that); engine mode needs no codec
  work at all because it substitutes whole block states, which every version already speaks.
- Fast movement (elytra, ender pearl, teleport, high speed): widen the margin with velocity, and
  send real sections ahead along the motion vector.
- Admin bypass is explicit (`/visibility off` for a player, the `aether.antixray.bypass`
  permission node granted via `[permissions] grants`, and the `bypass_players` config list);
  **operators are not bypassed automatically**, and `/visibility status` reports the state.

## Tests to write with it

- A sealed cave three sections away is never sent as real; breaking into it reveals it within N ticks.
- Turning 180° in place never shows a hole (no direction culling).
- Surface/open terrain: zero sections hidden (no false positives above ground).
- Bandwidth benchmark: a deep column with and without the module (see `benches/anti_xray.rs`).
- Reveal ordering under a burst (TNT crater) stays within the per-tick budget.
- Engine mode: an enclosed ore is sent as the surrounding rock; an exposed one is not; breaking a
  neighbour reveals the ore next to it.

## Config sketch

```toml
[anti_xray]
enabled = false
mode = "engine"           # "engine" (buried ores) | "culling" (sections)
hidden_blocks = []        # empty = every ore, ancient debris and monster spawner
worlds = []               # world-dir names this applies to; empty = all
bypass_players = []       # names always allowed to see everything

# culling mode only:
margin_blocks = 5
hidden_fill = "stone"     # "stone" | "air"
reveal_budget_per_tick = 32
```

