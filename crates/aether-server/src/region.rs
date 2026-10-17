//! The fast spatial index behind region / claim protection.
//!
//! A region (WorldGuard-style region, GriefPrevention-style claim) is a cuboid
//! in one world. Every protection check asks the same question — "which
//! regions cover this block?" — on the hot path, for every block break and
//! place, entity interaction and item use. The index in this module answers it
//! without allocating, without taking a lock and without scanning the region
//! set.
//!
//! ## Design: per-chunk bucket, not an R-tree
//!
//! The brief allowed a per-chunk bucket **or** an R-tree. This is the bucket,
//! and the reason is the query shape:
//!
//! * A point query only ever needs the regions covering **one** 16×16 chunk
//!   column, and a chunk is small enough that the number of regions in it is
//!   tiny — a handful in practice, the "k" of the O(k) result.
//! * A bucket is a plain hash lookup: expected `O(1 + k)`, which is *better*
//!   than the R-tree's `O(log n + k)` the brief asked for, and the constant is
//!   a single hash of two `i32`s. An R-tree would spend its log-factor walking
//!   nodes whose bounding boxes mostly do not contain the point, for no gain at
//!   this arity.
//! * The bucket is trivially rebuildable from a flat region list, which is what
//!   the snapshot-swap update model needs; an R-tree rebuild would have to be
//!   incremental to pay off, and updates here are admin-rare.
//!
//! A region that lies on a chunk border is entered into **every** chunk it
//! touches (`cx` spans `min.x.div_euclid(16)..=max.x.div_euclid(16)`, likewise
//! `cz`; `y` is not bucketed). A query filters the bucket down to the regions
//! whose full box actually contains the point, so the over-approximation of the
//! horizontal bucket costs only a couple of integer comparisons per candidate,
//! never a false hit.
//!
//! ## Snapshot reads, off-tick writes
//!
//! [`RegionIndex`] is an immutable, fully built index. [`RegionStore`] holds the
//! current one behind an `Arc`. A check calls [`RegionStore::snapshot`] — one
//! brief read lock, just long enough to bump the refcount — and then queries
//! the returned `Arc<RegionIndex>`, which is immutable and therefore lock-free
//! and allocation-free. An update (add / remove / move a region) builds a fresh
//! index **outside** the read lock and swaps the pointer in under a brief write
//! lock, so a reader always sees a whole index and never a half-built one. A
//! separate builder `Mutex` serialises writers without blocking readers for the
//! length of a rebuild.
//!
//! This mirrors the permission store's snapshot discipline (CMD-01/CMD-02); see
//! [`crate::permissions`]. An update takes effect at the *next* snapshot: the
//! `Arc` a caller already holds keeps answering from the old, consistent index
//! until it asks again, which is what makes "create/move takes effect at the
//! next safe point, never mid-operation" true.
//!
//! ## Module home
//!
//! The index lives in the core rather than in `aether-api`, following CMD-01's
//! precedent: the only consumer today is the core protection check (CMD-29b) and
//! the command layer. The model types here are shaped so that moving them behind
//! a plugin-boundary crate (`aether-api` / a service) is mechanical once the
//! regions plugin (CMD-34/CMD-35) needs to insert regions; see
//! `docs/tasks/CMD-29-region-index.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

/// A block cell in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    /// The X cell.
    pub x: i32,
    /// The Y cell.
    pub y: i32,
    /// The Z cell.
    pub z: i32,
}

impl BlockPos {
    /// A block cell.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        BlockPos { x, y, z }
    }

    /// The chunk column this cell falls in (the bucket key).
    pub const fn chunk(self) -> ChunkPos {
        ChunkPos {
            x: self.x >> 4,
            z: self.z >> 4,
        }
    }
}

/// A chunk column key: block coordinates shifted right by four.
///
/// Only `x`/`z` are part of the key; regions are bucketed per chunk column,
/// full height, and the exact `y` span is checked on the query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
    /// The chunk X (block x `>> 4`).
    pub x: i32,
    /// The chunk Z (block z `>> 4`).
    pub z: i32,
}

