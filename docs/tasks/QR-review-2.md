Verified against the Java at `/tmp/mc`, probes under `/tmp/opencode/probe`. All 38 tests pass. No repo files edited.

## Status of round-1 findings

| # | Finding | Status |
|---|---------|--------|
| 1 | Ticks keyed by position, no `is(block)` guard | **FIXED** — `view.rs:175` keys `(block,pos)`, `engine.rs:188` guards `block_id_of(...) != Some(block)`; test `a_stale_tick_is_dropped_when_the_block_changed` |
| 2 | `is_conductor` used as face-sturdy; hopper missing | **FIXED** — `shapes::face_full` / `is_hopper`, `wire.rs:160`; `face_full` reads per-state collision boxes, matching `SupportType.FULL.isSupporting→getBlockSupportShape→getCollisionShape` |
| 3 | Comparator treated like a repeater in `should_connect_to` | **FIXED** — `wire.rs:173` only `is_repeater()`; comparator registered as signal source |
| 4 | Name-matched dispatch, no per-block trait | **PARTIAL** — trait + state-indexed registry exist and the engine is name-free, but bespoke wire rules still live in `wire.rs` (see N2) |
| 5 | `set_block` raw, no flags | **PARTIAL** — flags implemented (`behaviour.rs:248`), but the removal hook runs before the write and the default over-notifies (N3) |
| 6 | Breadth-first, not `CollectingNeighborUpdater` | **FIXED** — faithful LIFO port; `UPDATE_ORDER`/`UPDATE_SHAPE_ORDER` and chain limit 512 match `NeighborUpdater.java` / `Block.java` |
| 7 | Repeater placed on powered input waits `2*delay` | **FIXED** — `set_placed_by` schedules delay 1 (`repeater.rs:69`); test passes |
| 8 | Lone wire is a dot | **FIXED** — `placement_update` uses `crossState`; test passes |
| 9 | Torch toggle log unpruned, on the view | **FIXED for pruning** — moved to the engine and pruned; but the relocation is itself buggy (N1) |
| 10 | Repeater delay change notifies nobody | **FIXED** — `use_block` writes with `PLACE`; test passes |

## New defects (R6–R9), strongest first

**N1 — MAJOR. The torch burnout log is engine-global, not per-world.** `engine.rs:42` (`toggles: RefCell<HashMap<BlockPos, _>>`), `engine.rs:53` `record_toggle`, `engine.rs:68` `toggle_count`.
Vanilla keys it per level: `RedstoneTorchBlock.RECENT_TOGGLES` is `WeakHashMap<BlockGetter, List<Toggle>>` (`RedstoneTorchBlock.java:27`, `isToggledTooFrequently` :126). This contradicts `view.rs:147` ("the same `Redstone` instance can drive any number of worlds") and `lib.rs:12`.
Circuit (probe, reproduced): one `Redstone` driving two `MemWorld`s; 7 off-toggles of a torch at `(1,0,0)` in world A, then a single off-toggle of a fresh torch at `(1,0,0)` in world B → world B's torch `lit=false` (vanilla `true`). Same sequence with a per-world engine relights. Any server with two dimensions on one engine corrupts torch burnout across dimensions.

**N2 — MAJOR (design; defeats the "add-a-module" claim). Observer/piston wire connections need a `wire.rs` edit.** `wire.rs:168-176`: `should_connect_to` = `wire || repeater(facing) || is_signal_source(state) && dir.is_some()`. The trait (`blocks/mod.rs:38`) has no `connects_to_wire(dir)` hook.
Vanilla `RedStoneWireBlock.shouldConnectTo` (`:378`) special-cases `Blocks.OBSERVER`: `direction == blockState.getValue(ObserverBlock.FACING)` **regardless of `powered`**, before the `isSignalSource` branch.
Circuit (probe): wire at `(0,0,0)` with wires N/S (to stop the forced-side logic masking it) and `observer[facing=east,powered=false]` at `(1,0,0)` → engine `wire.east=none`; vanilla `wire.east=side`. An Observer module returning `is_signal_source = powered` still cannot reproduce the unpowered case; the core `wire.rs` must be edited. The same applies to any future block with a bespoke wire rule (pistons with a facing output, levers already covered only by luck).

