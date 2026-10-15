# CMD-29a — Core API: region index build

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`, `oc/P1`
Parent/umbrella: `docs/tasks/CMD-29-region-index.md`

## Goal
The fast spatial index behind region/claim protection: a **per-chunk bucket** (or R-tree) mapping a
chunk to the region handles covering it, built from an immutable snapshot and swapped in atomically.

## API sketch
```rust
pub struct RegionIndex { /* per-chunk buckets -> Arc<[RegionHandle]> */ }
pub struct Region { id, world, min: BlockPos, max: BlockPos, priority: i32, flags, members, owners }
pub fn overlapping(&self, pos: BlockPos) -> impl Iterator<Item = &RegionHandle>;
```
- Query cost O(log n + k), no allocation, no lock.
- Build produces a new snapshot; updates (add/remove/move a region) swap a new snapshot at a safe
  point (mirror CMD-02's permission snapshots). A query never sees a half-built index.
- Region creation from a bounding box; a region on a chunk border is indexed in every chunk it
  touches.

## Files
- new `crates/aether-server/src/region.rs` (index half) and `aether-api` types if plugins need them
  across the plugin boundary.

## Edge cases to test explicitly
- many overlapping regions: all are returned; the caller (CMD-29b) decides by priority.
- a region exactly on a chunk border is found from both adjacent chunks.
- moving/creating a region takes effect at the next safe point, never mid-operation.
- query cost stays flat as region count grows (bench 10 / 1k / 100k regions).
- an unloaded chunk returns an empty result (no panic).

## Done criteria
- Index + tests + bench; the design choice written in module docs.
- fmt / clippy `-D warnings` / `cargo-locked test`. No plugin code.
