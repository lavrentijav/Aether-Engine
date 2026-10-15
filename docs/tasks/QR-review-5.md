Verified against the Java in `/tmp/mc`, probes under `/tmp/opencode/probe`. `CARGO_INCREMENTAL=0 cargo test --locked -p aether-redstone` (21 unit + 32 circuits + 8 comparator + 4 daylight + 4 observer + 5 plate + 5 target + 6 tripwire + 1 external + 1 doc) and clippy `--all-targets` are clean. No repo files edited.

## Round-4 findings (N1–N4)

| # | Finding | Status |
|---|---------|--------|
| N1 | Repeater used FULL, vanilla `DiodeBlock.canSurviveOn` is RIGID | **FIXED** — `diode::supported` → `shapes::face_rigid` (`diode.rs:63`); probe: repeater/comparator survive on a hopper |
| N2 | `face_center` ignored the axis; test pinned a hopper as CENTER | **FIXED** — `shapes::face_slice` uses `VoxelShape.getFaceShape` extreme slice; `can_support_center` = central 2/16 column; torch on a hopper now pops, fence still holds; tests corrected |
| N3 | Registry leaked a table per registration, race possible | **FIXED** — one `RwLock<Registry>` mutated in place (`behaviour.rs:667-707`); no `Box::leak` per register |
| N4 | Item drop delegated but undocumented | **FIXED (as a documentation item)** — `view.rs:215-226` states plainly that dropping is host-delegated |

## New findings (R20–R29), strongest first

**F1 — MAJOR. Removing a tripwire leaves the far hook stuck attached/powered (persistent wrong output).** `blocks/tripwire.rs:125` (`update_source` takes `_state` and ignores it), `:63` (`TripWire::on_remove` passes a powered-but-now-gone state into a function that never uses it), `:214-238` (`calculate_state` has no `i`-substitution and no `j > 1`).
Vanilla `TripWireBlock.affectNeighborsAfterRemoval` calls `updateSource(level, pos, state.setValue(POWERED, true))`, which calls `TripWireHookBlock.calculateState(..., i, source)`. `calculateState` substitutes the (now air) removed wire at distance `k == i` (`TripWireHookBlock.java:101-124`), so it walks past the hole, finds the far hook at `j`, and writes **both** hooks (`bl5`/`bl6`) plus a 10-tick recheck.
Circuit (probe): `tripwire_hook[facing=east]` at (0,0,0) – `tripwire` (1,0,0) – `tripwire_hook[facing=west]` at (2,0,0); step an entity onto the wire (both hooks powered 15, a lamp on hook B lights), then `set_block(wire, AIR, PLACE)`. Engine: hook A goes off, but hook B stays `powered=true`/`attached=true` and the lamp stays lit **forever** (checked at +100 ticks) — no tick is scheduled on B, and `update_source` searches only South/West from the removed cell, so B (east) is never re-derived. Vanilla keeps both on for 10 ticks then turns both off. Even with no entity, hook B is left `attached=true` permanently. This is a stuck signal, not a timing nit.

**F2 — MINOR. `will_tick_this_tick` is used where vanilla uses `getBlockTicks().hasScheduledTick`; the view has no `has_scheduled_tick`.** `blocks/target.rs:62` (`projectile_hit`) and `:39` (`on_place`); `blocks/observer.rs:113` (`start_signal`) and `:70` (`on_place`); `view.rs:182`.
Vanilla `TargetBlock.updateRedstoneOutput` guards with `!hasScheduledTick(pos, block)` — "any tick pending", not "due this tick". `willTickThisTick` is true only in the current tick.
Circuit (probe): target hit dead-centre at t=0 (power 15, reset due t+20); an edge hit at t=1. Engine sets power to 1; vanilla leaves 15 and leaves the timer alone. `ObserverBlock.startSignal`/`onPlace` use `hasScheduledTick` too; there the first-wins scheduler hides it, but the missing hook is the root cause for the target.

**F3 — MINOR. Tripwire `attached` omits vanilla's `j > 1` requirement.** `blocks/tripwire.rs:238` (`attached = line_intact && far.is_some()`); vanilla `bl5 &= j > 1` (`TripWireHookBlock.java:127`).
Circuit (probe): two adjacent hooks (no wire), `facing=east` at (0,0,0) and `facing=west` at (1,0,0) → engine `attached=true`; vanilla `false`.

**F4 — MINOR. Observer and target clear stale state with flag `2` where vanilla uses `18`.** `blocks/observer.rs:73`, `blocks/target.rs:42` (`SetBlockFlags::CLIENTS`). Vanilla `setBlock(..., 18)` = `UPDATE_CLIENTS|UPDATE_KNOWN_SHAPE`; the engine's `2` still runs the shape-update pass, an unnecessary extra cascade (and it differs from `Level.setBlock`'s shape suppression for these two "reset" writes).

## Core changes (R20–R23), judged

- **New `RedstoneView` host hooks** (`analog_output`, `sky_light`/`sun_angle`, `entity_count`; `view.rs:228-274`): **justified** — the crate has no inventory/light/entity model, and each is exactly what the vanilla block reads. `sun_angle` is documented in radians, matching `SUN_ANGLE * (PI/180)`.
- **`entity_inside` / `projectile_hit` on `BlockBehaviour` + `Redstone::on_entity_inside`/`on_projectile_hit`** (`blocks/mod.rs:196-211`, `engine.rs:180-193`): **justified and minimal**; the signatures carry what the behaviours need (face, local position, arrow flag).
- **In-crate registration lines** (`behaviour.rs:715-770`): **justified** — built-ins must be registered somewhere; the public `register` covers external crates, and the R20 rework made it race-free.
- **`diode.rs` refactor** (`blocks/diode.rs`): **justified** — repeater and comparator share `base_input_signal`, `control_input`, `alternate_signal`, `should_prioritize`, `supported`, `update_neighbors_in_front`; no behaviour change.

Minor spec-level divergences that are not worth blockers: the comparator computes its output live rather than from a `ComparatorBlockEntity` cache and schedules its 2-tick refresh on every neighbour change (`blocks/comparator.rs:66`), vs vanilla's cached signal + changed-only guard (`ComparatorBlock.checkTickOnNeighbor`); the daylight detector recomputes on placement instead of waiting for `gameTime % 20 == 0`; the item-frame analog input in `ComparatorBlock.getInputSignal` is not modelled (no entities). These are consistent with the no-block-entity design and settle to the same steady state.

## Verdict

**NOT STABLE** — blocker: **F1**, a tripwire whose wire is removed leaves the far hook permanently `attached`/`powered` (stuck 15 into any lamp/wire), because `update_source`/`calculate_state` omit vanilla's `i`-substitution and only re-derive one end of the line; **F2** (missing `has_scheduled_tick`, observable on the target) and **F3** should be fixed before stage 2 is called done. The other new components (comparator, observer, daylight detector, pressure plates, target basics) match vanilla on the rules inspected, and the four core changes are sound.
