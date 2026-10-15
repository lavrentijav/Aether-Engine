# CMD-30 — Native: bulk edit

Status: 📋 task brief (CMD0) · size: L · tier: native plugin (N)
Needs: `oc/CMD-00N`, `oc/CMD-01`, `oc/P1`

## Tier (CMD0 review, 3-tier model)
Reclassified from core to a **native plugin** (in-process Rust, `plugins/worldedit`). The core keeps
only the **minimal hook**: the low-level block-write and per-section chunk-resend primitives in the
world/session (already core), plus the journal API. The **batched write transaction, one-revision
grouping, undo and the resend/light batching** live in the native plugin (CMD-30a/b plus CMD-36).
The core does not depend on the plugin: with it disabled, ordinary placement still works.

## Split (CMD0 review)
This L task is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-30a | `docs/tasks/CMD-30a-bulk-edit-api.md` | `BulkEdit` API, pending-read semantics, one journal revision per commit, undo | M |
| CMD-30b | `docs/tasks/CMD-30b-bulk-edit-publish.md` | chunk-resend batching, neighbour/light update, per-tick budget | M |

## Goal
The mechanism behind WorldEdit-style operations: **batched multi-block writes** with one journal
operation, batched chunk resends, neighbour/light updates, and undo through the journal. Millions of
blocks are a performance path, so this is core (`docs/design/commands.md` §3); the selection,
clipboard, `//commands`, brushes and schematics are a plugin (CMD-36).

## API sketch
```rust
pub struct BulkEdit { /* undo id, counters */ }
impl BulkEdit {
    pub fn new(actor: JournalActor) -> Self;
    pub fn set(&mut self, world, pos, block) -> Result<()>;
    pub fn fill(&mut self, world, min, max, block) -> Result<()>;      // inclusive
    pub fn get(&self, world, pos) -> BlockStateId;
    pub fn commit(self, world) -> BulkResult { changed, chunks_resent, undo_id }
    pub fn abort(self);
}
```
- Reads see the pre-edit world plus this transaction's pending writes (so a `//copy` after `//set`
  is consistent).
- Writes are grouped: **one journal revision** per commit (not one per block), with the actor.
- Chunk resends are batched per chunk (one packet per section that changed), not per block.
- Neighbour updates and lighting run once per affected region after commit, using the existing
  placement/light passes; no per-block full recompute.
- Undo: a commit records enough to invert it via the journal (`/rollback`-style), or keeps an
  in-memory inverse for `//undo` (CMD-36). State which and keep it bounded (configurable history
  size / spill to the journal).
- The whole edit is applied at **block granularity** but published atomically: viewers see the
  before or the after, not a half-filled wall (batch the resend after all writes).

## Files
- new `crates/aether-server/src/bulk.rs`.
- `aether-world` chunk setter + journal grouping; existing chunk-resend batching in `session.rs`/
  `outbox.rs`; placement neighbour update; light update (S4 if merged).

## Edge cases to test explicitly
- a 1M-block fill completes within a per-tick budget (spread over ticks if needed) and does not
  stall the tick; measure and document the budget.
- reads inside the transaction see pending writes; reads outside a not-yet-committed region see the
  old world.
- abort leaves the world unchanged and writes no journal.
- commit of zero changes writes nothing.
- undo restores exactly the previous states (property test on random edits).
- editing across chunk borders resends each touched section exactly once.
- concurrent bulk edits from two actors are ordered and journaled correctly (no torn chunk).
- a block-change event from bulk edit fires the plugin `after-block-break/place` hooks once per
  changed block (or batched — decide; plugins expect per-block).

## Done criteria
- API + tests + a bench (blocks/second, tick budget) + a `//set`-sized integration test.
- fmt / clippy `-D warnings` / `cargo-locked test`.
