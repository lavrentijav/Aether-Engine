IDLE ses_001a13a1255d0b2d743825ea8b
All tests pass (`CARGO_TARGET_DIR=/srv/dev/build/aether-target cargo test -p aether-redstone`). I read the engine against the Java sources and validated the divergences with throwaway probes under `/tmp/opencode/probe`. No repo files were edited.

# Findings

## 1. MAJOR — Scheduled ticks are keyed by position only; `on_scheduled_tick` runs whatever block is there
`crates/aether-redstone/src/view.rs:204` (`MemWorld::schedule_block_tick` — `if self.pending.contains_key(&pos) { return; }`), `view.rs:214` (`will_tick_this_tick`), `engine.rs:906` (`on_scheduled_tick` re-dispatches by the *current* kind).
Vanilla keys ticks by `(Block, BlockPos)` (`net.minecraft.world.ticks.ScheduledTick.UNIQUE_TICK_HASH`, `LevelChunkTicks.schedule`), and `ServerLevel.tickBlock` (ServerLevel.java:801) guards with `if (blockState.is(block))`. So a tick scheduled for block A is dropped when the cell is now block B, and B's own tick is allowed to coexist.
Circuit (probe `[A]`): power a lamp at P, cut power (lamp schedules its 4-tick off tick), then before it fires replace P with a stone button and press it. Engine: the button's 20-tick release is refused (cell already pending) and the stale lamp tick is executed as `button_tick`; the button releases at t+4. Vanilla: the lamp tick is skipped (`is(lamp)` false), the button tick is scheduled, release at t+20.