impl ChunkPos {
    /// A chunk column.
    pub const fn new(x: i32, z: i32) -> Self {
        ChunkPos { x, z }
    }

    /// The chunk column containing `pos`.
    pub const fn of(pos: BlockPos) -> Self {
        pos.chunk()
    }
}

/// A stable region identifier, unique within a store.
///
/// The number is chosen by the caller (the regions plugin persists it), and it
/// is the tie-break for equal priorities, so it must be stable across restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionId(pub u64);

impl RegionId {
    /// A region id.
    pub const fn new(raw: u64) -> Self {
        RegionId(raw)
    }

    /// The raw number.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for RegionId {
    fn from(raw: u64) -> Self {
        RegionId(raw)
    }
}

/// Per-region flag values the check (CMD-29b) consults.
///
/// The index only carries them: flag *names* are opaque here so the plugin can
/// register its own (CMD-34) without a core enum. An absent flag is "unset" —
/// the check falls back to the built-in default for the action — and a present
/// one is the region's allow/deny. Values beyond a boolean (a greeting, a
/// `set`-style flag) are a CMD-29b/d design decision, not the index's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlagSet {
    values: BTreeMap<String, bool>,
}

impl FlagSet {
    /// No flags set (every action falls back to its default).
    pub fn new() -> Self {
        FlagSet::default()
    }

    /// Set `flag` to `allowed`, builder-style.
    pub fn with(mut self, flag: impl Into<String>, allowed: bool) -> Self {
        self.set(flag, allowed);
        self
    }

    /// Set (or replace) `flag`.
    pub fn set(&mut self, flag: impl Into<String>, allowed: bool) {
        self.values.insert(flag.into(), allowed);
    }

    /// The value of `flag`, or `None` when unset.
    pub fn get(&self, flag: &str) -> Option<bool> {
        self.values.get(flag).copied()
    }

    /// Whether `flag` is set at all.
    pub fn contains(&self, flag: &str) -> bool {
        self.values.contains_key(flag)
    }

    /// How many flags are set.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no flag is set.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Every set flag, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, bool)> {
        self.values
            .iter()
            .map(|(name, allowed)| (name.as_str(), *allowed))
    }
}

/// A cuboid region in one world.
///
/// `min`/`max` are inclusive cells; a point is inside when it lies within all
/// three axes. The box is world-scoped: the index is world-agnostic (the query
/// takes only a position) and the check filters by [`Region::world`].
#[derive(Clone, Debug)]
pub struct Region {
    /// The stable id.
    pub id: RegionId,
    /// The world (dimension) key this region belongs to.
    pub world: String,
    /// The inclusive minimum corner.
    pub min: BlockPos,
    /// The inclusive maximum corner.
    pub max: BlockPos,
    /// Higher wins when regions overlap; the check (CMD-29b) applies it.
    pub priority: i32,
    /// The region's flags.
    pub flags: FlagSet,
    /// Player names added as members (build rights, WorldGuard-style).
    pub members: BTreeSet<String>,
    /// Player names added as owners (full rights).
    pub owners: BTreeSet<String>,
}

impl Region {
    /// A region with default priority, no flags and no members/owners.
    pub fn new(id: RegionId, world: impl Into<String>, min: BlockPos, max: BlockPos) -> Self {
        Region {
            id,
            world: world.into(),
            min,
            max,
            priority: 0,
            flags: FlagSet::new(),
            members: BTreeSet::new(),
            owners: BTreeSet::new(),
        }
    }

    /// Set the priority, builder-style.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Set one flag, builder-style.
    pub fn with_flag(mut self, flag: impl Into<String>, allowed: bool) -> Self {
        self.flags.set(flag, allowed);
        self
    }

    /// Put this region behind an `Arc` — the form the index stores.
    pub fn handle(self) -> RegionHandle {
        Arc::new(self)
    }

    /// Whether `pos` lies within the box, on every axis.
    ///
    /// A box with `min` past `max` on an axis is empty and contains nothing.
    pub fn contains(&self, pos: BlockPos) -> bool {
        self.min.x <= pos.x
            && pos.x <= self.max.x
            && self.min.y <= pos.y
            && pos.y <= self.max.y
            && self.min.z <= pos.z
            && pos.z <= self.max.z
    }

