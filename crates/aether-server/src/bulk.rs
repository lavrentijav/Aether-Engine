//! Bulk edit: batched multi-block writes with **one journal revision per
//! commit** (CMD-30a).
//!
//! A WorldEdit-style operation touches thousands or millions of blocks as a
//! single act. Writing each block through [`World::set_block_by`] would number
//! and journal each one separately: the history would read as a million
//! unrelated placements, and "undo the last thing I did" would become a
//! question about a count. This module collects the writes of one operation in
//! memory ([`BulkEdit`]) and commits them as a single [`EventBody::BlockGroup`]
//! revision — one event, one sequence number, one actor — while still applying
//! them to the world in one batch ([`World::apply_block_changes`]).
//!
//! # Read semantics
//!
//! [`BulkEdit::get`] sees the pre-edit world **plus this transaction's pending
//! writes**, so a `//copy` after a `//set` is self-consistent. Nothing is
//! written to the world until [`BulkEdit::commit`], so a reader outside the
//! transaction (anyone with the `World` alone) sees the old world until then,
//! and [`BulkEdit::abort`] leaves no trace at all — no block touched, no
//! journal entry.
//!
//! # Undo
//!
//! A commit records both sides of every change, so the revision is its own
//! inverse. [`BulkHistory`] keeps the most recent revisions in a **bounded**
//! in-memory log; when it overflows the oldest are dropped, but they are not
//! lost — the revision is in the journal, and [`BulkHistory::undo`] falls back
//! to reading it by id. Undo itself is a new revision (attributed to the
//! original author), so it is auditable and can be undone in turn. See
//! `docs/design/bulk-edit.md` for the spill policy.
//!
//! # Atomicity and concurrency
//!
//! A commit is all-or-nothing: the revision is written to the journal before
//! the world is touched, so a journal failure leaves the world unchanged and
//! surfaces as [`BulkError::Journal`] rather than a silent partial apply.
//! `commit` and `undo` therefore return `Result`.
//!
//! The commit is not, however, serialised against other writers: `plan` reads
//! each `from` from the live world and only then applies, so two transactions
//! over the same block can lose an update. This mirrors
//! [`World::set_block_by`](aether_api::World::set_block_by) and does not
//! corrupt the journal; CMD-30b owns per-region serialisation.

use std::collections::BTreeMap;
use std::collections::VecDeque;

use aether_api::{BlockChange, BlockStateId, JournalActor, World};
use aether_world::journal::EventBody;
use aether_world::{KvBackend, StorageError};
use aether_worldgen::ChunkGenerator;

/// A world position, used as the pending-write map key so iteration and
/// therefore commit order are deterministic.
type Pos = (i32, i32, i32);

/// Lowest addressable Y. Below this the sub-chunk index overflows the `i8` the
/// storage key uses, and [`World`] silently reads air; a bulk edit refuses it
/// rather than pretend the write landed.
pub const MIN_Y: i32 = -2048;
/// Highest addressable Y (inclusive): section `127`, local `15`.
pub const MAX_Y: i32 = 2047;

/// Default cap on distinct blocks one transaction may hold pending.
///
/// The bound exists so a mistyped `//fill` cannot exhaust memory before the
/// tick budget (CMD-30b) ever gets a chance to spread the work. A caller that
/// genuinely needs more can raise it with [`BulkEdit::with_limit`].
pub const DEFAULT_MAX_BLOCKS: usize = 1_000_000;

/// Why a bulk edit was refused.
#[derive(Debug)]
pub enum BulkError {
    /// A position lies outside the addressable vertical band
    /// ([`MIN_Y`]..=[`MAX_Y`]).
    OutOfBounds { x: i32, y: i32, z: i32 },
    /// The transaction would hold more than its block limit.
    TooLarge { requested: usize, limit: usize },
    /// A block id the world's registry does not know. Staging it would commit
    /// a state with no name and default properties.
    UnknownBlock(BlockStateId),
    /// The undo id names no revision in memory or in the journal.
    UnknownRevision(u64),
    /// The undo id names a journal event that is not a bulk revision.
    NotARevision(u64),
    /// The journal refused the write.
    Journal(StorageError),
}

