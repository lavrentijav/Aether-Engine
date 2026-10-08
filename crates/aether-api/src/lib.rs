//! # aether-api
//!
//! The Aether Engine **core API**: a single [`World`] facade that ties the
//! storage, generation and physics subsystems together behind a small,
//! coordinate-oriented surface.
//!
//! ```no_run
//! use aether_api::World;
//! use aether_worldgen::NoiseGenerator;
//! use aether_world::MemStore;
//!
//! // A world backed by an in-memory store and noise terrain.
//! let world = World::new(MemStore::new(), NoiseGenerator::new(42));
//!
//! // Blocks are addressed in world coordinates; chunks generate on demand.
//! let ground = world.height_hint(0, 0);
//! let _ = world.get_block(0, ground, 0);
//!
//! // Drop a player in and let physics settle it onto the ground.
//! let mut player = world.spawn_player(0.5, 200.0, 0.5);
//! for _ in 0..400 {
//!     world.step_body(&mut player);
//! }
//! assert!(player.on_ground);
//! ```
//!
//! `World` uses interior mutability, so `&World` is enough to read blocks, run
//! physics and stream chunks — it can be shared across the tick's worker pool.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use aether_core::math::Vec3;
use aether_physics::{step, BlockView};
use aether_world::journal::{ActorId, EventBody, Filter, Journal, Restore};
use aether_world::registry::BlockRegistry;
use aether_world::{KvBackend, StorageError, SubChunk, WorldStorage};
use aether_worldgen::ChunkGenerator;

pub mod player;

// Public re-exports: the pieces callers most often need alongside `World`.
pub use aether_core::math::{Aabb, Vec3 as Vector3};
pub use aether_physics::{Body, PhysicsParams};
pub use aether_world::journal::{ActorId as JournalActor, Anomaly, Event, ItemUid, Ledger, Place};
pub use aether_world::registry::ids as block_ids;
pub use aether_world::storage::format::SubChunkKey;
pub use aether_world::{BlockProperties, BlockStateId, FullBright, LightView, MemStore};
pub use aether_worldgen::{FlatGenerator, NoiseGenerator};
pub use player::{GameMode, Player};

#[cfg(feature = "fjall")]
pub use aether_world::FjallStore;

// Vertical band of sub-chunks scanned when loading a column from storage:
// sections -4..=23, which is the engine's `-64..=383`.
const SCAN_CY_MIN: i8 = -4;
const SCAN_CY_MAX: i8 = 23;

/// Split a world coordinate into `(chunk-or-section index, local 0..16)`.
#[inline]
fn split(v: i32) -> (i32, usize) {
    (v.div_euclid(16), v.rem_euclid(16) as usize)
}

/// How a world treats sub-chunk snapshots saved under another revision of its
/// generator. See [`World::with_snapshot_policy`].
pub struct SnapshotPolicy {
    /// The generator's current revision. Snapshots saved from now on are
    /// stamped with it; any other stamp, or none, marks a snapshot as stale.
    pub revision: u32,
    /// Whether a block found in a stale snapshot, where the fresh generator
    /// put something else, is one a player placed — a block the generator
    /// that wrote the snapshot could not have produced — and so survives the
    /// rebuild. Everything else in a stale snapshot is the old terrain and is
    /// replaced by the new.
    pub keep: Box<dyn Fn(BlockStateId) -> bool + Send + Sync>,
    /// Told about each rebuilt sub-chunk and how many of its blocks were kept.
    pub on_rebuild: Box<dyn Fn(SubChunkKey, usize) + Send + Sync>,
}

/// A column's materialization latch, and when it was last used.
///
/// The `bool` is "this column is in the cache"; the `Mutex` is what makes a
/// late arrival *wait* for the thread that is still generating rather than
/// racing ahead and reading air out of the not-yet-populated cache.
///
/// Holding an `Arc` of it pins the column: [`World::unload`] leaves alone
/// every column whose latch anyone but the world itself holds, so a reader
/// or a writer keeps its own clone until it is done with the cache.
struct Latch {
    materialized: Mutex<bool>,
    /// The [`World::unload`] sweep during which the column was last touched.
    last_used: AtomicU64,
}

/// What one [`World::unload`] sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnloadStats {
    /// Columns unloaded because no player needs them.
    pub unneeded: usize,
    /// Columns unloaded, least recently used first, to get back within the
    /// budget.
    pub over_budget: usize,
    /// Columns resident after the sweep.
    pub resident: usize,
}

