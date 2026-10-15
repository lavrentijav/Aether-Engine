# CMD-29 — Core API: region index + protection check

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-01`, `oc/P1` (the region *model* is a plugin; this is the mechanism it uses)

## Split (CMD0 review)
This L task is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-29a | `docs/tasks/CMD-29a-region-index-build.md` | per-chunk region index, build/update, immutable snapshots | M |
| CMD-29b | `docs/tasks/CMD-29b-region-check-hooks.md` | protection check, hook wiring, bypass | M |

## Goal
The hot-path mechanism behind WorldGuard-style regions and GriefPrevention-style claims: a fast
**per-chunk region index** (R-tree/bucket) and a **protection check** consulted on block break/
place, entity interaction and item use. Per `docs/design/commands.md` the region model, flags and
`/rg` commands are a plugin (CMD-34); claims are another plugin (CMD-35) on the same API. This task
is core because a region check runs for every block event.

## API sketch
```rust
pub struct RegionIndex { /* per-chunk buckets -> Arc<[RegionHandle]> */ }
pub struct Region { id, world, min: BlockPos, max: BlockPos, priority: i32, flags: FlagSet, members, owners }
pub enum Action { Break, Place, Interact, UseItem, Pvp, Explosion, ... }
pub fn check(&self, world, pos, action, actor: &Actor) -> Verdict; // Allow | Deny | Defer
```
- Query cost: O(log n + k) per event, no allocation, no lock. Build from an immutable snapshot;
  updates swap a new snapshot at a safe point (mirror CMD-02's permission snapshots).
- The protection hook is called from the **existing** event paths: block break/place
  (`game/provenance`/placement), interaction (`game/interact.rs`), explosion damage, and the
  plugin `before-*` events (P1). `Allow` proceeds; `Deny` cancels with a configurable message;
  `Defer` lets the next region/plugin decide (priority order).
- Flags are typed (`build`, `pvp`, `use`, `entry`, …) with a default per-region; plugin-registered
  flags (CMD-34) extend the set behind the API, not the core.
- Bypass: a permission node (`aether.region.bypass` / WG `worldguard.region.bypass`) skips checks.

## Files
- new `crates/aether-server/src/region.rs` (index + check) and `crates/aether-api` types if plugins
  need them across the C ABI/WIT boundary.
- hook points in `placement.rs`, `game/interact.rs`, `game/combat.rs`, explosion handling.
- event path wiring in P1's bus.

## Edge cases to test explicitly
- many overlapping regions: priority decides; equal priority by id (deterministic); `Defer` chains.
- a region exactly on a chunk border is found from both adjacent chunks.
- moving/creating a region takes effect at the next safe point, never mid-operation.
- check cost stays constant as region count grows (bench 10, 1k, 100k regions).
- explosion spanning a protected and unprotected region denies only protected blocks.
- bypass permission allows an op; a non-op is denied.
- an unloaded position/chunk returns a safe verdict (no panic, no false allow for protected areas).
- no lock-order inversion with the entity/inventory locks (`docs/storage-v2/lock-order.md`).

## Done criteria
- Index + check + hook wiring + tests + a bench; the design (R-tree vs per-chunk buckets) written in
  the module docs. No plugin code — the model/commands are CMD-34/CMD-35.
- fmt / clippy `-D warnings` / `cargo-locked test`.
