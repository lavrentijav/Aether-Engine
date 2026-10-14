# Queues S1–S4 — next independent features

Read `docs/tasks/COMMON.md` first. Each agent does ONE section, in its own worktree/branch
(`oc/S1` … `oc/S4`), one commit per sub-step (`S1a: ...`). Other agents work in parallel on the
compat layer (`protocol/compat`, codecs) and redstone (`crates/aether-redstone`): do not edit those.
Blocked → `docs/tasks/<branch>-notes.md`, continue. Vanilla 1.21.11 behaviour; the decompiled
sources may be under `/tmp/mc/net/minecraft/` (read-only).

## S1 — visibility culling / anti-xray (optional module)

Design: `docs/design/visibility-culling.md` — implement exactly that, default **off**.
- S1a: per-section visibility graph (6×6 face connectivity through non-opaque blocks, like vanilla
  client `VisGraph`), cached per section, recomputed when a block in it changes opacity. Pure module
  with unit tests (sealed box, tunnel through, L-shaped cave).
- S1b: per-player reachable set: BFS over sections from every section within `margin_blocks` of the
  player, through connected faces, bounded by view distance; recomputed on section-border crossing or
  a graph change in range. No view-direction culling.
- S1c: send layer: hidden sections are encoded as a single-valued stone section (1.18+ formats) /
  full stone section (older), light kept; reveal real sections before they become visible, with a
  per-tick budget (nearest first). Config `[visibility]` as in the design; admin bypass + spectators.
- Tests listed in the design doc (sealed cave never sent; 180° turn shows no hole; open surface hides
  nothing; reveal budget). Add a Criterion bench for the BFS.

## S2 — brewing and potions

Status effects exist (`game/effects.rs`). Add: brewing stand block entity + window (3 bottles,
ingredient, blaze-powder fuel, 400-tick brew), the vanilla potion recipe table (from the data pack /
`PotionBrewing` mixes: awkward/mundane/thick, every effect potion, redstone = longer, glowstone =
stronger, fermented spider eye inversions, gunpowder = splash, dragon's breath = lingering), drinking a
potion (32 ticks, applies effects, returns a glass bottle), splash potions (thrown entity, effect by
distance), water bottles from water. Persist like other containers (`game/containers.rs`). Tests per
recipe family and for the brew timing.

## S3 — mob pathfinding

KNOWN_ISSUES: mobs steer in straight lines. Implement A* over the block grid for ground mobs (vanilla
`WalkNodeEvaluator` rules simplified: walkable = solid below + 2 free cells; step up 1; max drop 3;
avoid lava/fire/cactus; doors as walls for now), path cache per mob, re-path when the target moves more
than N blocks or the path is blocked, node budget per tick across all mobs. Use the per-state collision
shapes (`aether_world::registry::shapes`) so slabs/fences are handled. Zombies chase players around
walls; animals wander with random reachable targets. Tests: maze, around a wall, unreachable target
(gives up), budget respected. Bench the search.

## S4 — incremental relight and light caching

KNOWN_ISSUES/STATUS: light is recomputed per chunk send, no incremental relight, no cross-column
propagation, no caching. Implement: cached light per section (stored with the chunk), incremental
update on a block change (vanilla-style increase/decrease BFS queues for block and sky light), and
propagation across column borders; send Light Update packets to viewers when light changes. Keep the
existing mask-dilation path as the initial full compute. Tests: place/remove a torch (levels around it
exact), dig a shaft (sky light), cross-column torch near a border, equality of incremental result vs a
full recompute on random edits (property test).

## Done

fmt, clippy `-D warnings`, tests of touched crates, commit per sub-step; update `docs/STATUS.md` /
`docs/KNOWN_ISSUES.md` (EN + RU) for what you close. Report: hashes, tests, what is left.