impl std::fmt::Display for BulkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BulkError::OutOfBounds { x, y, z } => {
                write!(f, "position {x} {y} {z} is outside the addressable world")
            }
            BulkError::TooLarge { requested, limit } => {
                write!(f, "edit of {requested} blocks exceeds the limit of {limit}")
            }
            BulkError::UnknownBlock(id) => {
                write!(f, "block state {} is not known to this world", id.0)
            }
            BulkError::UnknownRevision(id) => write!(f, "no bulk revision {id}"),
            BulkError::NotARevision(id) => write!(f, "journal event {id} is not a bulk revision"),
            BulkError::Journal(e) => write!(f, "journal error: {e}"),
        }
    }
}

impl std::error::Error for BulkError {}

/// The outcome of a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkResult {
    /// How many blocks actually changed (a write to the state a block already
    /// held is not counted and is not journalled).
    pub changed: usize,
    /// The journal sequence number of the revision, which is also its undo id.
    /// `0` when nothing changed — no revision was written.
    pub undo_id: u64,
}

fn check_y(x: i32, y: i32, z: i32) -> Result<(), BulkError> {
    if (MIN_Y..=MAX_Y).contains(&y) {
        Ok(())
    } else {
        Err(BulkError::OutOfBounds { x, y, z })
    }
}

/// The world's registry must know `block`, or committing it would store a
/// state the registry cannot name and whose properties would be guessed.
fn check_block<B: KvBackend, G: ChunkGenerator>(
    world: &World<B, G>,
    block: BlockStateId,
) -> Result<(), BulkError> {
    if world.block_name_of(block).is_some() {
        Ok(())
    } else {
        Err(BulkError::UnknownBlock(block))
    }
}

/// Compute the changes a pending map represents against the world's current
/// (pre-edit) state, in deterministic order, dropping no-ops.
fn plan<B: KvBackend, G: ChunkGenerator>(
    pending: &BTreeMap<Pos, BlockStateId>,
    world: &World<B, G>,
) -> Vec<BlockChange> {
    pending
        .iter()
        .filter_map(|(&(x, y, z), &to)| {
            let from = world.get_block(x, y, z);
            (from != to).then_some(BlockChange { x, y, z, from, to })
        })
        .collect()
}

/// A batched multi-block edit that has not been committed yet.
///
/// Accumulate writes with [`BulkEdit::set`] / [`BulkEdit::fill`], read the
/// world as the transaction sees it with [`BulkEdit::get`], then either
/// [`BulkEdit::commit`] them as one revision or [`BulkEdit::abort`] and touch
/// nothing. The pending map is not a second copy of the world: it holds only
/// the positions this transaction writes.
pub struct BulkEdit {
    actor: JournalActor,
    pending: BTreeMap<Pos, BlockStateId>,
    limit: usize,
}

impl BulkEdit {
    /// A new, empty transaction attributed to `actor`.
    pub fn new(actor: JournalActor) -> Self {
        Self {
            actor,
            pending: BTreeMap::new(),
            limit: DEFAULT_MAX_BLOCKS,
        }
    }

    /// [`BulkEdit::new`] with an explicit block limit.
    pub fn with_limit(actor: JournalActor, limit: usize) -> Self {
        Self {
            actor,
            pending: BTreeMap::new(),
            limit,
        }
    }

    /// The actor every change of this transaction is journalled under.
    pub fn actor(&self) -> JournalActor {
        self.actor
    }

    /// How many distinct positions are pending.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// The block limit this transaction enforces.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Stage a write of `block` at `(x, y, z)`.
    ///
    /// The world is not touched until commit. Writing the state a position
    /// already holds (as this transaction sees it, pending included) is
    /// allowed and simply becomes a no-op at commit.
    pub fn set<B: KvBackend, G: ChunkGenerator>(
        &mut self,
        world: &World<B, G>,
        x: i32,
        y: i32,
        z: i32,
        block: BlockStateId,
    ) -> Result<(), BulkError> {
        check_y(x, y, z)?;
        check_block(world, block)?;
        if self.pending.len() >= self.limit && !self.pending.contains_key(&(x, y, z)) {
            return Err(BulkError::TooLarge {
                requested: self.pending.len() + 1,
                limit: self.limit,
            });
        }
        self.pending.insert((x, y, z), block);
        Ok(())
    }