## 2. MAJOR — Wire shape/connection uses `is_conductor` where vanilla uses `isFaceSturdy`, and `canSurviveOn` drops the hopper case
`engine.rs:379` (`face_sturdy` = `is_conductor`), `engine.rs:383` (`can_survive_on` = `face_sturdy`), used at `engine.rs:402` (`wire_connecting_side`).
Vanilla: `RedStoneWireBlock.canSurviveOn` = `isFaceSturdy(...,UP) || blockState.is(HOPPER)`, and the up/side decision is `blockState.isFaceSturdy(level, pos2, direction.getOpposite())` (`RedStoneWireBlock.getConnectingSide`). `isRedstoneConductor` and `isFaceSturdy` are different predicates per state.
Circuit (probe): lever → wire at (0,0,0), bottom `oak_slab` at (1,0,0), wire at (1,1,0). Engine sets `source.east = none`; vanilla sets `side` (slab top is face-sturdy, west face isn't so `SIDE`). Same for `hopper` (explicitly included by vanilla) and stairs. This also mis-scores a `type=double` slab / any full-collision state because `aether-world`'s `solid` flag is per-block, not per-state (`crates/aether-world/src/registry/blocks.rs:5238`).

## 3. MAJOR — `should_connect_to` special-cases comparators like repeaters
`engine.rs:390` (`Kind::Repeater | Kind::Comparator => dir == Some(f) || Some(f.opposite()) == dir`).
Vanilla `RedStoneWireBlock.shouldConnectTo` special-cases **only** `Blocks.REPEATER`; a comparator falls through to `isSignalSource() && direction != null`, so wire connects to a comparator on any horizontal side (`ComparatorBlock extends DiodeBlock`, `isSignalSource()` true).
Circuit (probe `[1]`): wire at (0,0,0), `comparator[facing=north]` at (1,0,0). Engine `wire.east = none`; vanilla `wire.east = side`. This changes the wire's stored shape and its `getSignal` toward that side.

## 4. MAJOR (design) — The engine is a hardcoded name match with no per-block behaviour dispatch
`engine.rs:61` (`kind_of` on `&str`), `engine.rs:300` (`block_signal`), `engine.rs:241` (`direct_signal`), `engine.rs:827` (`dispatch`).
Comparators (analog `getOutputSignal`, alternate signal), observers (pulse, two delayed ticks, `isSignalSource` only while pulsing), pressure plates, and pistons/QC are not addable through `RedstoneView`/the registry; every one requires editing these match arms and the `Kind` enum. Vanilla dispatches `BlockState.getSignal`/`getDirectSignal`/`isSignalSource`/`neighborChanged`/`tick` to the block. Adding any of the "next stages" means rewriting `engine.rs`, not extending data.

## 5. MAJOR (design) — `RedstoneView::set_block` is raw with no update flags, no except-facing update and no orientation
`view.rs:68` (`set_block`), `view.rs:60` (`RedstoneView`).
Vanilla's single mutation entry point is `Level.setBlock(pos, state, flags)` (Level.java:211) where the flags select `updateNeighborsAt`, shape updates, etc., plus `NeighborUpdater.updateNeighborsAtExceptFromFacing` and `Orientation`/`ExperimentalRedstoneUtils` (1.21.11 oriented redstone). Wiring this crate into the server, where `Level.setBlock` *always* runs `onPlace` + `updateNeighbourShapes`, will either double-notify (engine also fans out) or diverge, and pistons/observers cannot express face-excluded or oriented updates. The engine also discards the `from` block in `on_neighbour_changed` (`engine.rs:893`), which those blocks need.

## 6. MAJOR/MINOR — Update order is breadth-first, not vanilla's depth-first `CollectingNeighborUpdater`
`engine.rs:816` (`drain` pops a `VecDeque` FIFO); `engine.rs:747` (`update_neighbors_at` enqueues the whole fan-out). Admitted in `lib.rs` docs.
Vanilla processes a LIFO `ArrayDeque` stack and re-enters generated updates before continuing a `MultiNeighborUpdate` (`CollectingNeighborUpdater.runUpdates`/`MultiNeighborUpdate.runNext`). R3 explicitly requires the 1.21.11 default algorithm. For pure wire the fixed point is order-independent, so no divergence has been demonstrated in this stage, but the mandated algorithm is not implemented and the claimed equivalence does not hold once stateful blocks interact.

## 7. MINOR — A repeater placed onto an already-powered input takes `2*delay` ticks
`engine.rs:861` (`on_placed` → `Update(self)` → `diode_check_tick` at `engine.rs:628` schedules `2*delay`).
Vanilla `DiodeBlock.setPlacedBy` does `if (shouldTurnOn(...)) level.scheduleTick(blockPos, this, 1)` — one tick, regardless of DELAY.
Circuit (probe `[2]`): powered lever at (0,0,0), then place `repeater[facing=west,delay=4]` at (1,0,0). Engine turns it on at t=8; vanilla at t=1.

## 8. MINOR — A lone placed wire becomes a dot, not a cross
`engine.rs:454` (`wire_connection_state`'s `was_dot` early return) via `on_placed` (`engine.rs:861`).
Vanilla `RedStoneWireBlock.getStateForPlacement` passes `this.crossState` (all `SIDE`) to `getConnectionState`, and `getConnectionState` rebuilds from `defaultBlockState`, so an isolated dust is a **cross** (`all side`).
Circuit (probe `[3]`): `on_placed` a lone `redstone_wire`; engine `n/e/s/w = none`; vanilla all `side`. This also inverts the `useWithoutItem` toggle parity (`engine.rs` `wire_use`: engine dot→cross, vanilla cross→dot).

## 9. MINOR — Torch burnout state lives on the view and is never pruned
`view.rs:222` (`MemWorld::push_toggle`), `view.rs:226` (`toggles_since`).
The per-position `Vec<u64>` in `MemWorld` grows without bound (a clock torch toggles forever) and survives block replacement at the same cell; `toggles_since` only filters on read. Vanilla keeps `RECENT_TOGGLES` per level and removes entries older than 60 ticks at the start of every torch tick (`RedstoneTorchBlock.tick`). Putting burnout bookkeeping on `RedstoneView` is also a layering leak (the world interface must implement one block's timer).

## 10. MINOR — Changing a repeater's delay notifies nobody
`engine.rs:691` (`repeater_use` only calls `view.set_block`).
Vanilla `RepeaterBlock.useWithoutItem` does `level.setBlock(blockPos, blockState.cycle(DELAY), 3)` (flag 3 = `UPDATE_NEIGHBORS|UPDATE_CLIENTS`), which runs `onPlace` → `updateNeighborsInFront`. The engine emits no neighbour update / front update on delay change.

---
No existing test pins any of the divergent cases above; the current R5 suite matches the vanilla rules it names. Findings 1–3 are the ones I would block on; 4–5 are the ones that determine whether the "later stages extend without rewriting" goal holds.

[exited with code 0]