**N3 — MINOR. Removal hook runs before the write, and the default over-notifies.** `behaviour.rs:258` calls `on_remove` before `behaviour.rs:261` `view.set_block`; default `blocks/mod.rs:134` does `ctx.update_neighbors_at(pos)`.
Vanilla writes first (`LevelChunk.setBlockState` → `affectNeighborsAfterRemoval`, `LevelChunk.java:319`) and the default `BlockBehaviour.affectNeighborsAfterRemoval` is empty (`BlockBehaviour.java:172`); the sole fan-out is `Level.setBlock`'s `updateNeighborsAt` (`Level.java:245`). So for a plain block / redstone block / lamp removal the engine fans out twice, the first time reading the not-yet-removed block (probe `[1]` shows the intermediate write ordering around a `redstone_block`→air swap). No circuit with a wrong *final* state was found (monotone power converges), but observers/pistons that read `from`/neighbour state on removal will diverge.

**N4 — MINOR. Shape updates never recurse and `updateIndirectNeighbourShapes` is never called.** `behaviour.rs:530` `update_neighbour_shapes`, `behaviour.rs:544` `execute_shape_update`, `:556` forces `UPDATE_KNOWN_SHAPE` into the nested write.
Vanilla `Level.setBlock` recurses shape updates with the limit `j-1` (`Level.java:243`) and calls `blockState2/blockState.updateIndirectNeighbourShapes`, which `RedStoneWireBlock.updateIndirectNeighbourShapes` overrides to notify the diagonal wires of an up-and-over. The engine cuts recursion at one level and implements neither indirect call; correct wiring depends entirely on `notify_around`/`update_neighbors_of_neighboring_wires`.

**N5 — MINOR. Repeater `update_shape` skips the vertical lock recompute.** `repeater.rs:99`: guard is `dir.is_horizontal() && dir != facing && dir != facing.opposite()`.
Vanilla `RepeaterBlock.updateShape` recomputes `LOCKED` for **any** `direction.getAxis() != FACING.getAxis()` — which includes `UP`/`DOWN`. Only visible with a side input present, so low impact.

**N6 — MINOR. `update_neighbors_except` uses the wrong source state.** `behaviour.rs:353`: source = `self.get_block(pos)` of the notified cell, not the block that caused the update. Vanilla `DiodeBlock.updateNeighborsInFront` passes `this` (the repeater) as the source to `Level.updateNeighborsAtExceptFromFacing` (`DiodeBlock.java:154`). Masked today because every `neighbor_changed` ignores `_from`, but observers/pistons read the source direction, so this must be fixed before those modules.

**N7 — MINOR. `UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE` (128) is missing** from `SetBlockFlags` (`view.rs:66`), so `execute_shape_update` cannot reproduce `NeighborUpdater.executeShapeUpdate`'s `(i & 128) == 0 || !blockState2.is(REDSTONE_WIRE)` guard. Related: `lib.rs:12` and `engine.rs:3` still claim "`Redstone` itself holds nothing" while it now owns the burnout log.

**N8 — STANDING (acknowledged). Support destruction is not implemented.** `wire.rs:89` / `repeater.rs:99` `update_shape` ignore `dir` and never return air; probe `[3]`/`[5]` show a wire and a repeater floating after their support is removed (vanilla `RedStoneWireBlock.updateShape(DOWN)`/`RepeaterBlock.updateShape(DOWN)` return `AIR`). Listed in `KNOWN_ISSUES`, but it is a genuine behaviour difference the brief asked about.

## Verdict

**NOT STABLE** — blockers: the engine-global torch burnout log (N1) breaks the documented multi-world contract and corrupts cross-dimension circuits, and the wire-connection special cases living outside `BlockBehaviour` (N2) mean the first new component (observer) still requires editing the core `wire.rs`, so "add a module and register it" is not yet true.