    /// Stage filling the inclusive box between `min` and `max` with `block`.
    ///
    /// Corners may be given in any order; the box is normalized. A box that
    /// would take the transaction past its limit is refused **before** any
    /// pending write is changed.
    pub fn fill<B: KvBackend, G: ChunkGenerator>(
        &mut self,
        world: &World<B, G>,
        min: (i32, i32, i32),
        max: (i32, i32, i32),
        block: BlockStateId,
    ) -> Result<(), BulkError> {
        let (x1, y1, z1) = (min.0.min(max.0), min.1.min(max.1), min.2.min(max.2));
        let (x2, y2, z2) = (min.0.max(max.0), min.1.max(max.1), min.2.max(max.2));
        check_y(x1, y1, z1)?;
        check_y(x2, y2, z2)?;
        check_block(world, block)?;

        // Compute the box volume arithmetically in `u128`, and reject a box
        // bigger than the limit **before** touching a position. The earlier
        // version counted the box with a triple loop, so a mistyped selection
        // (`//fill ~-2e9 … ~2e9 …`) iterated billions of positions — a hang,
        // not a refusal — and the `u64` counter could overflow. `x`/`z` are
        // unbounded, so this arithmetic bound is the only thing standing
        // between a typo and a stuck tick thread.
        //
        // `volume > limit` alone is enough to refuse: distinct new positions
        // are at least `volume - overlap` and `overlap <= pending.len()`, so
        // `pending.len() + new >= volume > limit` no matter how much of the
        // box is already staged.
        let dx = (x2 as i64 - x1 as i64 + 1) as u128;
        let dy = (y2 as i64 - y1 as i64 + 1) as u128;
        let dz = (z2 as i64 - z1 as i64 + 1) as u128;
        let volume = dx * dy * dz;
        if volume > self.limit as u128 {
            return Err(BulkError::TooLarge {
                requested: volume.min(usize::MAX as u128) as usize,
                limit: self.limit,
            });
        }
        // `volume <= limit` here, so the counting loop is bounded by the limit
        // and `added` cannot overflow.
        let mut added = 0usize;
        for x in x1..=x2 {
            for y in y1..=y2 {
                for z in z1..=z2 {
                    if !self.pending.contains_key(&(x, y, z)) {
                        added += 1;
                    }
                }
            }
        }
        let requested = self.pending.len() + added;
        if requested > self.limit {
            return Err(BulkError::TooLarge {
                requested,
                limit: self.limit,
            });
        }
        for x in x1..=x2 {
            for y in y1..=y2 {
                for z in z1..=z2 {
                    self.pending.insert((x, y, z), block);
                }
            }
        }
        Ok(())
    }

    /// The block at `(x, y, z)` as **this transaction** sees it: a pending
    /// write if there is one, otherwise the pre-edit world.
    pub fn get<B: KvBackend, G: ChunkGenerator>(
        &self,
        world: &World<B, G>,
        x: i32,
        y: i32,
        z: i32,
    ) -> BlockStateId {
        match self.pending.get(&(x, y, z)) {
            Some(id) => *id,
            None => world.get_block(x, y, z),
        }
    }

    /// Commit the transaction as one journal revision.
    ///
    /// Returns how many blocks changed and the revision's undo id, or an error
    /// if the journal refused the write. A commit of zero changes writes
    /// nothing and returns `Ok(undo_id == 0)`. The commit is all-or-nothing:
    /// on error the world is left exactly as it was (see
    /// [`World::commit_block_group`]), so a failed edit is never silently
    /// applied.
    pub fn commit<B: KvBackend, G: ChunkGenerator>(
        self,
        world: &World<B, G>,
    ) -> Result<BulkResult, BulkError> {
        let changes = plan(&self.pending, world);
        if changes.is_empty() {
            return Ok(BulkResult {
                changed: 0,
                undo_id: 0,
            });
        }
        let undo_id = world
            .commit_block_group(self.actor, &changes)
            .map_err(BulkError::Journal)?;
        Ok(BulkResult {
            changed: changes.len(),
            // Non-empty, so the journal always returned an id.
            undo_id: undo_id.unwrap_or(0),
        })
    }

    /// Discard the transaction. The world was never touched, so this writes
    /// nothing and needs no undo.
    pub fn abort(self) {
        // Dropping `pending` is the whole of it: no world write ever happened,
        // so there is no journal entry to compensate for.
    }
}