/// A live world: block access, on-demand generation and physics over a
/// [`KvBackend`] and a [`ChunkGenerator`].
pub struct World<B: KvBackend, G: ChunkGenerator> {
    storage: WorldStorage<Arc<B>>,
    /// Every change anyone has made, and the rollback over it. Shares the
    /// backend with `storage` under a disjoint key prefix.
    journal: Journal<Arc<B>>,
    generator: G,
    /// Behind a lock because a block name can be met for the first time at
    /// runtime — a player placing something the generator never emits — and
    /// interning it needs to mutate the table through a shared `&World`.
    registry: RwLock<BlockRegistry>,
    cache: RwLock<HashMap<SubChunkKey, SubChunk>>,
    /// Sections with edits not yet saved, each with the number of its latest
    /// edit, so a flush clears only the marks it actually saved.
    dirty: RwLock<HashMap<SubChunkKey, u64>>,
    /// Numbers the edits that mark sections dirty.
    edits: AtomicU64,
    /// Per-column materialization latch: see [`Latch`]. Keyed per column so
    /// unrelated columns still generate in parallel. An entry exists exactly
    /// while the column may be resident; [`World::unload`] removes both.
    columns: RwLock<HashMap<(i32, i32), Arc<Latch>>>,
    /// The current [`World::unload`] sweep, stamped on each column touched.
    sweep: AtomicU64,
    params: PhysicsParams,
    snapshots: Option<SnapshotPolicy>,
}

impl<B: KvBackend, G: ChunkGenerator> World<B, G> {
    /// The generator this world is built on.
    ///
    /// Exposed so a caller can ask it about itself — a cache wrapper reporting
    /// its hit rate, say. Read-only: swapping the generator under a world that
    /// has already latched columns would make its baseline disagree with the
    /// one the journal was recorded against.
    pub fn generator(&self) -> &G {
        &self.generator
    }

    /// Create a world from a storage backend and a chunk generator.
    pub fn new(backend: B, generator: G) -> Self {
        let backend = Arc::new(backend);
        let journal = Journal::open(Arc::clone(&backend))
            .expect("journal head unreadable; refusing to renumber history");
        let storage = WorldStorage::new(backend);
        // A world that has been saved before carries the name table its stored
        // ids refer to. Restoring it is what keeps those ids meaning the same
        // block across restarts; a world without one has only ever used the
        // seeded blocks, whose ids are fixed anyway.
        // A saved table that cannot be restored is *not* something to shrug
        // at. `BlockRegistry::restore` returns `None` exactly when the save was
        // written by an incompatible build — and falling back to a fresh
        // registry then reinterprets every stored id as a different block,
        // which is the corruption that check exists to prevent. It used to do
        // precisely that.
        let registry = match storage.load_registry() {
            Ok(None) => BlockRegistry::default(),
            Ok(Some(table)) => match BlockRegistry::restore(&table) {
                Some(r) => r,
                None => panic!(
                    "world at this path was written by an incompatible build: its block \
                     table does not start with this build's ({} names stored). Opening it \
                     would silently reinterpret every stored block id. Move the world \
                     directory aside, or restore a build whose block table matches.",
                    table.len()
                ),
            },
            // A read error is not the same as "no table": it might be there and
            // unreadable, and generating fresh ids over it would do the same
            // damage.
            Err(e) => panic!("world block table is unreadable: {e}"),
        };
        Self {
            storage,
            journal,
            generator,
            registry: RwLock::new(registry),
            cache: RwLock::new(HashMap::new()),
            dirty: RwLock::new(HashMap::new()),
            edits: AtomicU64::new(0),
            columns: RwLock::new(HashMap::new()),
            sweep: AtomicU64::new(0),
            params: PhysicsParams::default(),
            snapshots: None,
        }
    }

    /// Stamp snapshots with the generator's revision, and rebuild the stale
    /// ones as they load instead of trusting them whole.
    ///
    /// A snapshot holds a whole sub-chunk — the terrain *and* the edits — so
    /// one saved before the generator changed brings the old terrain back
    /// with it, a slab of it between freshly generated neighbours. A stale
    /// snapshot is therefore rebuilt from the fresh generation, keeping only
    /// the blocks `policy.keep` calls player-made, and then the journal is
    /// replayed over it as for any column. The rebuilt section is saved
    /// (stamped) at the next flush. Without a policy, snapshots load whole.
    pub fn with_snapshot_policy(mut self, policy: SnapshotPolicy) -> Self {
        self.snapshots = Some(policy);
        self
    }

    /// Mirror every recorded change into `sink` as well as the journal.
    ///
    /// Builder-style and consuming, so a world either has a mirror from the
    /// moment it exists or never does — attaching one to a running world would
    /// leave a gap at the front that nothing records.
    pub fn with_journal_sink(mut self, sink: aether_world::journal::BatchingSink) -> Self {
        self.journal = self.journal.with_sink(sink);
        self
    }

    /// Override the physics tuning used by [`World::step_body`].
    pub fn with_physics(mut self, params: PhysicsParams) -> Self {
        self.params = params;
        self
    }

    /// The properties of a block id.
    pub fn props_of(&self, id: BlockStateId) -> BlockProperties {
        self.registry.read().unwrap().props_of(id)
    }

    /// The id of `name`, if the registry already knows it.
    pub fn block_id(&self, name: &str) -> Option<BlockStateId> {
        self.registry.read().unwrap().get(name)
    }

