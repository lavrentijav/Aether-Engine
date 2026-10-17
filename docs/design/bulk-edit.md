# Bulk edit — batched writes, one journal revision, bounded undo, budgeted publish

The mechanism behind WorldEdit-style operations: a `//set`, a fill, a
schematic paste or a brush touches thousands (or millions) of blocks as **one
act**. CMD-30a is the write and the history; CMD-30b is the publish half
(chunk resend, neighbour updates, per-tick budget).

- API: `aether_server::bulk::{BulkEdit, BulkResult, BulkHistory, BulkError,
  BulkPublisher, PublishBudget, PublishSink, BulkCommit}`
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

The world write is atomic per commit: every changed block is applied before
publication starts, so a chunk a viewer receives is wholly before or wholly
after the edit, never a half-filled wall. The re-sends that follow are spread
over ticks (below), but each one reads a world in which the whole edit has
already landed.

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

CMD-30a documented that `plan` reads each `from` from the live world and only
then applies, so two concurrent transactions over the *same* block could both
read `from = A` and journal `A→B` and `A→C` while the world ends at `C`.
CMD-30b closes that: every `BulkEdit`/`BulkHistory` commit, undo and publish
commit takes one process-wide coarse lock around plan+apply (`COMMIT_LOCK`).
Commits are rare, batched operations, so a coarse lock costs nothing measurable
and gives the stronger guarantee the publish path needs: two actors can never
interleave their per-section writes, and a concurrent fill of one section can
only leave it wholly one actor's state. The lock is not held during publication
(which spans ticks); resends read sections that each commit applied under the
world's own cache lock, so a section is never observed half-written.

## Publishing (CMD-30b)

`BulkHistory::commit_and_publish(world, edit)` commits the revision and queues
its publication. A caller that already holds the `BulkCommit` (from
`commit_with_changes`) can instead call `enqueue_publish(actor, changes)`. The
game tick drains the queue with `bulk::publish_tick`, one bounded slice per
tick. `BulkPublisher` is the testable core: `step(world, sink, budget)` returns
a `PublishProgress` and resumes on the next call.

A job has three phases, in this order:

1. **Settle.** For every changed cell, re-run the placement shape rule on the
   cell itself (`placement::reshape`) and on its six *unchanged* neighbours.
   Changed neighbours are skipped because their own reshape covers them; an
   interior cell of a solid fill therefore does no neighbour work at all. The
   corrections are applied as one batch (`World::apply_block_changes`), not
   journalled as separate acts: they are a derived consequence of the one
   revision, not a second user edit. This reuses the existing placement pass,
   not a per-block full recompute.
2. **Re-send.** The touched `16³` sections are collected while settling and
   collapsed to distinct chunk columns. The wire protocol has a column packet,
   not a section packet, so one packet carries every changed section of its
   column; a column is **never sent twice** even when many of its sections
   changed. That is stronger than "one packet per section" and avoids the
   per-block packet storm. Re-sending the column also re-runs the codec's light
   pass for it, which is the engine's current lighting model: incremental
   relight/cross-column propagation (S4) has not landed, so no separate light
   packet is sent.
3. **Hooks.** One callback per changed block: `after-block-break` when the
   result is air, `after-block-place` for every other change. This is the
   per-block choice (not batched), so a plugin that counts changes sees exactly
   `BulkResult::changed` calls. `after-block-place` is native-only: the frozen
   `@0.1.0` WASM world has no such export, and adding one would break every
   already-compiled component. An edit by an actor who is offline (console,
   RCON) changes the world and journal but fires no hook, because the hooks
   carry a `player`.

**Lighting (S4 is not merged).** At the time of writing `main` has no
incremental or cached light engine: the only primitive is
`aether_world::light::column::compute_column`, which recomputes a whole
column's sky and block light from its blocks, and the codecs call it while
encoding a column packet. Publishing therefore gets fresh light for the
affected region by re-sending every touched column — no separate `Light Update`
packet is sent because there is no incremental light state to send. This is
tested end to end at the primitive level
(`a_published_edit_rebuilds_the_affected_columns_light`): a bulk-placed
glowstone leaves block light 15 in its cell and 14 next to it after the
publish step. The remaining gap is exactly S4's: a *neighbouring* column whose
light would change only through cross-column propagation is not re-sent, and
there is no per-section light cache to update in place. When S4 lands it
replaces this per-column recompute with incremental relight and region light
updates; the publisher's `resend_column` is the single call site to route
through it.

The budget is configurable under `[bulk]`: `cells_per_tick`,
`columns_per_tick`, `hooks_per_tick` (all default `16384`, `32`, `16384`). Each is
a hard per-tick cap; a job resumes where it stopped, so an edit larger than the
budget always eventually completes. On the target machine the default
`cells_per_tick` step measured **≈ 14–16 ms** across runs on the shared builder
(`cargo-locked bench -p aether-server --bench bulk_edit`,
`bulk_publish/step_default_tick_budget`), leaving most of the 50 ms tick for
gameplay. A 1M-block fill is 61 settle steps, 49 column sends and
61 hook steps in the worst case (about six seconds), and never takes an unbounded
tick.

## Format version

`BlockGroup` is format **v6**. Older blobs decode unchanged; a tag-10 body under
a v1–v5 header is refused (`UnknownTag`), never guessed at, because a misread
revision would undo the wrong blocks.
