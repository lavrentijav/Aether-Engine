# Visibility culling — do not send what the player cannot see (planned, optional module)

Status: ✅ implemented, default **off** (2026-10-14). The section face graph and the reachable-set
BFS live in `aether_world::visibility`; the cache, the per-player state, the stone masking and the
per-tick reveal budget live in `crates/aether-server/src/visibility.rs`, wired through
`players.rs::render` and the game tick. Config is `[visibility]` below. Not yet done: the optional
ray refinement (point 4) and block-level buried-ore masking (point 6) — parsed and ignored; the real
light of a hidden section (the codec reads one `BlockSource` for both blocks and light), and
re-hiding a section that becomes hidden again. See [Known Issues A.16](../KNOWN_ISSUES.md).

## Goal

A sub-chunk (16×16×16 section) the player cannot see — even after turning the head or moving a
little — is not sent as it is. It is sent as **solid stone** (default) or **omitted/air** (option).

Why:
- **Anti-xray.** Caves, ores and bases behind solid rock are never on the client, so x-ray mods,
  transparent resource packs and cave-finder tools see only stone. This is the main benefit.
- **Bandwidth and client load.** A hidden section encodes as a single-valued palette (stone): a
  few bytes instead of kilobytes, on 1.18+ protocols. Underground-heavy columns shrink a lot.

Switchable: `visibility.enabled = false` by default until it is proven; per world.

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
6. **Block-level mode (later, optional).** Inside *visible* sections, ores not adjacent to any
   transparent block can still be sent as stone and revealed on exposure (like Paper's anti-xray
   engine-mode 1). Separate flag `visibility.mask_buried_ores`.

## What must keep working

- Collision and movement prediction: hidden = stone keeps the client from falling into "void";
  "omit/air" mode is only for spectators/flying and is riskier.
- Light: hidden sections are sent with the light the real section has, so nothing flickers on reveal.
- Block updates and the journal do not change; this is purely the network send layer per player.
- Every protocol version: hidden-section encoding goes through the same per-version chunk writer
  (single-valued palette on 1.18+, a full stone section on older formats).
- Fast movement (elytra, ender pearl, teleport, high speed): widen the margin with velocity, and
  send real sections ahead along the motion vector.
- Admin bypass (`/aether visibility off` for a player), and spectators get everything.

## Tests to write with it

- A sealed cave three sections away is never sent as real; breaking into it reveals it within N ticks.
- Turning 180° in place never shows a hole (no direction culling).
- Surface/open terrain: zero sections hidden (no false positives above ground).
- Bandwidth benchmark: deep column with/without the module.
- Reveal ordering under a burst (TNT crater) stays within the per-tick budget.

## Config sketch

```toml
[visibility]
enabled = false
margin_blocks = 5
hidden_fill = "stone"     # "stone" | "air"
ray_refinement = false
mask_buried_ores = false
reveal_budget_per_tick = 32
```