    /// The id of `name`, assigning a fresh one if this is the first sighting.
    ///
    /// New ids are appended, never renumbered, and the whole table is written
    /// out by [`World::flush`], so a saved world always reads back with the
    /// same ids it was written with.
    pub fn intern_block(&self, name: &str) -> (BlockStateId, BlockProperties) {
        self.registry.write().unwrap().intern_full(name)
    }

    /// Every block name the registry holds, in id order.
    pub fn block_names(&self) -> Vec<String> {
        self.registry.read().unwrap().names().to_vec()
    }

    /// Ensure the chunk column `(cx, cz)` is present in the cache, loading it
    /// from storage or generating it on first touch.
    ///
    /// Returns the column's pin: hold it while reading or writing the cache,
    /// so [`World::unload`] cannot take the column away in between.
    #[must_use = "the column may be unloaded as soon as the pin is dropped"]
    fn ensure_column(&self, cx: i32, cz: i32) -> Arc<Latch> {
        // Take this column's latch so two workers can't materialize the same
        // `(cx, cz)` concurrently. A second worker must *block* here until the
        // first has populated the cache — returning early on a
        // reserved-but-unpopulated column would hand the caller air for a
        // column that is merely still being generated.
        let found = self.columns.read().unwrap().get(&(cx, cz)).cloned();
        let latch = match found {
            Some(latch) => latch,
            None => {
                let mut columns = self.columns.write().unwrap();
                Arc::clone(columns.entry((cx, cz)).or_insert_with(|| {
                    Arc::new(Latch {
                        materialized: Mutex::new(false),
                        last_used: AtomicU64::new(0),
                    })
                }))
            }
        };
        // Only ever forwards: a touch that read the clock just before a sweep
        // must not overwrite one that read it just after.
        let now = self.sweep.load(Ordering::Relaxed);
        if latch.last_used.load(Ordering::Relaxed) < now {
            latch.last_used.fetch_max(now, Ordering::Relaxed);
        }
        let mut materialized = latch.materialized.lock().unwrap();
        if *materialized {
            drop(materialized);
            return latch;
        }

        // A column is *generated*, then *overlaid*. The generator is
        // deterministic, so its output is a baseline that costs nothing to
        // store and is never written down; only the differences are. A column
        // nobody has touched therefore occupies zero bytes on disk however
        // many players have walked across it.
        let column = self.generator.generate_column(cx, cz);
        {
            let mut cache = self.cache.write().unwrap();
            for (cy, mut sc) in column.sections {
                // Generators build sections block by block, so their palettes
                // carry whatever passed through; the cache holds the minimal
                // form, and nothing at all for a section of air.
                sc.compact();
                let key = SubChunkKey::new(cx, cy, cz);
                if sc.is_empty() {
                    cache.remove(&key);
                } else {
                    cache.insert(key, sc);
                }
            }
        }

        // Distinguish a genuine read error from "nothing stored": on error the
        // column must NOT be latched, or the bare generator output would look
        // authoritative and the next edit would be recorded against the wrong
        // `from` state.
        let mut read_error = false;
        // Set when a stale snapshot is rebuilt: the checkpoint then no longer
        // holds, since edits it vouched for may have been dropped with the old
        // terrain, and the column's whole history is replayed instead.
        let mut rebuilt = false;
        let checkpoint = match self.storage.column_checkpoint(cx, cz) {
            Ok(c) => c.unwrap_or(0),
            Err(_) => {
                read_error = true;
                0
            }
        };

        // The overlay, in two layers. First the snapshots: sub-chunks that
        // have been edited are written out whole, which is the "last state"
        // half of the model and makes loading O(1) in history length.
        for cy in SCAN_CY_MIN..=SCAN_CY_MAX {
            let key = SubChunkKey::new(cx, cy, cz);
            match self.storage.load(key) {
                Ok(Some(sc)) => {
                    let sc = match &self.snapshots {
                        None => sc,
                        Some(policy) => match self.storage.revision(key) {
                            Ok(Some(r)) if r == policy.revision => sc,
                            Ok(_) => {
                                rebuilt = true;
                                self.rebuild_stale(key, &sc, policy)
                            }
                            Err(_) => {
                                read_error = true;
                                sc
                            }
                        },
                    };
                    self.cache.write().unwrap().insert(key, sc);
                }
                Ok(None) => {}
                Err(_) => read_error = true,
            }
        }

        // Then the journal, replayed forwards over the top: the events after
        // the column's checkpoint, which the snapshots may not hold — exactly
        // those of a crash between an edit and the next flush, where the
        // journal is the one that was written synchronously. Replay is what
        // turns "we lost the last thirty seconds" into "we lost nothing", and
        // the checkpoint is what keeps it from re-reading the column's whole
        // history on every load, however old the world.
        let from = if rebuilt { 0 } else { checkpoint };
        match self.journal.column_events_from(cx, cz, from) {
            Ok(events) => {
                for e in events {
                    if let EventBody::BlockSet { x, y, z, to, .. } = e.body {
                        let Some((key, lx, ly, lz)) = Self::key_of(x, y, z) else {
                            continue;
                        };
                        let props = self.props_of(to);
                        self.cache
                            .write()
                            .unwrap()
                            .entry(key)
                            .or_default()
                            .set(lx, ly, lz, to, props);
                    }
                }
            }
            Err(_) => read_error = true,
        }

        // Only latch the column as done once it really is. A read error left
        // it holding nothing but generator output, so a later touch must retry
        // rather than be told the cache is authoritative.
        *materialized = !read_error;
        drop(materialized);
        latch
    }

