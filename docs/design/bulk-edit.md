# Bulk edit — batched writes, one journal revision, bounded undo (CMD-30a)

The mechanism behind WorldEdit-style operations: a `//set`, a fill, a
schematic paste or a brush touches thousands (or millions) of blocks as **one
act**. This is the core half — the write and the history. The publish half
(chunk resend, neighbour/light updates, per-tick budget) is CMD-30b.

- API: `aether_server::bulk::{BulkEdit, BulkResult, BulkHistory, BulkError}`
  (`crates/aether-server/src/bulk.rs`).
- Primitive: `aether_world::journal::EventBody::BlockGroup` — a journal
  **revision** — and `Journal::append_block_group`; the batch setter
  `SubChunk::set_many`; the world batch `World::apply_block_changes` /
  `World::commit_block_group` (`crates/aether-api/src/lib.rs`).

## Why one revision

The journal is the source of truth and its unit is a change someone made. A
`//set` of 100,000 blocks is one thing a player did, not 100,000 acts.
Recording it as 100,000 `EventBody::BlockSet` events would misreport the
history, assign 100,000 sequence numbers, and make "undo the last thing I did"
a question about a count. So a commit is one event:

```rust
EventBody::BlockGroup { changes: Vec<BlockChange> }   // format v6, tag 10
BlockChange { x, y, z, from, to }                     // both sides of the change
```

One event = one `seq` = one actor = one timestamp = one undo unit. `from` is
kept exactly as `BlockSet` keeps it, so the revision carries its own inverse.

The changes are stored sorted by column. The journal's **column index** gets one
entry per **distinct column** the revision touches (not one per block, which
would be a million entries for a million-block fill), and `World::ensure_column`
binary-searches the sorted change list for its own column, so loading a column of
a large revision costs `O(log N + its changes)` rather than a scan of the whole
revision. `/rollback` by actor or area expands the revision into per-block
restorations (`Journal::plan_rollback_where`). An area or `block:`/`action:`
filter selects a revision *whole* if **any** of its changes matches — a revision
is atomic, so its rollback touches all of it.

## Read semantics

`BulkEdit::get` returns a pending write if the position has one, otherwise the
world's pre-edit block. Nothing reaches the world until `commit`, so:

- a `//copy` after a `//set` is self-consistent (it reads the pending writes);
- **anyone holding only the `World` sees the old world** until commit;
- `abort` leaves no trace — no block touched, no journal entry.

Consequently the whole edit is published at once (at block granularity, after
commit); viewers never see a half-filled wall. The resend/lighting that follows
that atomic write is CMD-30b.

## Undo, and the spill policy

Undo is journal-backed. `BulkHistory` keeps the newest `capacity` revisions'
inverse changes **hot** in memory for undo without a journal read; when it
overflows, the oldest records are **spilled from memory — not lost**. The
revision is still in the journal, and `BulkHistory::undo(undo_id)` falls back to
reading it by sequence number, so a spilled revision undoes identically to a hot
one. `capacity == 0` keeps nothing hot and undoes everything from the journal.
The journal is append-only and its per-column checkpoints do not delete events,
so an undo id stays valid for the life of the history.

Undo applies the inverse as a **new revision**, attributed to the original
author, and records it in turn — so an undo is auditable (it appears in the
history like any other edit) and can itself be undone (a redo). The `from` it
records is read from the world at undo time, so the event is accurate even if
someone edited the same block since; a position already holding its old state
is dropped rather than journalled as a no-op. Nothing is ever erased.

## Limits

`BulkEdit` enforces a block limit (`DEFAULT_MAX_BLOCKS`, raisable with
`BulkEdit::with_limit`) on the number of distinct pending positions. A fill's
box volume is computed arithmetically first and refused **before** any position
is visited, so a mistyped selection (`//fill ~-2e9 … ~2e9 …`) is rejected at
once instead of iterating billions of positions and hanging the tick thread, and
before it can exhaust memory. Positions outside the addressable vertical band
(`MIN_Y..=MAX_Y`, i.e. section `-128..=127`) and block ids the registry does not
know are refused rather than stored unnameable.

## Atomicity and concurrency

A commit is **all-or-nothing**: `World::commit_block_group` appends the
revision to the journal *first* and only then applies the blocks, so a journal
failure leaves the world untouched and propagates `BulkError::Journal` — never a
block change with no event to undo it. `BulkEdit::commit`, `BulkHistory::commit`
and `BulkHistory::undo` all return `Result` for this reason.

A commit is, however, a **read-modify-write without a lock across plan and
apply**: `plan` reads each `from` from the live world and only then applies. Two
concurrent transactions over the *same* block can therefore both read `from = A`
and journal `A→B` and `A→C` while the world ends at `C` (a lost update). This is
the same race as `World::set_block_by`, not a new one, and the journal stays
consistent (two well-formed revisions, in sequence order); CMD-30b, which owns
the command wiring and tick budget, is where serialisation per region belongs.

## Format version

`BlockGroup` is format **v6**. Older blobs decode unchanged; a tag-10 body under
a v1–v5 header is refused (`UnknownTag`), never guessed at, because a misread
revision would undo the wrong blocks.