struct UndoRecord {
    undo_id: u64,
    actor: JournalActor,
    changes: Vec<BlockChange>,
}

/// A bounded, journal-backed log of committed bulk revisions, for undo.
///
/// The newest [`BulkHistory::capacity`] revisions are kept in memory. Older
/// ones are **spilled from memory, not lost**: [`BulkHistory::undo`] falls
/// back to reading the revision from the journal by id, so any committed
/// revision can still be undone however long ago it was. A capacity of zero
/// keeps nothing hot and undoes everything straight from the journal.
pub struct BulkHistory {
    capacity: usize,
    entries: VecDeque<UndoRecord>,
}

impl BulkHistory {
    /// A history holding the `capacity` most recent revisions hot.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::new(),
        }
    }

    /// How many revisions are kept hot.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many revisions are currently hot.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no revision is hot.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether revision `undo_id` is hot (still in memory).
    pub fn contains(&self, undo_id: u64) -> bool {
        self.entries.iter().any(|r| r.undo_id == undo_id)
    }

    /// Record a committed revision's inverse. Spills the oldest when full.
    pub fn record(&mut self, undo_id: u64, actor: JournalActor, changes: Vec<BlockChange>) {
        if self.capacity == 0 {
            return;
        }
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(UndoRecord {
            undo_id,
            actor,
            changes,
        });
    }

    /// Commit `edit` and record its revision for undo.
    ///
    /// On a journal failure nothing is applied and nothing is recorded; the
    /// error propagates. A zero-change commit records nothing and returns
    /// `undo_id == 0`.
    pub fn commit<B: KvBackend, G: ChunkGenerator>(
        &mut self,
        world: &World<B, G>,
        edit: BulkEdit,
    ) -> Result<BulkResult, BulkError> {
        let actor = edit.actor;
        let changes = plan(&edit.pending, world);
        if changes.is_empty() {
            return Ok(BulkResult {
                changed: 0,
                undo_id: 0,
            });
        }
        let undo_id = world
            .commit_block_group(actor, &changes)
            .map_err(BulkError::Journal)?;
        if let Some(undo_id) = undo_id {
            self.record(undo_id, actor, changes.clone());
        }
        Ok(BulkResult {
            changed: changes.len(),
            undo_id: undo_id.unwrap_or(0),
        })
    }

    /// Undo the revision `undo_id`, restoring every block it changed.
    ///
    /// Uses the hot record when there is one and the journal otherwise, so a
    /// spilled revision undoes identically. The undo is applied as a **new**
    /// revision attributed to the original author and recorded for a possible
    /// redo; the caller can undo the returned `undo_id` to reapply.
    pub fn undo<B: KvBackend, G: ChunkGenerator>(
        &mut self,
        world: &World<B, G>,
        undo_id: u64,
    ) -> Result<BulkResult, BulkError> {
        if undo_id == 0 {
            return Err(BulkError::UnknownRevision(0));
        }
        let hot = self.entries.iter().position(|r| r.undo_id == undo_id);
        let (actor, changes) = match hot {
            Some(i) => {
                let rec = self.entries.remove(i).expect("position just found");
                (rec.actor, rec.changes)
            }
            None => {
                let event = world.journal().get(undo_id).map_err(BulkError::Journal)?;
                match event {
                    Some(e) => match e.body {
                        EventBody::BlockGroup { changes } => (e.actor, changes),
                        _ => return Err(BulkError::NotARevision(undo_id)),
                    },
                    None => return Err(BulkError::UnknownRevision(undo_id)),
                }
            }
        };
        // The inverse: put back what was there. `from` is read from the world
        // now, not copied from the revision, so the undo event records the
        // state it is actually replacing — accurate even if someone edited the
        // same block since. A position already holding its old state is
        // dropped rather than journalled as a no-op.
        let inverse: Vec<BlockChange> = changes
            .iter()
            .filter_map(|c| {
                let from = world.get_block(c.x, c.y, c.z);
                (from != c.from).then_some(BlockChange {
                    x: c.x,
                    y: c.y,
                    z: c.z,
                    from,
                    to: c.from,
                })
            })
            .collect();
        if inverse.is_empty() {
            return Ok(BulkResult {
                changed: 0,
                undo_id: 0,
            });
        }
        let new_id = world
            .commit_block_group(actor, &inverse)
            .map_err(BulkError::Journal)?
            .unwrap_or(0);
        if new_id != 0 {
            self.record(new_id, actor, inverse.clone());
        }
        Ok(BulkResult {
            changed: inverse.len(),
            undo_id: new_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::{block_ids, MemStore};
    use aether_worldgen::FlatGenerator;

    type TestWorld = World<MemStore, FlatGenerator>;

    fn world() -> TestWorld {
        World::new(MemStore::new(), FlatGenerator::classic())
    }

    fn actor(id: u128) -> JournalActor {
        JournalActor(id)
    }

    #[test]
    fn reads_see_pending_inside_and_the_old_world_outside() {
        let world = world();
        // An edit made outside the transaction, so "old world" is not just
        // generator output.
        world.set_block(0, 10, 0, "minecraft:dirt").unwrap();

        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 5, 10, 5, block_ids::STONE).unwrap();

        // Inside the pending region: the pending write.
        assert_eq!(edit.get(&world, 5, 10, 5), block_ids::STONE);
        // Outside it: the pre-edit world, transaction or not.
        assert_eq!(edit.get(&world, 0, 10, 0), block_ids::DIRT);
        assert_eq!(world.get_block(5, 10, 5), BlockStateId::AIR, "uncommitted");
    }

    #[test]
    fn a_copy_after_a_set_sees_the_set() {
        // The pending-read rule is what makes `//set stone` then `//copy`
        // consistent: the second read must see the first's writes.
        let world = world();
        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 1, 20, 1, block_ids::STONE).unwrap();
        assert_eq!(edit.get(&world, 1, 20, 1), block_ids::STONE);
        // A second write to the same cell overrides the first.
        edit.set(&world, 1, 20, 1, block_ids::DIRT).unwrap();
        assert_eq!(edit.get(&world, 1, 20, 1), block_ids::DIRT);
    }

    #[test]
    fn abort_leaves_the_world_unchanged_and_writes_no_journal() {
        let world = world();
        let head = world.journal().head();
        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 3, 20, 3, block_ids::STONE).unwrap();
        edit.fill(&world, (0, 20, 0), (2, 21, 2), block_ids::DIRT)
            .unwrap();
        edit.abort();
        assert_eq!(world.get_block(3, 20, 3), BlockStateId::AIR);
        assert_eq!(world.get_block(1, 20, 1), BlockStateId::AIR);
        assert_eq!(world.journal().head(), head, "abort wrote no journal entry");
    }

    #[test]
    fn commit_of_zero_changes_writes_nothing() {
        let world = world();
        let head = world.journal().head();
        // Setting a cell to what it already holds is a no-op at commit.
        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 4, 20, 4, world.get_block(4, 20, 4))
            .unwrap();
        let result = edit.commit(&world).unwrap();
        assert_eq!(result.changed, 0);
        assert_eq!(result.undo_id, 0);
        assert_eq!(world.journal().head(), head, "no revision written");
    }

    #[test]
    fn a_commit_is_exactly_one_journal_revision() {
        let world = world();
        let head = world.journal().head();
        let mut edit = BulkEdit::new(actor(7));
        for i in 0..100 {
            edit.set(&world, i, 30, 0, block_ids::STONE).unwrap();
        }
        let result = edit.commit(&world).unwrap();
        assert_eq!(result.changed, 100);
        assert_eq!(
            world.journal().head(),
            head + 1,
            "one operation, one sequence number"
        );
        let ev = world.journal().get(result.undo_id).unwrap().expect("event");
        assert_eq!(ev.actor, actor(7));
        match ev.body {
            EventBody::BlockGroup { changes } => assert_eq!(changes.len(), 100),
            other => panic!("expected a block group, got {other:?}"),
        }
    }

    #[test]
    fn a_committed_edit_reappears_after_a_column_reload() {
        // The revision must be replayable: a column materialized again (a
        // restart) reads the journal and must apply the whole group.
        let backend = std::sync::Arc::new(MemStore::new());
        {
            let world = World::new(std::sync::Arc::clone(&backend), FlatGenerator::classic());
            let mut edit = BulkEdit::new(actor(1));
            edit.set(&world, 1, 40, 1, block_ids::STONE).unwrap();
            // A second column, so the group's multi-position index matters.
            edit.set(&world, 17, 40, 1, block_ids::SAND).unwrap();
            edit.commit(&world).unwrap();
        }
        let world = World::new(backend, FlatGenerator::classic());
        assert_eq!(world.get_block(1, 40, 1), block_ids::STONE);
        assert_eq!(world.get_block(17, 40, 1), block_ids::SAND);
    }

    #[test]
    fn undo_inverts_the_commit_exactly() {
        let world = world();
        // Seed a non-trivial pre-state.
        world.set_block(2, 20, 2, "minecraft:dirt").unwrap();
        let before = world.get_block(2, 20, 2);

        let mut history = BulkHistory::new(8);
        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 2, 20, 2, block_ids::STONE).unwrap();
        edit.set(&world, 3, 20, 2, block_ids::STONE).unwrap();
        let result = history.commit(&world, edit).unwrap();
        assert_eq!(result.changed, 2);
        assert_eq!(world.get_block(2, 20, 2), block_ids::STONE);

        history.undo(&world, result.undo_id).unwrap();
        assert_eq!(world.get_block(2, 20, 2), before, "exact restore");
        assert_eq!(world.get_block(3, 20, 2), BlockStateId::AIR);
    }

    #[test]
    fn undo_records_the_state_it_actually_replaces() {
        let world = world();
        let mut history = BulkHistory::new(4);
        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 1, 20, 1, block_ids::STONE).unwrap();
        let result = history.commit(&world, edit).unwrap();
        // Someone else changes the same block after the bulk commit.
        world.set_block(1, 20, 1, "minecraft:dirt").unwrap();
        let undone = history.undo(&world, result.undo_id).unwrap();
        assert_eq!(world.get_block(1, 20, 1), BlockStateId::AIR);
        // The undo event records replacing dirt (the state it found), not the
        // stone the bulk revision had put there.
        let ev = world.journal().get(undone.undo_id).unwrap().unwrap();
        match ev.body {
            EventBody::BlockGroup { changes } => {
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].from, block_ids::DIRT);
                assert_eq!(changes[0].to, BlockStateId::AIR);
            }
            other => panic!("expected a block group, got {other:?}"),
        }
    }

    #[test]
    fn undo_works_after_the_record_spilled_from_memory() {
        let world = world();
        let mut history = BulkHistory::new(2);
        let mut first_id = 0;
        for i in 0..3 {
            let mut edit = BulkEdit::new(actor(1));
            edit.set(&world, i, 40, 0, block_ids::STONE).unwrap();
            let r = history.commit(&world, edit).unwrap();
            if i == 0 {
                first_id = r.undo_id;
            }
        }
        assert!(!history.contains(first_id), "the oldest record spilled");
        assert_eq!(history.len(), 2);
        // ...but it is still in the journal, so undo reaches it regardless.
        history.undo(&world, first_id).unwrap();
        assert_eq!(world.get_block(0, 40, 0), BlockStateId::AIR);
    }

    #[test]
    fn undo_of_an_unknown_revision_is_an_error() {
        let world = world();
        let mut history = BulkHistory::new(4);
        assert!(matches!(
            history.undo(&world, 999),
            Err(BulkError::UnknownRevision(999))
        ));
        assert!(matches!(
            history.undo(&world, 0),
            Err(BulkError::UnknownRevision(0))
        ));
    }

    #[test]
    fn an_out_of_range_position_is_refused() {
        let world = world();
        let mut edit = BulkEdit::new(actor(1));
        assert!(matches!(
            edit.set(&world, 0, MAX_Y + 1, 0, block_ids::STONE),
            Err(BulkError::OutOfBounds { .. })
        ));
        assert!(matches!(
            edit.set(&world, 0, MIN_Y - 1, 0, block_ids::STONE),
            Err(BulkError::OutOfBounds { .. })
        ));
        assert!(edit.is_empty(), "a refused write stages nothing");
    }

    #[test]
    fn the_block_limit_is_enforced_before_any_write_is_staged() {
        let world = world();
        let mut edit = BulkEdit::with_limit(actor(1), 4);
        edit.set(&world, 0, 20, 0, block_ids::STONE).unwrap();
        // A 5-wide fill adds four new positions, taking the total to 5 > 4;
        // refused whole, leaving the one staged write untouched.
        assert!(matches!(
            edit.fill(&world, (0, 20, 0), (4, 20, 0), block_ids::DIRT),
            Err(BulkError::TooLarge { .. })
        ));
        assert_eq!(edit.len(), 1, "the refused fill staged nothing");
        // A fill that fits is accepted.
        edit.fill(&world, (0, 20, 0), (2, 20, 0), block_ids::DIRT)
            .unwrap();
        assert_eq!(edit.len(), 3);
    }

    #[test]
    fn two_actors_commits_are_ordered_and_journalled() {
        // Two transactions interleaved on one world: each commit is its own
        // revision, the sequence numbers are distinct and increasing, and each
        // change is attributed to its actor.
        let world = world();
        let mut a = BulkEdit::new(actor(0xAA));
        let mut b = BulkEdit::new(actor(0xBB));
        a.set(&world, 0, 20, 0, block_ids::STONE).unwrap();
        b.set(&world, 100, 20, 0, block_ids::DIRT).unwrap();
        let ra = a.commit(&world).unwrap();
        let rb = b.commit(&world).unwrap();
        assert!(rb.undo_id > ra.undo_id, "later commit, later sequence");
        let ea = world.journal().get(ra.undo_id).unwrap().unwrap();
        let eb = world.journal().get(rb.undo_id).unwrap().unwrap();
        assert_eq!(ea.actor, actor(0xAA));
        assert_eq!(eb.actor, actor(0xBB));
        assert_eq!(world.get_block(0, 20, 0), block_ids::STONE);
        assert_eq!(world.get_block(100, 20, 0), block_ids::DIRT);
    }

    #[test]
    fn concurrent_commits_from_two_actors_are_ordered_and_journalled() {
        // Two transactions from two threads on one world. Each commit is its
        // own revision with a distinct sequence number and its own actor, and
        // neither region is torn.
        let world = std::sync::Arc::new(world());
        let head = world.journal().head();

        let w1 = std::sync::Arc::clone(&world);
        let t1 = std::thread::spawn(move || {
            let mut edit = BulkEdit::new(actor(0xAA));
            for i in 0..16 {
                edit.set(&w1, i, 20, 0, block_ids::STONE).unwrap();
            }
            edit.commit(&w1)
        });
        let w2 = std::sync::Arc::clone(&world);
        let t2 = std::thread::spawn(move || {
            let mut edit = BulkEdit::new(actor(0xBB));
            for i in 0..16 {
                edit.set(&w2, 1000 + i, 20, 0, block_ids::DIRT).unwrap();
            }
            edit.commit(&w2)
        });

        let r1 = t1.join().unwrap().unwrap();
        let r2 = t2.join().unwrap().unwrap();
        assert_ne!(r1.undo_id, r2.undo_id, "two distinct revisions");
        assert_eq!(world.journal().head(), head + 2, "two revisions written");
        for i in 0..16 {
            assert_eq!(world.get_block(i, 20, 0), block_ids::STONE);
            assert_eq!(world.get_block(1000 + i, 20, 0), block_ids::DIRT);
        }
        let actors: std::collections::HashSet<u128> = [r1.undo_id, r2.undo_id]
            .iter()
            .map(|&id| world.journal().get(id).unwrap().unwrap().actor.0)
            .collect();
        assert_eq!(actors, [0xAA_u128, 0xBB].into_iter().collect());
    }

    #[test]
    fn random_edits_commit_and_undo_to_the_exact_prior_state() {
        // A small deterministic LCG stands in for a property test's random
        // source; the claim is the same: whatever was edited, undo restores
        // every cell byte-for-byte.
        let world = world();
        let mut seed: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as i32
        };
        let palette = [
            block_ids::STONE,
            block_ids::DIRT,
            block_ids::GRASS_BLOCK,
            BlockStateId::AIR,
        ];
        let mut positions: Vec<(i32, i32, i32)> = Vec::new();
        let mut history = BulkHistory::new(16);
        for _ in 0..20 {
            let mut edit = BulkEdit::new(actor(1));
            for _ in 0..64 {
                let x = next().rem_euclid(64);
                let y = 32 + next().rem_euclid(8);
                let z = next().rem_euclid(64);
                let block = palette[next().rem_euclid(palette.len() as i32) as usize];
                edit.set(&world, x, y, z, block).unwrap();
                positions.push((x, y, z));
            }
            let snapshot: Vec<BlockStateId> = positions
                .iter()
                .map(|&(x, y, z)| world.get_block(x, y, z))
                .collect();
            let result = history.commit(&world, edit).unwrap();
            assert!(result.changed > 0);
            history.undo(&world, result.undo_id).unwrap();
            for (p, &was) in positions.iter().zip(&snapshot) {
                assert_eq!(
                    world.get_block(p.0, p.1, p.2),
                    was,
                    "undo did not restore {:?}",
                    p
                );
            }
        }
    }

    #[test]
    fn a_committed_bulk_change_counts_as_a_block_change() {
        // Finding 3: a `BlockGroup` is a block change like any other, so the
        // history queries and worldgen-original check must see it.
        let world = world();
        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 5, 20, 5, block_ids::STONE).unwrap();
        edit.commit(&world).unwrap();
        let j = world.journal();
        assert!(
            j.last_block_change_at(5, 20, 5).unwrap().is_some(),
            "the position was changed"
        );
        assert!(
            !j.is_worldgen_original(5, 20, 5).unwrap(),
            "a bulk edit makes a block non-original"
        );
        assert!(
            !j.block_history(5, 20, 5, None, 10).unwrap().is_empty(),
            "a bulk edit appears in the block's history"
        );
    }

    #[test]
    fn a_failed_journal_write_does_not_apply_the_edit() {
        // Finding 1: the commit is atomic. A backend that fails its writes
        // must leave the world exactly as it was, and surface the error
        // rather than a silent `undo_id == 0`.
        use aether_world::{KvBackend, StorageError};
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Failing {
            inner: MemStore,
            fail: AtomicBool,
        }
        impl KvBackend for Failing {
            fn put(&self, k: &[u8], v: &[u8]) -> Result<(), StorageError> {
                if self.fail.load(Ordering::SeqCst) {
                    return Err(StorageError::Backend("disk full".into()));
                }
                self.inner.put(k, v)
            }
            fn get(&self, k: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
                self.inner.get(k)
            }
            fn delete(&self, k: &[u8]) -> Result<(), StorageError> {
                self.inner.delete(k)
            }
            fn scan_prefix(&self, p: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
                self.inner.scan_prefix(p)
            }
        }

        let backend = Failing {
            inner: MemStore::new(),
            fail: AtomicBool::new(false),
        };
        let world = World::new(&backend, FlatGenerator::classic());
        let before = world.get_block(0, 20, 0);
        backend.fail.store(true, Ordering::SeqCst);

        let mut edit = BulkEdit::new(actor(1));
        edit.set(&world, 0, 20, 0, block_ids::STONE).unwrap();
        match edit.commit(&world) {
            Err(BulkError::Journal(_)) => {}
            other => panic!("expected a journal error, got {other:?}"),
        }
        assert_eq!(
            world.get_block(0, 20, 0),
            before,
            "the failed commit left the world untouched"
        );
    }

    #[test]
    fn a_huge_fill_is_refused_without_scanning_it() {
        // Finding 2: a mistyped fill must be refused by arithmetic, not by
        // iterating (which would hang). These boxes are far larger than the
        // limit and would take effectively forever to count.
        let world = world();
        let mut edit = BulkEdit::new(actor(1));
        assert!(matches!(
            edit.fill(&world, (0, 20, 0), (1_000_000_000, 20, 0), block_ids::STONE),
            Err(BulkError::TooLarge { .. })
        ));
        assert!(matches!(
            edit.fill(
                &world,
                (i32::MIN, 20, i32::MIN),
                (i32::MAX, 20, i32::MAX),
                block_ids::STONE
            ),
            Err(BulkError::TooLarge { .. })
        ));
        assert!(edit.is_empty(), "a refused fill staged nothing");
    }

    #[test]
    fn an_unknown_block_id_is_refused() {
        // Finding 9: the `world` parameter is used to reject a state the
        // registry cannot name, instead of committing default properties.
        let world = world();
        let mut edit = BulkEdit::new(actor(1));
        assert!(matches!(
            edit.set(&world, 0, 20, 0, BlockStateId(999_999)),
            Err(BulkError::UnknownBlock(_))
        ));
        assert!(matches!(
            edit.fill(&world, (0, 20, 0), (1, 20, 1), BlockStateId(999_999)),
            Err(BulkError::UnknownBlock(_))
        ));
        assert!(edit.is_empty());
    }
}
