# Queue R — redstone foundation (stage 1: skeleton + minimum)

Read `docs/tasks/COMMON.md` first. Worktree `/srv/dev/projects/Aether-Engine-wt/QR`, branch `oc/QR`.
One commit per task (`R1: ...`). Blocked → `docs/tasks/QR-notes.md`, continue.

## Scope of THIS stage

Build the redstone engine as a **standalone crate, not connected to the game**. Do not edit
`aether-server` game code, the tick loop, or the protocol. The game hook comes in a later stage, after
review. The goal of this stage: a clean, tested core that later stages extend without rewriting.

Behaviour target: vanilla 1.21.11 (Java). Reference: the decompiled game sources if present under
`/tmp/mc/net/minecraft/` (e.g. `world/level/block/RedStoneWireBlock.java`, `DiodeBlock.java`,
`RepeaterBlock.java`, `RedstoneTorchBlock.java`, `level/redstone/*`), else the Minecraft wiki.
`docs/ROADMAP.md` (Phase 1: "compiled Directed Dependency Graph… QC deviations allowed") and
`docs/KNOWN_ISSUES.md` section B (Redstone, Update order) describe the long-term design; this stage
must not block it, but implement the simple vanilla-style update model first.

## R1 — crate skeleton and the world interface

- New crate `crates/aether-redstone` (workspace member; follow `crates/aether-physics` as the model:
  no storage access, `//!` module docs, `-D warnings` clean). Depends on `aether-core`/`aether-world`
  only for ids/positions if needed.
- `trait RedstoneView` — what the engine needs from the world: read a block state at a position,
  write a block state, "is this a redstone conductor (full solid opaque block)", and schedule a block
  tick (pos, delay, priority). A `MemWorld` test implementation in the crate (a `HashMap` of states +
  a tick queue) so every test runs without the server.
- Block states are `aether_world::BlockStateId`; read/write redstone properties (`power`, `powered`,
  `lit`, `delay`, `facing`, `north/east/south/west` wire sides) through the existing state/property
  helpers in `aether-world` (find them; do not invent a second property system).
- `struct Redstone` — the engine: `on_neighbour_changed(view, pos, from)`, `on_placed`,
  `on_removed`, `on_scheduled_tick(view, pos)`, `on_use` (lever/button). Pure functions over the view.

## R2 — power model

- Weak vs strong power exactly as vanilla: `get_signal(pos, dir)` (weak), `get_direct_signal` (strong),
  `has_neighbor_signal`, `get_best_neighbor_signal`; conductors pass strong power into weak power.
- Sources: lever, button (stone 20 ticks / wood 30), redstone block, redstone torch (inverts the block
  it is attached to, 2-tick delay, burnout rule: more than 8 toggles in 60 ticks → burnt out).

## R3 — redstone wire

- Power level 0–15, decays by 1 per block, connection shape (side/up/none) incl. up-and-over a
  conductor and being cut by a conductor above; wire powers the block it sits on and the block it
  points into (weak), not sideways blocks it does not point at.
- Update order: implement vanilla's default ("Eigencraft" turned off — the 1.21.11 default
  algorithm). Document in the crate docs which order you implemented and where it may differ.

## R4 — repeater and lamp

- Repeater: delay 1–4 (×2 game ticks), locking by a side repeater/comparator, pulse extension,
  priority ticks (repeater facing into a diode gets higher priority, as vanilla).
- Redstone lamp: lights immediately, turns off after 4 ticks.

## R5 — tests (this is what the review looks at)

Table-driven tests on `MemWorld`, each a tiny circuit built in code:
- lever → wire ×15 → lamp lit; wire ×16 → lamp off at the end.
- torch on a block powered by a lever → torch off; NOT gate; torch burnout clock.
- repeater 4-tick delay timing exact; repeater lock; 1-tick pulse through a repeater extended to its delay.
- wire up-and-over a block; wire cut by a conductor on top.
- strong vs weak: lever on a block powers adjacent wire through the block; a wire-powered block does
  not power a wire next to it (weak only).
- determinism: the same circuit run twice gives the same tick-by-tick trace.
Give each test a short comment with the vanilla behaviour it pins down.

## Not in this stage

Comparators, observers, pistons, hoppers interaction, droppers/dispensers activation, doors/trapdoors
reacting to power, rails, quasi-connectivity, the dependency-graph compiler, wiring into the server.
List them in the crate docs as "next stages".

## Done

fmt, clippy `-D warnings`, `cargo test -p aether-redstone`, commit per task. Do not touch
`docs/STATUS.md` beyond one line "redstone core crate (not wired)". Report: hashes, test list, and
an honest list of known differences from vanilla.
