Verified against the Java in `/tmp/mc` and probes under `/tmp/opencode/probe`. `cargo test --locked -p aether-redstone` and `cargo clippy --all-targets` are clean (16 unit + 27 integration + 1 doc). No repo files edited.

## Status of round-2 findings

| # | Finding | Status |
|---|---------|--------|
| N1 | Engine-global torch burnout log | **FIXED** — log moved to `RedstoneView::record_toggle`/`toggle_count` (`view.rs:198/201`), `MemWorld` prunes at 60 ticks (`view.rs:324-343`); `Redstone` is `Copy`/stateless (`engine.rs:36`). Probe: two worlds on one engine no longer share history |
| N2 | Observer wire connection needs a `wire.rs` edit | **FIXED** — `BlockBehaviour::connects_to_wire` (`blocks/mod.rs:85`), `wire.rs:211` delegates, `TestObserver` overrides it; unit test `a_custom_block_decides_its_own_wire_connection` passes |
| N3 | Removal hook before write / default over-notifies | **FIXED** — write first, then `on_remove` (`behaviour.rs:270`), default `on_remove` empty (`blocks/mod.rs:155`), sole fan-out is the `UPDATE_NEIGHBORS` pass; spy unit tests pass |
| N4 | No recursion / no indirect shape updates | **FIXED** — `set_block_at` recurses with a decrementing limit, `update_shapes` runs the old-indirect / new-direct / new-indirect passes, wire implements `update_indirect_neighbour_shapes`; `a_shape_change_wakes_a_diagonal_wire` passes |
| N5 | Repeater skipped vertical lock recompute | **FIXED** — `repeater.rs` recomputes `LOCKED` for any dir off the facing axis; test passes |
| N6 | except-facing update used wrong source | **PARTIAL** — the source *state* is now the repeater (`repeater.rs:257`), test passes; but `NeighborSource.pos` is still `output`, not the repeater (`behaviour.rs:351/521`), so a component reading `from.pos` gets the wrong cell |
| N7 | No `UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE` (128) | **FIXED** — flag added (`view.rs:98`) and honoured in `execute_shape_update`; test passes |
| N8 | Support destruction absent | **PARTIAL** — wire/repeater/standing+wall torch/lever/button pop via `update_shape`; lever-pop cascade verified correct. But no item is dropped (documented), the support predicate is approximated (below), and the pop cascade is subject to the new truncation defect |

## New defects, strongest first

**D1 — FATAL. Neighbour-update chain limit is 512, mislabelled as vanilla's default.** `behaviour.rs:52` `const MAX_CHAINED: usize = 512;` (comment `:51` "vanilla's `maxChainedNeighborUpdates`"), enforced in `add_and_run` (`behaviour.rs:449`). Vanilla's default is **1,000,000** (`DedicatedServerProperties.java:100`, `max-chained-neighbor-updates=1000000`); `CollectingNeighborUpdater` only drops updates past that. R7 replaced the old BFS engine's `MAX_UPDATES = 1_000_000` with 512.
Circuit: a 7×7 redstone-wire patch pre-built with `MemWorld::put` (no engine calls), then **one** `on_placed` on a redstone block in the middle. Half the patch never charges:
```
engine:   10 11 12 13 12 11 10       expected: 10 11 12 13 12 11 10
          11 12 13 14 13 12 11                 11 12 13 14 13 12 11
          12 13 14 15 14 13 12                 12 13 14 15 14 13 12
          13 14 15 RB 15 14 13                 13 14 15 .. 15 14 13
          12 13 14 15 14 13 12                 12 13 14 15 14 13 12
          11 12 13  0  0  0  0                 11 12 13 14 13 12 11
           0  0  0  0  0  0  0                 10 11 12 13 12 11 10
```
Radius 1–2 are correct, radius 3+ (48+ wire cells) silently truncates. Existing tests never catch it because they place each block with its own `on_placed`, and each engine call resets `count`; a single cascade (lever toggle, redstone block placed, scheduled tick) that exceeds ~512 `add_and_run` calls leaves wires at power 0. Fix: `MAX_CHAINED = 1_000_000` (vanilla) or make it host-configurable; correct the doc comment.

**D2 — MINOR. `NeighborSource.pos` is the notified cell, not the changed cell, for the except-facing and `notify_around` paths.** `behaviour.rs:351` (`update_neighbors_at_except_from_facing` sets `source_pos` = the centre), `behaviour.rs:521` (`NeighborSource { pos: *source_pos, .. }`), used by `repeater.rs:257` with the output cell and by `Context::notify_around` with each satellite cell. Current behaviours ignore `_from`, so it is latent; but the observer/piston modules this trait is meant to enable must read `from.pos`/`orientation` to know which face changed, and `orientation` is always `None` (`repeater.rs:257, behaviour.rs` Multi dispatch). Adding an observer would expose it.

**D3 — MINOR. Torch support uses the FULL face test where vanilla uses CENTER.** `torch.rs:100` `standing_support` / `torch.rs:112` `wall_support` call `shapes::face_full`; vanilla `BaseTorchBlock.canSurvive` uses `Block.canSupportCenter(..., SupportType.CENTER)` (`Block.java:340`). Probe: a standing torch placed on `oak_fence` or `hopper` and then given a `DOWN` shape update is removed by the engine, although the center column is supported. (This is the documented "support shape approximated by collision shape" gap, but the torch case is a specifically *weaker* vanilla predicate than the engine's, so it over-destroys.)

**D4 — MINOR. `on_placed`/`on_removed` start the shape pass with limit 512, not 511.** `engine.rs:120/135` call `ctx.update_shapes(..., SHAPE_UPDATE_LIMIT)` directly; vanilla `Level.setBlock` uses `j-1` (`Level.java:243`). Off-by-one in the recursion budget only.

**D5 — MINOR (design). "Add a module" still requires editing the core registry.** `behaviour.rs::build` must gain a `register(...)` line, and `TestObserver`/`Spy` are `#[cfg(test)]`-only so an external crate cannot register a behaviour at all. Dead surface left by R11: `BlockBehaviour::is_repeater` (`blocks/mod.rs:72`) is no longer read, and `Context::update_neighbors_except` (`behaviour.rs:368`) has no callers.

## Verdict

**NOT STABLE** — blocker: the 512-entry chain cap (D1) silently truncates any single neighbour cascade larger than a 5×5 wire patch, producing wrong power levels where vanilla resolves; D2/D3/D4 must be settled before observers and pistons are added.