    /// A stale snapshot rebuilt over the fresh generation: see
    /// [`World::with_snapshot_policy`]. Marked dirty so it is saved, stamped.
    fn rebuild_stale(&self, key: SubChunkKey, old: &SubChunk, policy: &SnapshotPolicy) -> SubChunk {
        let mut fresh = self
            .cache
            .read()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let mut kept = 0;
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let was = old.get(x, y, z);
                    if was != fresh.get(x, y, z) && (policy.keep)(was) {
                        fresh.set(x, y, z, was, self.props_of(was));
                        kept += 1;
                    }
                }
            }
        }
        self.mark_dirty(key);
        (policy.on_rebuild)(key, kept);
        fresh
    }

    /// Mark `key` as holding an edit not yet saved.
    fn mark_dirty(&self, key: SubChunkKey) {
        let n = self.edits.fetch_add(1, Ordering::Relaxed);
        self.dirty.write().unwrap().insert(key, n);
    }

    fn key_of(x: i32, y: i32, z: i32) -> Option<(SubChunkKey, usize, usize, usize)> {
        let (cx, lx) = split(x);
        let (cy, ly) = split(y);
        let (cz, lz) = split(z);
        if cy < i8::MIN as i32 || cy > i8::MAX as i32 {
            return None;
        }
        Some((SubChunkKey::new(cx, cy as i8, cz), lx, ly, lz))
    }

    /// The block id at world coordinates `(x, y, z)` (air if out of range).
    pub fn get_block(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let Some((key, lx, ly, lz)) = Self::key_of(x, y, z) else {
            return BlockStateId::AIR;
        };
        let _pin = self.ensure_column(key.cx, key.cz);
        self.cache
            .read()
            .unwrap()
            .get(&key)
            .map(|sc| sc.get(lx, ly, lz))
            .unwrap_or(BlockStateId::AIR)
    }

    /// Copy a whole section's block ids into `out`, indexed `x | z << 4 |
    /// y << 8`, under a single lock — what a chunk encoder wants instead of
    /// 4096 separate [`World::get_block`] calls, each taking it again.
    pub fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        if cy < i8::MIN as i32 || cy > i8::MAX as i32 {
            out.fill(BlockStateId::AIR);
            return;
        }
        let _pin = self.ensure_column(cx, cz);
        let cache = self.cache.read().unwrap();
        match cache.get(&SubChunkKey::new(cx, cy as i8, cz)) {
            Some(sc) => {
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = sc.get(i & 15, i >> 8, (i >> 4) & 15);
                }
            }
            None => out.fill(BlockStateId::AIR),
        }
    }

    /// The full state name of a block id — properties and all.
    pub fn block_name_of(&self, id: BlockStateId) -> Option<String> {
        self.registry.read().unwrap().name_of(id)
    }

    /// The block name at `(x, y, z)`, if known to the registry.
    pub fn get_block_name(&self, x: i32, y: i32, z: i32) -> Option<String> {
        self.registry
            .read()
            .unwrap()
            .name_of(self.get_block(x, y, z))
    }

    /// Set the block at `(x, y, z)` to a registry block `name`, generating the
    /// column first if needed. Returns the assigned block id.
    /// A name the registry has not seen before is interned rather than
    /// refused, so a player can place any block the client offers.
    pub fn set_block(&self, x: i32, y: i32, z: i32, name: &str) -> Option<BlockStateId> {
        let (id, props) = self.intern_block(name);
        self.set_block_id(x, y, z, id, props);
        Some(id)
    }

    /// Set the block at `(x, y, z)` to an explicit id + properties, with no
    /// entry in the history.
    ///
    /// For changes that have no author and must not be undoable — replaying
    /// the journal itself, or a test fixture. Everything a player does should
    /// go through [`World::set_block_by`] instead, or it cannot be rolled
    /// back and will not appear in any audit.
    pub fn set_block_id(&self, x: i32, y: i32, z: i32, id: BlockStateId, props: BlockProperties) {
        let Some((key, lx, ly, lz)) = Self::key_of(x, y, z) else {
            return;
        };
        // Pinned until the section is marked dirty: a dirty column is never
        // unloaded, and until then the pin is what keeps it.
        let _pin = self.ensure_column(key.cx, key.cz);
        {
            let mut cache = self.cache.write().unwrap();
            cache.entry(key).or_default().set(lx, ly, lz, id, props);
        }
        self.mark_dirty(key);
    }

    /// Set the block at `(x, y, z)` and record who did it.
    ///
    /// The block that was there is read *before* the write and stored in the
    /// event, which is what makes the change undoable without replaying the
    /// world from the beginning. Returns the sequence number of the recorded
    /// event, or `None` when the write was a no-op or out of range.
    ///
    /// Writing a block to the state it already holds records nothing: a
    /// no-op event would still be a real entry in someone's history and would
    /// be undone by a rollback, quietly reverting a *later* edit by someone
    /// else to the same block.
    pub fn set_block_by(
        &self,
        actor: ActorId,
        x: i32,
        y: i32,
        z: i32,
        id: BlockStateId,
        props: BlockProperties,
    ) -> Option<u64> {
        let from = self.get_block(x, y, z);
        if from == id {
            return None;
        }
        self.set_block_id(x, y, z, id, props);
        self.journal
            .append(
                actor,
                EventBody::BlockSet {
                    x,
                    y,
                    z,
                    from,
                    to: id,
                },
            )
            .ok()
    }

    /// Borrow the world's history.
    pub fn journal(&self) -> &Journal<Arc<B>> {
        &self.journal
    }

    /// Undo everything `filter` selects, attributing the undo to `by`.
    ///
    /// Returns the blocks that changed, in the order they were applied. The
    /// undo is itself appended to the history rather than erasing what it
    /// undoes: a rollback that deleted its own evidence could not be
    /// reviewed, could not be undone, and would let a rollback command become
    /// the tidiest way to hide an exploit.
    pub fn rollback(&self, by: ActorId, filter: &Filter) -> Result<Vec<Restore>, StorageError> {
        self.rollback_where(by, filter, &|_| true)
    }

    /// [`World::rollback`] with an extra predicate the journal's own filter
    /// cannot express — a block name, a direction of change.
    ///
    /// Kept as a second entry point rather than folded into [`Filter`] because
    /// the journal must stay able to narrow a scan using its indices, and a
    /// caller-supplied closure is opaque to any index.
    pub fn rollback_where(
        &self,
        by: ActorId,
        filter: &Filter,
        accept: &dyn Fn(&aether_world::journal::Event) -> bool,
    ) -> Result<Vec<Restore>, StorageError> {
        let plan = self.journal.plan_rollback_where(filter, accept)?;
        for r in &plan {
            let props = self.props_of(r.block);
            self.set_block_by(by, r.x, r.y, r.z, r.block, props);
        }
        Ok(plan)
    }

    /// Store a server-level blob alongside the world.
    ///
    /// The world's backend is a plain key/value store shared by the sub-chunks,
    /// the journal and its two indexes, each under its own leading byte. This
    /// lets the layers above — player inventories, for one — use it too. The
    /// caller owns its key space and must not collide with `C`, `E`, `X`, `P`,
    /// `H`, `REG`, or a nine-byte sub-chunk key.
    pub fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.storage.backend().put(key, value)
    }

    /// Read back a blob stored by [`World::put_meta`].
    pub fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.storage.backend().get(key)
    }

    /// Persist every sub-chunk modified since the last flush.
    pub fn flush(&self) -> Result<(), StorageError> {
        // Read before anything is copied: an edit writes the cache before its
        // event is numbered, so every event below this is already in the
        // sections about to be saved. It becomes their columns' checkpoint.
        let head = self.journal.head();
        let marks: Vec<(SubChunkKey, u64)> = self
            .dirty
            .read()
            .unwrap()
            .iter()
            .map(|(&k, &n)| (k, n))
            .collect();
        let keys: Vec<SubChunkKey> = marks.iter().map(|&(k, _)| k).collect();
        {
            let cache = self.cache.read().unwrap();
            for key in &keys {
                if let Some(sc) = cache.get(key) {
                    self.storage.save(*key, sc)?;
                    if let Some(policy) = &self.snapshots {
                        self.storage.save_revision(*key, policy.revision)?;
                    }
                }
            }
        }
        // Every event of a saved column below `head` is in what was just
        // written: an event dirties its section before it is numbered, so its
        // section was in `keys`. Written after the sections, so a crash can
        // lose a checkpoint but never keep one without its snapshots.
        let columns: HashSet<(i32, i32)> = keys.iter().map(|k| (k.cx, k.cz)).collect();
        for (cx, cz) in columns {
            self.storage.save_column_checkpoint(cx, cz, head)?;
        }

        // The name table goes out with the sections, and before the durability
        // flush: the ids just written are meaningless without it.
        self.storage
            .save_registry(self.registry.read().unwrap().names())?;

        // Only commit the durability flush, then clear exactly the marks we
        // saved — not the whole set, and not a key edited again since it was
        // read above — so an edit made while this ran stays dirty (and its
        // column resident) until the next flush, and a failed flush leaves
        // the bookkeeping intact for a retry.
        self.storage.flush()?;
        let mut dirty = self.dirty.write().unwrap();
        for (key, n) in &marks {
            if dirty.get(key) == Some(n) {
                dirty.remove(key);
            }
        }
        Ok(())
    }

    /// A cheap surface-height hint for `(x, z)`: scan down from the top of the
    /// generated band for the first non-air block. Useful for spawning.
    pub fn height_hint(&self, x: i32, z: i32) -> i32 {
        let top = SCAN_CY_MAX as i32 * 16 + 15;
        let bottom = SCAN_CY_MIN as i32 * 16;
        for y in (bottom..=top).rev() {
            if self.get_block(x, y, z) != BlockStateId::AIR {
                return y;
            }
        }
        bottom
    }

    /// Spawn a player-sized [`Body`] with its feet at `(x, y, z)`.
    pub fn spawn_player(&self, x: f64, y: f64, z: f64) -> Body {
        Body::player(Vec3::new(x, y, z))
    }

    /// Advance a physical body one tick against this world's blocks.
    pub fn step_body(&self, body: &mut Body) {
        step(self, body, self.params);
    }

    /// Advance a body one tick with its own physics constants — an item
    /// falls at half a player's gravity, and a client simulating it with
    /// vanilla's numbers drifts from a server using any others.
    pub fn step_body_with(&self, body: &mut Body, params: PhysicsParams) {
        step(self, body, params);
    }

    /// The number of sub-chunks currently resident in the cache.
    pub fn resident_sections(&self) -> usize {
        self.cache.read().unwrap().len()
    }

    /// The number of columns currently resident.
    pub fn resident_columns(&self) -> usize {
        self.columns.read().unwrap().len()
    }

    /// Bytes held by the resident sub-chunks, their keys included.
    ///
    /// Walks the whole cache under its read lock: for a status line, not for
    /// every tick.
    pub fn resident_bytes(&self) -> usize {
        let cache = self.cache.read().unwrap();
        let per_slot = std::mem::size_of::<(SubChunkKey, SubChunk)>() + 8;
        cache
            .values()
            .map(|sc| sc.memory_bytes() - std::mem::size_of::<SubChunk>() + per_slot)
            .sum()
    }

    /// Drop columns from memory: those `keep` does not want and that have
    /// not been touched for `idle_sweeps` calls of this, then — while more
    /// than `budget` columns remain resident (`0` for no limit) — the least
    /// recently used of the rest.
    ///
    /// A column is only ever unloaded clean: one with an unsaved edit stays
    /// until [`World::flush`] has written it, and one being read or written
    /// right now (see [`World::ensure_column`]'s pin) stays too. Unloading is
    /// therefore invisible except in time: a later touch loads the column
    /// again exactly as it was — generator output, snapshots, then the
    /// journal after its checkpoint.
    ///
    /// Each call is one sweep of the clock columns are stamped with, so
    /// `idle_sweeps` counts calls: call it on a fixed period.
    pub fn unload(
        &self,
        keep: &dyn Fn(i32, i32) -> bool,
        budget: usize,
        idle_sweeps: u64,
    ) -> UnloadStats {
        let now = self.sweep.fetch_add(1, Ordering::Relaxed) + 1;
        let idle_long = |used: u64| now.saturating_sub(used) > idle_sweeps;
        // `keep` is the caller's and may call back into the world, so it is
        // asked before any lock of ours is held.
        let unneeded: HashSet<(i32, i32)> = {
            let seen: Vec<((i32, i32), u64)> = self
                .columns
                .read()
                .unwrap()
                .iter()
                .map(|(&col, latch)| (col, latch.last_used.load(Ordering::Relaxed)))
                .collect();
            seen.into_iter()
                .filter(|&((cx, cz), used)| idle_long(used) && !keep(cx, cz))
                .map(|(col, _)| col)
                .collect()
        };
        let mut columns = self.columns.write().unwrap();
        let dirty: HashSet<(i32, i32)> = self
            .dirty
            .read()
            .unwrap()
            .keys()
            .map(|k| (k.cx, k.cz))
            .collect();

        // Columns that could go: nobody holds them, nothing unsaved in them.
        // A latch only the map holds cannot gain a holder while we hold the
        // map's write lock: every clone is taken under one of its locks.
        let mut idle: Vec<((i32, i32), u64)> = columns
            .iter()
            .filter(|(col, latch)| Arc::strong_count(latch) == 1 && !dirty.contains(col))
            .map(|(&col, latch)| (col, latch.last_used.load(Ordering::Relaxed)))
            .collect();

        let mut gone = Vec::new();
        let mut stats = UnloadStats::default();
        idle.retain(|&(col, used)| {
            // Idle checked again: the column may have been touched since.
            if unneeded.contains(&col) && idle_long(used) {
                gone.push(col);
                stats.unneeded += 1;
                false
            } else {
                true
            }
        });
        let resident = columns.len() - gone.len();
        if budget > 0 && resident > budget {
            idle.sort_by_key(|&(_, used)| used);
            for &(col, used) in idle.iter().take(resident - budget) {
                // Never one touched since this sweep began.
                if used >= now {
                    break;
                }
                gone.push(col);
                stats.over_budget += 1;
            }
        }

        if !gone.is_empty() {
            for col in &gone {
                columns.remove(col);
            }
            // Every section of each, the scanned band and any written outside
            // it alike.
            let gone_set: HashSet<(i32, i32)> = gone.iter().copied().collect();
            self.cache
                .write()
                .unwrap()
                .retain(|k, _| !gone_set.contains(&(k.cx, k.cz)));
        }
        // Still under the map's lock: a column touched again cannot start
        // loading — and have the generator remember it afresh — until the
        // generator has forgotten the old one.
        for &(cx, cz) in &gone {
            self.generator.forget(cx, cz);
        }
        stats.resident = columns.len();
        stats
    }
}

