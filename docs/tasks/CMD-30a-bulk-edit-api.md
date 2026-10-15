# CMD-30a — Core API: BulkEdit API + journal + undo

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`, `oc/P1`
Parent/umbrella: `docs/tasks/CMD-30-bulk-edit.md`

## Goal
The batched multi-block write API with one journal revision per commit and bounded undo. The
publish half (chunk resend, neighbour/light, tick budget) is CMD-30b.

## API sketch
```rust
pub struct BulkEdit { /* undo id, counters */ }
impl BulkEdit {
    pub fn new(actor: JournalActor) -> Self;
    pub fn set(&mut self, world, pos, block) -> Result<()>;
    pub fn fill(&mut self, world, min, max, block) -> Result<()>;
    pub fn get(&self, world, pos) -> BlockStateId;   // sees pending writes
    pub fn commit(self, world) -> BulkResult;         // { changed, undo_id }
    pub fn abort(self);
}
```
- Reads see the pre-edit world plus this transaction's pending writes (a `//copy` after `//set` is
  consistent).
- One **journal revision** per commit, not one per block, with the actor.
- Undo records enough to invert the commit (journal-backed, bounded history; spill policy in docs).

## Files
- new `crates/aether-server/src/bulk.rs`.
- `aether-world` chunk setter + journal grouping.

## Edge cases to test explicitly
- reads inside the transaction see pending writes; reads outside an uncommitted region see the old
  world.
- `abort` leaves the world unchanged and writes no journal.
- commit of zero changes writes nothing.
- undo restores exactly the previous states (property test on random edits).
- two concurrent bulk edits from two actors are ordered and journaled correctly.

## Done criteria
- API + tests + a bench (blocks/second); fmt / clippy `-D warnings` / `cargo-locked test`.
