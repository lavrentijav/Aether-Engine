# CMD-29b — Core API: protection check + hook wiring

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`, `oc/P1`, `oc/CMD-29a`
Parent/umbrella: `docs/tasks/CMD-29-region-index.md`

## Goal
The hot-path protection check and its wiring into the existing event paths, plus the bypass
permission. The region *model and commands* are the plugin (CMD-34/35); this is the mechanism.

## API sketch
```rust
pub enum Action { Break, Place, Interact, UseItem, Pvp, Explosion, … }
pub enum Verdict { Allow, Deny, Defer }
pub fn check(&self, world, pos, action, actor: &Actor) -> Verdict;
```
- `Allow` proceeds; `Deny` cancels with a configurable message; `Defer` lets the next region decide
  (priority order; ties by id, deterministic).
- Hook points: block break/place (`game/provenance`/`placement.rs`), interaction
  (`game/interact.rs`), explosion damage, and the P1 `before-*` events.
- Bypass permission `aether.region.bypass` skips the check.

## Files
- `crates/aether-server/src/region.rs` (check half); hook wiring in `placement.rs`,
  `game/interact.rs`, `game/combat.rs`, explosion handling.

## Edge cases to test explicitly
- overlapping regions: priority decides; equal priority by id; `Defer` chains.
- explosion spanning protected and unprotected regions denies only protected blocks.
- bypass allows an op; a non-op is denied.
- check cost constant as region count grows; no lock-order inversion with entity/inventory locks
  (`docs/storage-v2/lock-order.md`).
- an unloaded/chunked-out position returns a safe verdict.

## Done criteria
- Check + hook wiring + tests + a bench; fmt / clippy `-D warnings` / `cargo-locked test`.