/// Blocks with the `collision` property act as solid unit cubes for physics.
impl<B: KvBackend, G: ChunkGenerator> BlockView for World<B, G> {
    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let Some((key, lx, ly, lz)) = Self::key_of(x, y, z) else {
            return false;
        };
        let _pin = self.ensure_column(key.cx, key.cz);
        let cache = self.cache.read().unwrap();
        match cache.get(&key) {
            Some(sc) => sc.props(lx, ly, lz).collision,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_worldgen::{FlatGenerator, NoiseGenerator};

    /// A scratch directory unique to this test binary and `tag`.
    #[cfg(feature = "fjall")]
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aether-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn an_interned_block_keeps_its_identity_across_a_restart() {
        // Ids past the seeded set are handed out in first-seen order, and the
        // save stores those ids raw. Reopening interns in whatever order the
        // new session touches names, so without the persisted table the same
        // id would name a different block. Two names, interned in a known
        // order, then read back through the *names* rather than the ids.
        let dir = scratch_dir("registry");
        {
            let world = World::new(FjallStore::open(&dir).unwrap(), FlatGenerator::classic());
            world
                .set_block(1, 20, 1, "minecraft:diamond_block")
                .unwrap();
            world.set_block(2, 20, 2, "minecraft:oak_planks").unwrap();
            world.flush().unwrap();
        }
        {
            let world = World::new(FjallStore::open(&dir).unwrap(), FlatGenerator::classic());
            // Touch the second name first: a fresh registry would give it the
            // id the first one holds on disk.
            let _ = world.intern_block("minecraft:oak_planks");
            assert_eq!(
                world.get_block_name(1, 20, 1).as_deref(),
                Some("minecraft:diamond_block")
            );
            assert_eq!(
                world.get_block_name(2, 20, 2).as_deref(),
                Some("minecraft:oak_planks")
            );
        }
    }

    #[test]
    fn every_vanilla_block_is_known_without_being_placed_first() {
        // The registry seeds the whole vanilla set, so a player can be handed
        // any block and the world already knows what it is — no first-sighting
        // interning, and no properties guessed from the name.
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        for name in [
            "minecraft:diamond_block",
            "minecraft:glass",
            "minecraft:oak_slab",
            "minecraft:glowstone",
        ] {
            assert!(world.block_id(name).is_some(), "{name} should be known");
        }
        // ...and it knows the real properties, not a guess from the name.
        let glass = world.props_of(world.block_id("minecraft:glass").unwrap());
        assert!(glass.collision && !glass.solid);
        let glow = world.props_of(world.block_id("minecraft:glowstone").unwrap());
        assert_eq!(glow.light_emission, 15);
    }

    #[test]
    fn a_name_outside_the_vanilla_set_is_interned_rather_than_refused() {
        // A modded client can still offer something this build has never
        // heard of, and refusing it would be worse than assuming it is a
        // block.
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        assert!(world.block_id("modid:fancy_block").is_none());
        let id = world.set_block(0, 20, 0, "modid:fancy_block");
        assert!(id.is_some(), "an unseen name must not be refused");
        assert_eq!(
            world.get_block_name(0, 20, 0).as_deref(),
            Some("modid:fancy_block")
        );
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn edits_survive_a_restart() {
        // The point of a persistent backend: what a player builds is still
        // there next time the server starts.
        let dir = scratch_dir("restart");
        {
            let world = World::new(FjallStore::open(&dir).unwrap(), FlatGenerator::classic());
            assert_eq!(world.get_block(3, 3, 3), block_ids::GRASS_BLOCK);
            world.set_block(3, 20, 3, "minecraft:stone").unwrap();
            // Dig out a generated block too: a removal has to persist just as
            // a placement does.
            world.set_block(3, 3, 3, "minecraft:air").unwrap();
            world.flush().unwrap();
        }
        {
            let world = World::new(FjallStore::open(&dir).unwrap(), FlatGenerator::classic());
            assert_eq!(
                world.get_block(3, 20, 3),
                block_ids::STONE,
                "a placed block must survive a restart"
            );
            assert_eq!(
                world.get_block(3, 3, 3),
                BlockStateId::AIR,
                "a dug block must not grow back"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn a_half_written_column_is_completed_not_left_holed() {
        // Regression: completion used to be inferred from "did any section come
        // back from storage". A column whose first save was interrupted then
        // counted as finished, so the sections that never made it to disk were
        // never generated either — holes in the world. Simulate exactly that:
        // seed one mid-column section and nothing else, with no marker.
        use aether_world::{BlockProperties, SubChunk, SubChunkKey, WorldStorage};

        let dir = scratch_dir("partial");
        let marker = BlockStateId(9_999);
        {
            let storage = WorldStorage::new(FjallStore::open(&dir).unwrap());
            let mut sc = SubChunk::new();
            sc.set(1, 2, 3, marker, BlockProperties::SOLID);
            storage.save(SubChunkKey::new(0, 3, 0), &sc).unwrap();
            storage.flush().unwrap();
        }

        let world = World::new(FjallStore::open(&dir).unwrap(), NoiseGenerator::new(42));
        assert_eq!(
            world.get_block(0, 0, 0),
            block_ids::BEDROCK,
            "the rest of the column must be generated, not left as air"
        );
        assert_eq!(
            world.get_block(1, 3 * 16 + 2, 3),
            marker,
            "the section that did reach disk must not be overwritten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generation_is_unbounded_horizontally() {
        // Terrain must exist as far out as the coordinate space goes, in every
        // quadrant — nothing about the world itself stops at a boundary.
        let world = World::new(MemStore::new(), NoiseGenerator::new(42));
        for (x, z) in [
            (0, 0),
            (5_000, 5_000),
            (-5_000, -5_000),
            (1_000_000, -1_000_000),
            (-30_000_000, 30_000_000),
        ] {
            assert_eq!(
                world.get_block(x, 0, z),
                block_ids::BEDROCK,
                "no bedrock at ({x}, {z})"
            );
        }
    }

    #[test]
    fn concurrent_first_touch_never_reads_air() {
        // Regression: `ensure_column` used to reserve a column and return, so a
        // second thread touching the same column while the first was still
        // generating read straight out of the not-yet-populated cache and got
        // air — whole chunks reached clients empty. Every column has bedrock at
        // y=0, so a concurrent hammer must never see anything else there.
        let world = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let world = Arc::clone(&world);
                std::thread::spawn(move || {
                    for cx in 0..16 {
                        for cz in 0..16 {
                            assert_eq!(
                                world.get_block(cx * 16, 0, cz * 16),
                                block_ids::BEDROCK,
                                "column ({cx}, {cz}) read as air mid-generation"
                            );
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn flat_world_reads_generated_blocks() {
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        // Classic flat: bedrock@0, dirt@1-2, grass@3, air above.
        assert_eq!(world.get_block(0, 0, 0), block_ids::BEDROCK);
        assert_eq!(world.get_block(5, 3, 9), block_ids::GRASS_BLOCK);
        assert_eq!(world.get_block(0, 10, 0), BlockStateId::AIR);
    }

    #[test]
    fn set_block_round_trips_and_persists() {
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        world.set_block(2, 20, 3, "minecraft:stone").unwrap();
        assert_eq!(world.get_block(2, 20, 3), block_ids::STONE);
        world.flush().unwrap();
        // A fresh world over the same backing store... (MemStore is owned, so
        // just confirm the block survived a flush within this world).
        assert_eq!(world.get_block(2, 20, 3), block_ids::STONE);
    }

    #[test]
    fn player_falls_onto_flat_ground() {
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        // Grass surface at y=3 -> top face at y=4.
        let mut body = world.spawn_player(0.5, 40.0, 0.5);
        for _ in 0..400 {
            world.step_body(&mut body);
        }
        assert!(body.on_ground, "player should land");
        assert!(
            (body.feet().y - 4.0).abs() < 1.0e-6,
            "feet at {}",
            body.feet().y
        );
    }

    #[test]
    fn is_solid_reflects_collision_property() {
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        assert!(world.is_solid(0, 0, 0)); // bedrock
        assert!(!world.is_solid(0, 10, 0)); // air
    }

    #[test]
    fn negative_coordinates_split_correctly() {
        let world = World::new(MemStore::new(), NoiseGenerator::new(1));
        // Should not panic and should be deterministic for the same column.
        let a = world.get_block(-17, 64, -33);
        let b = world.get_block(-17, 64, -33);
        assert_eq!(a, b);
    }

    #[test]
    fn height_hint_finds_surface() {
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        assert_eq!(world.height_hint(0, 0), 3); // grass on top of the flat stack
    }
}
