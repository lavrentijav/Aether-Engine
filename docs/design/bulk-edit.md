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
kept exactly as `BlockSet` keeps it, so the revision carries its own inverse:
undo is reading it back and swapping `from`/`to`.

The journal's **column index** records every touched position, so a column
materialized after a restart replays the whole revision wherever any part of it
lands (`World::ensure_column`), and `/rollback` by actor or area expands the
revision into per-block restorations (`Journal::plan_rollback_where`). An area
filter selects a revision *whole* if any of its changes falls in the area — a
revision is atomic, so its rollback touches all of it.

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
`BulkEdit::with_limit`) on the number of distinct pending positions. A fill
that would exceed it is refused **before** any pending write changes, so a
mistyped selection cannot exhaust memory before CMD-30b's tick budget gets a
chance to spread the work. Positions outside the addressable vertical band
(`MIN_Y..=MAX_Y`, i.e. section `-128..=127`) are refused rather than silently
read back as air.

## Format version

`BlockGroup` is format **v6**. Older blobs decode unchanged; a tag-10 body under
a v1–v5 header is refused (`UnknownTag`), never guessed at, because a misread
revision would undo the wrong blocks.
