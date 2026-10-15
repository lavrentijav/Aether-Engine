# CMD-31 — Core API: teleport with safe-spot search

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`, `oc/P1`

## Goal
A core teleport service plugins reuse: cross-world/dimension teleport, exact teleport, and a
**safe-spot search** for random teleport (`/rtp`), `/spawn`, and EssX-style `/top`/`/back`. Vanilla
has `TeleportCommand` and a `findSafePosition`-style search; the search belongs in core because it
touches collision/light and runs often.

## API sketch
```rust
pub enum TeleportTarget { Exact(Vec3, yaw, pitch), Safe(BlockPos) }
pub fn teleport(world, entity, target: TeleportTarget, cause: TeleportCause) -> Result<Vec3>;
pub fn find_safe_spot(world, around: BlockPos, radius: u32, config) -> Option<BlockPos>;
pub fn random_safe_spot(world, center, min_radius, max_radius, rng) -> Option<BlockPos>;
```
- `teleport` moves the entity, sends the right packet(s) (relative flags, passengers, mount),
  unloads/loads chunks as needed, fires the plugin `after-teleport` event, and preserves the
  existing `game/commands.rs::teleport` behaviour (pos + `Teleport` event + move broadcast).
- `find_safe_spot` uses `aether-world::registry::shapes` collision: two non-colliding cells with a
  solid floor, no lava/fire/cactus/campfire, within the world height; a preference for the
  dimension's natural surface when available. Deterministic with a supplied seed.
- `TeleportCause` distinguishes plugin/command/portal so events and checks can differ (region
  `entry` flag).
- Cross-dimension: if the entity is a player, the codec sends the dimension change/respawn dance;
  non-players are moved/despawned as vanilla does.

## Files
- new `crates/aether-server/src/teleport.rs`.
- reuse `game/commands.rs::teleport`; `aether-physics` collision; region check (CMD-29) for the
  `entry` flag; codec packets for dimension change.

## Edge cases to test explicitly
- safe-spot search in a sealed cave returns the cave floor, never inside lava; on a full column it
  picks the surface.
- random spot fails after the radius budget → returns `None` (no infinite loop).
- teleport through a portal vs command fires different causes; region `entry` deny cancels.
- teleporting a player with a vehicle carries or dismounts per vanilla; passengers move together.
- teleport into unloaded chunks loads them, bounded per tick.
- exact teleport with `~` (from CMD-06) resolves before the call.
- the existing `/tp`, `/spawn` behaviour is unchanged after the refactor.

## Done criteria
- API + tests for the collision/height/lava cases + a `/rtp`-sized bench.
- fmt / clippy `-D warnings` / `cargo-locked test`.
