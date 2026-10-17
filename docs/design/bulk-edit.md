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

An undo **queues its own publication**, like any other commit, so the re-sent
columns are not left showing the post-edit state. Its settle phase re-runs over
the inverse's changed cells and their neighbours, which also restores the
derived fence/wall/pane/stair shapes that the original publish settled (those
corrections are not separate journal events). The same off-tick rule applies:
the undo commit is synchronous.

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
only leave it wholly one actor's state. A publish **settle slice** takes the
same lock around its read-and-correct, so a shape correction derived from one
state cannot overwrite a commit that landed in between.

The read side is covered separately. A chunk encoder used to copy a column
section by section (one cache read lock each), so a commit landing between two
copies produced a column whose lower half was before the edit and upper half
after. `ColumnView` now asks the source for a whole-column snapshot
(`BlockSource::copy_column` → `World::copy_column_sections`), which materialises
the column and then copies every section under **one** read lock. The
compat-pack wrapper forwards that snapshot too (`CarrierView::copy_column`
remaps every state), so a client with the pack does not fall back to torn
per-section copies. `main` has no incremental light engine yet, so that
snapshot is also what the per-column light pass reads; a test commits changes
to two sections from a writer thread while reading both from a `ColumnView` and
requires them to agree.

**A large commit still pauses concurrent reads once.** `plan` reads every
`from` cell, and `World::apply_block_changes` takes the world cache **write**
lock for the whole batch, so a gameplay read on another thread waits for that
apply. The publish settle no longer waits behind the commit: it takes
`COMMIT_LOCK` with `try_lock` and leaves the job queued when a commit owns it.
What remains is the one-off cache-lock pause, measured by the commit bench
(≈0.8 s for a 1M-block fill in release on the reference builder). A fully
non-blocking commit would need per-section shard locks or a chunked apply, which
would give up the single-batch atomic write; it is not claimed here.

## Publishing (CMD-30b)

`BulkHistory::commit_and_publish(world, edit)` commits the revision and queues
its publication. A caller that already holds the `BulkCommit` (from
`commit_with_changes`) can instead call `enqueue_publish(actor, changes)`. The
game tick drains the queue with `bulk::publish_tick`, one bounded slice per
tick. `BulkPublisher` is the testable core: `step(world, sink, budget)` returns
a `PublishProgress` and resumes on the next call.

**The commit is synchronous and off-tick.** One journal revision cannot be split
across ticks without breaking the one-event guarantee, so the write half is
*not* budgeted: the measured 16,384-block commit is ≈12.7 ms, making a
million-block fill's commit ≈0.8 s (plus the journal encoding, which the bench
includes). `commit_and_publish` must therefore run on a connection thread, a
console/RCON command, or a worker the caller spawns — **not** in a plugin
command handler, which the host runs on the tick thread. Only the publish half
below is tick-budgeted; the one-million-block test commits through the real
journal-and-world path and then asserts every publish step stays inside the
budget.

A job has three phases, in this order:

1. **Settle, to a fixed point.** Seed the changed cells and re-run the
   placement shape rule (`placement::reshape`) on each. Applying those
   corrections can change a neighbour's shape, and some shapes depend on a
   neighbour's *shape*, not only its block (a chest's `type` pairs with the
   neighbour's `type`). A single pass is therefore not enough: each correction
   re-queues its six neighbours, and the frontier is drained until no cell
   changes, before any re-send starts. Corrections are applied in batches
   (`World::apply_block_changes`), and the frontier is seeded from the
   **applied** batch, so a cell processed earlier in the same slice with the
   old state is re-checked. Corrections are not journalled as separate acts:
   they are a derived consequence of the one revision, not a second user edit.
   This reuses the existing placement pass, not a per-block full recompute.
   A chest-run test pins that the published state is a reshape fixed point.
2. **Re-send.** The touched `16³` sections are collected while settling and
   collapsed to distinct chunk columns. The wire protocol has a column packet,
   not a section packet, so one packet carries every changed section of its
   column; a column is **never sent twice** even when many of its sections
   changed. That is stronger than "one packet per section" and avoids the
   per-block packet storm. Re-sending the column re-runs the **modern** codecs'
   light pass for it, which is the engine's current lighting model for 1.21+;
   1.17/1.18 send their pre-existing full-bright light packet and 1.8 uses
   `FullBright`. Incremental relight/cross-column propagation (S4) has not
   landed, so no separate light packet is sent.
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
affected region from the **modern** codecs by re-sending every touched column —
no separate `Light Update` packet is sent because there is no incremental light
state to send, and the pre-1.21 codecs were already full-bright. This is
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
`columns_per_tick`, `hooks_per_tick` (defaults `4096`, `4`, `8192`). Each is a
per-tick safety cap; a job resumes where it stopped, so an edit larger than the
budget always eventually completes. The defaults are set from the measured
**worst** steps on the shared reference builder (`cargo-locked bench
-p aether-server --bench bulk_edit`):

| step | measured |
|---|---|
| `settle_step` (4,096 dense cells) | ≈ 1.0 ms |
| `last_settle_slice` (final full 4,096-cell seed slice) | ≈ 1.5 ms |
| `frontier_to_resend_step_real_encoder` (fixed-point slice + 4 columns through the real 1.21.11 codec, one viewer) | ≈ 3.4 ms |
| `hooks_step` (8,192 **sink calls**, not plugin dispatch) | ≈ 0.8 ms |
| `sparse_settle_step` (4,096 isolated cells; stone neighbours, reshape is a no-op) | ≈ 1.2 ms |
| `fence_sparse_settle_step` (4,096 fixed-point checks, each on a fence with six fence neighbours) | ≈ 2.4 ms |

The fence case is the worst shape work: each frontier position does four-to-six
neighbour reads and a `with_props`. It is the number the default is chosen
against; at 4,096 cells the measured worst step stays around 2–4 ms, under a
tenth of the 50 ms tick, with room for many viewers' column encodes. The hook number measures the
sink only; in production each call goes through the plugin host (`HOST` mutex +
plugin code), whose cost is unbounded in plugin code, which is why
`hooks_per_tick` is a safety cap rather than a time guarantee. `columns_per_tick`
is a **send per viewer**: `ServerSink` broadcasts a `ChunkColumn` and the
registry encodes it once per online client, so wall-clock cost scales with the
audience; lower it on a crowded server (the work just spans more ticks). The
bench's `frontier_to_resend` step is the actual combined worst step — a full
fixed-point slice followed by four encodes — because re-sends only start after
the frontier has drained, not right after the last seed slice.

With the defaults a 1M-block fill is 245 seed slices (`1,000,000 / 4,096` rounds
up), a proportional fixed-point frontier, 49 column sends (13 ticks at four per
tick) and 123 hook slices; publication completes over a few seconds and never
takes an unbounded tick. The commit half is separate and off-tick, above, and
its one-off cache-write pause is documented there.

## Format version

`BlockGroup` is format **v6**. Older blobs decode unchanged; a tag-10 body under
a v1–v5 header is refused (`UnknownTag`), never guessed at, because a misread
revision would undo the wrong blocks.