    /// Every chunk column this region touches, in row-major order.
    ///
    /// A region exactly on a border touches both chunks: `x = 0..=16` spans
    /// chunk 0 and chunk 1. An empty (inverted) box has no chunks.
    pub fn chunks(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        let x0 = self.min.x.div_euclid(16);
        let x1 = self.max.x.div_euclid(16);
        let z0 = self.min.z.div_euclid(16);
        let z1 = self.max.z.div_euclid(16);
        (x0..=x1).flat_map(move |cx| (z0..=z1).map(move |cz| ChunkPos::new(cx, cz)))
    }
}

/// A cheap-to-clone, shared reference to a [`Region`].
///
/// The index hands these out by reference; cloning one bumps a refcount, and
/// the region data behind it is immutable once built.
pub type RegionHandle = Arc<Region>;

/// An immutable, fully built region index: per-chunk column → covering regions.
///
/// Build one with [`RegionIndex::build`] (or let a [`RegionStore`] do it on an
/// update) and query it with [`RegionIndex::overlapping`]. It is not mutated
/// after construction, so a query needs no lock and allocates nothing.
#[derive(Default)]
pub struct RegionIndex {
    /// Chunk column → the regions covering it, sorted `(priority desc, id asc)`
    /// and frozen into a shared slice.
    buckets: HashMap<ChunkPos, Arc<[RegionHandle]>>,
    /// Every region by id, for lookup and for rebuilding on an update.
    regions: HashMap<RegionId, RegionHandle>,
}

impl RegionIndex {
    /// An index with no regions.
    pub fn empty() -> Self {
        RegionIndex::default()
    }

    /// Build an index over `regions`.
    ///
    /// A duplicate [`RegionId`] keeps the last handle seen. The build is
    /// `O(total chunk coverage)`: every region is entered into every chunk it
    /// touches, then each bucket is sorted by `(priority desc, id asc)` so
    /// [`RegionIndex::overlapping`] yields the regions already in the order the
    /// check wants to consider them.
    pub fn build(regions: impl IntoIterator<Item = RegionHandle>) -> Self {
        let regions: HashMap<RegionId, RegionHandle> = regions
            .into_iter()
            .map(|handle| (handle.id, handle))
            .collect();
        RegionIndex::from_map(regions)
    }

    /// Build an index directly from owned regions.
    pub fn from_regions(regions: impl IntoIterator<Item = Region>) -> Self {
        RegionIndex::build(regions.into_iter().map(Region::handle))
    }

    /// Build the buckets from an id-keyed region map.
    fn from_map(regions: HashMap<RegionId, RegionHandle>) -> Self {
        let mut buckets: HashMap<ChunkPos, Vec<RegionHandle>> = HashMap::new();
        for handle in regions.values() {
            for chunk in handle.chunks() {
                buckets.entry(chunk).or_default().push(Arc::clone(handle));
            }
        }
        let buckets: HashMap<ChunkPos, Arc<[RegionHandle]>> = buckets
            .into_iter()
            .map(|(chunk, mut covering)| {
                // Higher priority first; ties by id so the order is total and
                // stable regardless of hash-map iteration order.
                covering.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
                (chunk, Arc::from(covering.into_boxed_slice()))
            })
            .collect();
        RegionIndex { buckets, regions }
    }

    /// The regions whose box contains `pos`, highest priority first.
    ///
    /// Expected `O(1 + k)` — one hash lookup plus a comparison per candidate in
    /// the chunk — with no allocation, no lock and no scan of the region set.
    /// The bucket is an over-approximation on the vertical axis; the exact
    /// containment test filters it here, so every returned region really does
    /// contain `pos`.
    pub fn overlapping(&self, pos: BlockPos) -> impl Iterator<Item = &RegionHandle> + '_ {
        self.overlapping_chunk(pos.chunk())
            .filter(move |handle| handle.contains(pos))
    }

    /// The bucket for `chunk`: every region covering that chunk column, whether
    /// or not its `y` span covers any particular block. Sorted by priority.
    ///
    /// An untracked chunk (one no region touches, including a chunk far from
    /// any region) yields an empty iterator rather than panicking.
    pub fn overlapping_chunk(&self, chunk: ChunkPos) -> impl Iterator<Item = &RegionHandle> + '_ {
        self.buckets.get(&chunk).into_iter().flat_map(|b| b.iter())
    }

    /// The region with this id, if any.
    pub fn get(&self, id: RegionId) -> Option<&RegionHandle> {
        self.regions.get(&id)
    }

    /// Every indexed region, in unspecified id order.
    pub fn regions(&self) -> impl Iterator<Item = &RegionHandle> + '_ {
        self.regions.values()
    }

    /// How many regions are indexed.
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Whether no region is indexed.
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// How many chunk columns carry at least one region.
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

/// The live region set behind an atomically swapped snapshot.
///
/// Readers call [`RegionStore::snapshot`] and query the returned
/// `Arc<RegionIndex>`; writers call [`RegionStore::upsert`],
/// [`RegionStore::remove`] or [`RegionStore::replace_all`], which build a fresh
/// index off the read lock and swap it in. There is no window in which a reader
/// sees a partially updated index, and the hot path never holds a lock across
/// the query.
#[derive(Default)]
pub struct RegionStore {
    /// Serialises builders so two updates cannot base their rebuild on the same
    /// stale snapshot and lose one. Held only by writers.
    builder: Mutex<()>,
    /// The current index. The read lock is held only to clone the `Arc`.
    current: RwLock<Arc<RegionIndex>>,
}

impl RegionStore {
    /// An empty store.
    pub fn new() -> Self {
        RegionStore::default()
    }

    /// The current immutable index. The only lock a reader takes, held just
    /// long enough to bump the refcount.
    pub fn snapshot(&self) -> Arc<RegionIndex> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Add a region, or replace the one with the same id (this is also how a
    /// region is moved/redefined). A new snapshot is published atomically.
    pub fn upsert(&self, region: Region) {
        self.edit(|regions| {
            regions.insert(region.id, Arc::new(region));
        });
    }

    /// Move or redefine a region: [`RegionStore::upsert`] under a name that
    /// says what the caller means.
    pub fn move_region(&self, region: Region) {
        self.upsert(region);
    }

    /// Remove the region with `id`, if present. A missing id is a no-op.
    pub fn remove(&self, id: RegionId) {
        self.edit(|regions| {
            regions.remove(&id);
        });
    }

    /// Replace the whole set in one swap (startup load, `/rg reload`).
    pub fn replace_all(&self, regions: impl IntoIterator<Item = RegionHandle>) {
        let _builder = self.builder.lock().unwrap_or_else(|e| e.into_inner());
        let next = Arc::new(RegionIndex::build(regions));
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = next;
    }

    /// Apply `f` to a copy of the region map, rebuild the index and swap it in.
    ///
    /// The rebuild happens off the read lock, so a large update does not stall
    /// a concurrent query; the swap itself is a pointer write. The `builder`
    /// mutex makes the read-copy-build-swap sequence atomic against other
    /// writers. A reader that took a snapshot before the swap keeps answering
    /// from the old index until it asks again.
    fn edit(&self, f: impl FnOnce(&mut HashMap<RegionId, RegionHandle>)) {
        let _builder = self.builder.lock().unwrap_or_else(|e| e.into_inner());
        let mut regions = self
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .regions
            .clone();
        f(&mut regions);
        let next = Arc::new(RegionIndex::from_map(regions));
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(id: u64, min: (i32, i32, i32), max: (i32, i32, i32)) -> Region {
        Region::new(
            RegionId(id),
            "overworld",
            BlockPos::new(min.0, min.1, min.2),
            BlockPos::new(max.0, max.1, max.2),
        )
    }

    #[test]
    fn many_overlapping_regions_are_all_returned() {
        // The check (CMD-29b) decides by priority; the index must not drop any
        // covering region, however many overlap.
        let store = RegionStore::new();
        for id in 0..200u64 {
            store.upsert(region(id, (0, 0, 0), (15, 255, 15)).with_priority((id % 5) as i32));
        }
        let index = store.snapshot();
        let found: Vec<RegionId> = index
            .overlapping(BlockPos::new(8, 64, 8))
            .map(|h| h.id)
            .collect();
        assert_eq!(found.len(), 200);
    }

    #[test]
    fn overlapping_is_ordered_by_priority_then_id() {
        // Deterministic order is what lets the check take the first match and
        // breaks equal-priority ties reproducibly.
        let index = RegionIndex::from_regions([
            region(3, (0, 0, 0), (15, 255, 15)).with_priority(1),
            region(1, (0, 0, 0), (15, 255, 15)).with_priority(1),
            region(2, (0, 0, 0), (15, 255, 15)).with_priority(5),
        ]);
        let order: Vec<u64> = index
            .overlapping(BlockPos::new(0, 0, 0))
            .map(|h| h.id.get())
            .collect();
        assert_eq!(order, vec![2, 1, 3]);
    }

    #[test]
    fn a_region_on_a_chunk_border_is_indexed_in_every_touched_chunk() {
        // `0..=16` crosses the x=16 and z=16 borders: four chunk columns, and a
        // point on either side of the seam finds it.
        let index = RegionIndex::from_regions([region(1, (0, 0, 0), (16, 255, 16))]);
        assert_eq!(index.bucket_count(), 4);
        for pos in [
            BlockPos::new(0, 64, 0),
            BlockPos::new(15, 64, 15),
            BlockPos::new(16, 64, 16),
            BlockPos::new(16, 64, 0),
            BlockPos::new(0, 64, 16),
        ] {
            assert!(
                index.overlapping(pos).any(|h| h.id == RegionId(1)),
                "region not found at {pos:?}"
            );
        }
    }

    #[test]
    fn negative_coordinates_index_into_the_right_chunks() {
        // `div_euclid` (== `>> 4`) must put -1 in chunk -1, not chunk 0.
        let index = RegionIndex::from_regions([region(1, (-1, 0, -1), (0, 255, 0))]);
        assert_eq!(index.bucket_count(), 4);
        assert!(index
            .overlapping(BlockPos::new(-1, 64, -1))
            .any(|h| h.id == RegionId(1)));
        assert!(index
            .overlapping(BlockPos::new(0, 64, 0))
            .any(|h| h.id == RegionId(1)));
        assert!(!index
            .overlapping(BlockPos::new(1, 64, 1))
            .any(|h| h.id == RegionId(1)));
    }

    #[test]
    fn a_point_outside_the_vertical_span_is_not_returned() {
        let index = RegionIndex::from_regions([region(1, (0, 0, 0), (15, 63, 15))]);
        // Same chunk, above the box: the bucket holds it, the filter rejects it.
        assert!(index.overlapping(BlockPos::new(8, 63, 8)).next().is_some());
        assert!(index.overlapping(BlockPos::new(8, 64, 8)).next().is_none());
    }

    #[test]
    fn an_untouched_chunk_returns_an_empty_iterator() {
        // "No region here" must be an empty result, never a panic — including a
        // chunk nobody ever touched.
        let index = RegionIndex::from_regions([region(1, (0, 0, 0), (15, 255, 15))]);
        assert_eq!(
            index
                .overlapping(BlockPos::new(1_000_000, 64, -1_000_000))
                .count(),
            0
        );
        assert_eq!(
            index
                .overlapping_chunk(ChunkPos::new(62_500, -62_500))
                .count(),
            0
        );
    }

    #[test]
    fn an_empty_region_contains_nothing_and_has_no_chunks() {
        // `min` past `max` on a horizontal axis: the chunk range inverts to
        // `cx 1..=0`, which is empty, so it is entered into no bucket at all.
        let inverted = region(1, (20, 0, 20), (5, 255, 5));
        assert!(!inverted.contains(BlockPos::new(10, 64, 10)));
        assert_eq!(inverted.chunks().count(), 0);
    }

    #[test]
    fn an_update_takes_effect_at_the_next_snapshot() {
        // A caller already holding a snapshot keeps seeing the old, consistent
        // index; only a fresh snapshot sees the edit. This is the "safe point".
        let store = RegionStore::new();
        let before = store.snapshot();
        store.upsert(region(1, (0, 0, 0), (15, 255, 15)));

        let after = store.snapshot();
        assert!(before.overlapping(BlockPos::new(8, 8, 8)).next().is_none());
        assert_eq!(after.overlapping(BlockPos::new(8, 8, 8)).count(), 1);
        // The old handle is untouched, not mutated in place.
        assert!(before.overlapping(BlockPos::new(8, 8, 8)).next().is_none());
    }

    #[test]
    fn remove_and_move_change_coverage() {
        let store = RegionStore::new();
        store.upsert(region(1, (0, 0, 0), (15, 255, 15)));
        assert_eq!(
            store.snapshot().overlapping(BlockPos::new(8, 8, 8)).count(),
            1
        );

        store.remove(RegionId(1));
        assert_eq!(
            store.snapshot().overlapping(BlockPos::new(8, 8, 8)).count(),
            0
        );

        // Moving is an upsert under the same id: the old box no longer answers,
        // the new one does.
        store.upsert(region(1, (100, 0, 100), (115, 255, 115)));
        assert_eq!(
            store.snapshot().overlapping(BlockPos::new(8, 8, 8)).count(),
            0
        );
        assert_eq!(
            store
                .snapshot()
                .overlapping(BlockPos::new(108, 8, 108))
                .count(),
            1
        );
    }

    #[test]
    fn replace_all_swaps_the_whole_set_atomically() {
        let store = RegionStore::new();
        store.upsert(region(1, (0, 0, 0), (15, 255, 15)));
        store.replace_all([region(2, (100, 0, 100), (115, 255, 115)).handle()]);
        let index = store.snapshot();
        assert_eq!(index.len(), 1);
        assert_eq!(index.overlapping(BlockPos::new(8, 8, 8)).count(), 0);
        assert_eq!(index.overlapping(BlockPos::new(108, 8, 108)).count(), 1);
    }

    #[test]
    fn a_query_stays_correct_as_the_region_count_grows() {
        // The bench measures the flat cost; this pins the answer at scale so a
        // "fast but wrong" index cannot pass.
        let mut regions: Vec<Region> = (0..10_000u64)
            .map(|id| {
                let x = (id as i32 % 100) * 32;
                let z = (id as i32 / 100) * 32;
                region(id, (x, 0, z), (x + 15, 255, z + 15))
            })
            .collect();
        regions
            .push(region(999_999, (-10_000, 0, -10_000), (10_000, 255, 10_000)).with_priority(100));
        let index = RegionIndex::from_regions(regions);

        // The one big region overlaps every small one at (0, 64, 0).
        let found: Vec<RegionId> = index
            .overlapping(BlockPos::new(0, 64, 0))
            .map(|h| h.id)
            .collect();
        assert!(found.contains(&RegionId(0)));
        assert!(found.contains(&RegionId(999_999)));
        assert_eq!(
            found.first(),
            Some(&RegionId(999_999)),
            "highest priority first"
        );
    }

    #[test]
    fn a_store_is_shareable_across_threads() {
        // The snapshot discipline must be sound under concurrency: a reader
        // hammering the index while a writer swaps it never sees a torn index.
        let store = Arc::new(RegionStore::new());
        store.upsert(region(1, (0, 0, 0), (15, 255, 15)));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        let index = store.snapshot();
                        // Every region covers (8, 8, 8), so a consistent index
                        // must return exactly as many as it holds. A snapshot
                        // whose buckets lag its region map would disagree.
                        assert_eq!(
                            index.overlapping(BlockPos::new(8, 8, 8)).count(),
                            index.len(),
                            "a snapshot saw a half-built index"
                        );
                    }
                })
            })
            .collect();
        let writer = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                for id in 2..200u64 {
                    store.upsert(region(id, (0, 0, 0), (15, 255, 15)));
                }
            })
        };
        for r in readers {
            r.join().unwrap();
        }
        writer.join().unwrap();
    }
}
