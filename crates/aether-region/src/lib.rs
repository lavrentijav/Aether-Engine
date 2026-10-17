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
//! The index lives in this small, dependency-free crate rather than inside
//! `aether-server`, because it has two consumers that must not depend on each
//! other: the server's hot-path protection check (CMD-29b) and the native
//! `plugins/regions` plugin (CMD-34/CMD-35). Both link the same
//! [`RegionStore`], so the plugin edits the one index the check reads. See
//! `docs/tasks/CMD-29-region-index.md` and `docs/tasks/CMD-34a-regions-model.md`.

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

/// The action a protection check is deciding.
///
/// The variants map onto the WorldGuard-style flags the regions plugin
/// (CMD-34) writes: build for [`Action::Break`] and [`Action::Place`], use for
/// [`Action::Interact`] and [`Action::UseItem`], and the action's own name for
/// pvp and explosions. The mapping is data ([`Action::flag`]), not a match in
/// the hot path, so a plugin can add flags without changing the core enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A player breaks a block.
    Break,
    /// A player places a block.
    Place,
    /// A player right-clicks a block or entity (open a chest, use a door).
    Interact,
    /// A player uses an item in the air (eat, drink, throw).
    UseItem,
    /// A player attacks another player.
    Pvp,
    /// An explosion damages blocks.
    Explosion,
    /// A player enters a region (reserved for the teleport `entry` seam).
    Entry,
}

impl Action {
    /// The flag name this action consults.
    pub const fn flag(self) -> &'static str {
        match self {
            Action::Break | Action::Place => "build",
            Action::Interact | Action::UseItem => "use",
            Action::Pvp => "pvp",
            Action::Explosion => "explosion",
            Action::Entry => "entry",
        }
    }

    /// The built-in answer when no covering region states the flag.
    ///
    /// A region is protection by default: with no flag set, breaking, placing,
    /// using and explosions are denied for a non-member, and pvp is denied for
    /// everyone. A region that wants to be permissive sets the flag.
    pub const fn default_allowed(self) -> bool {
        match self {
            Action::Break
            | Action::Place
            | Action::Interact
            | Action::UseItem
            | Action::Pvp
            | Action::Explosion
            | Action::Entry => false,
        }
    }

    /// Whether region membership grants this action when its flag is unset.
    ///
    /// Members and owners may enter, build and use their region; membership is
    /// not a licence to pvp or to explode inside it.
    pub const fn membership_grants(self) -> bool {
        matches!(
            self,
            Action::Break | Action::Place | Action::Interact | Action::UseItem | Action::Entry
        )
    }
}

/// The answer to a protection check.
///
/// [`Verdict::Defer`] is a per-region answer meaning "this region has no policy
/// for the action; let the next region decide". [`RegionIndex::check`] resolves
/// the chain and returns only `Allow` or `Deny`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The action may proceed.
    Allow,
    /// The action is cancelled.
    Deny,
    /// This region does not decide; consult the next one.
    Defer,
}

impl Verdict {
    /// Whether the action may proceed.
    pub const fn allows(self) -> bool {
        matches!(self, Verdict::Allow)
    }
}

/// Who is asking.
///
/// `name` is the player name region membership is matched against (case
/// insensitively); `bypass` is the resolved `aether.region.bypass` permission.
/// A server actor has no name and never bypasses.
#[derive(Clone, Copy, Debug)]
pub struct Actor<'a> {
    /// The player name, empty for the server.
    pub name: &'a str,
    /// Whether the actor holds the bypass permission.
    pub bypass: bool,
}

impl<'a> Actor<'a> {
    /// A player who does not bypass.
    pub const fn new(name: &'a str) -> Self {
        Actor {
            name,
            bypass: false,
        }
    }

    /// A player with an already-resolved bypass decision.
    pub const fn with_bypass(name: &'a str, bypass: bool) -> Self {
        Actor { name, bypass }
    }

    /// The server acting on its own behalf (explosions).
    pub const fn server() -> Self {
        Actor {
            name: "",
            bypass: false,
        }
    }
}

/// The permission node that skips region protection.
pub const BYPASS_PERMISSION: &str = "aether.region.bypass";

/// The dimension key the core's one world uses.
///
/// [`crate::region`] compares world keys with the `minecraft:` namespace
/// ignored, so a plugin may store either `overworld` or `minecraft:overworld`.
pub const OVERWORLD: &str = "minecraft:overworld";

/// Whether two world keys name the same dimension.
///
/// The comparison ignores the `minecraft:` namespace, case and surrounding
/// whitespace, so the core's `minecraft:overworld` matches a plugin's
/// `overworld` / `OVERWORLD`. An empty key matches only an empty key.
pub fn same_world(a: &str, b: &str) -> bool {
    fn normal(world: &str) -> &str {
        let world = world.trim();
        // The namespace is ASCII, so only slice when those bytes really are
        // the prefix: a UTF-8 world key must not be split mid-character.
        match world.get(..10) {
            Some(prefix) if prefix.eq_ignore_ascii_case("minecraft:") => &world[10..],
            _ => world,
        }
    }
    normal(a).eq_ignore_ascii_case(normal(b))
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
    /// Per-player action grants, narrower than membership.
    ///
    /// Each entry maps a player name to the [`Action::flag`] names they may
    /// perform even though they are not a member. This is how the claims
    /// plugin expresses GriefPrevention trust levels: a container-trusted
    /// player gets `use` (open chests) but not `build` (break/place), while a
    /// build-trusted player gets both. An explicit flag still wins, so a
    /// region-wide deny is never overridden by a grant.
    pub trusted: BTreeMap<String, BTreeSet<String>>,
    /// The plugin that owns this region (`claims`, `regions`), or `None` for a
    /// region created directly in the core. The host stamps it on a plugin
    /// write, so one plugin cannot delete another's region.
    pub source: Option<String>,
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
            trusted: BTreeMap::new(),
            source: None,
        }
    }

    /// Set the priority, builder-style.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Set the owning plugin, builder-style.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set one flag, builder-style.
    pub fn with_flag(mut self, flag: impl Into<String>, allowed: bool) -> Self {
        self.flags.set(flag, allowed);
        self
    }

    /// Add an owner, builder-style.
    pub fn owner(mut self, name: impl Into<String>) -> Self {
        self.owners.insert(name.into());
        self
    }

    /// Add a member, builder-style.
    pub fn member(mut self, name: impl Into<String>) -> Self {
        self.members.insert(name.into());
        self
    }

    /// Grant `name` the actions whose [`Action::flag`] names are in `flags`,
    /// builder-style.
    pub fn trusted(
        mut self,
        name: impl Into<String>,
        flags: impl IntoIterator<Item = String>,
    ) -> Self {
        self.trusted
            .insert(name.into(), flags.into_iter().collect());
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

    /// This region's answer for `action`, or [`Verdict::Defer`] when it has no
    /// policy for it.
    ///
    /// An explicit flag decides for everyone; a bypassing actor is handled by
    /// [`RegionIndex::check`] before this is called. With the flag absent a
    /// player with a matching [`Region::trusted`] grant is allowed that single
    /// action, then a member or owner is allowed to build/use, and everything
    /// else defers to the next region (with the action's built-in default as
    /// the final fallback).
    pub fn verdict(&self, action: Action, actor: &Actor<'_>) -> Verdict {
        match self.flags.get(action.flag()) {
            Some(true) => Verdict::Allow,
            Some(false) => Verdict::Deny,
            None if self.is_trusted(actor.name, action.flag()) => Verdict::Allow,
            None if action.membership_grants() && self.is_member(actor.name) => Verdict::Allow,
            None => Verdict::Defer,
        }
    }

    /// Whether `name` has an explicit grant for the action flag `flag`.
    fn is_trusted(&self, name: &str, flag: &str) -> bool {
        !name.is_empty()
            && self
                .trusted
                .iter()
                .any(|(trusted, flags)| trusted.eq_ignore_ascii_case(name) && flags.contains(flag))
    }

    /// Whether `name` is this region's owner or a member.
    pub fn is_member(&self, name: &str) -> bool {
        !name.is_empty()
            && (self.owners.iter().any(|n| n.eq_ignore_ascii_case(name))
                || self.members.iter().any(|n| n.eq_ignore_ascii_case(name)))
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

    /// Every region in `world` whose box overlaps the inclusive box
    /// `min..=max`, in id order (not priority order — callers that need the
    /// protection order run the regular priority chain).
    ///
    /// This is the box query a plugin model uses to refuse an overlapping
    /// claim. It walks only the chunk columns the box covers and deduplicates
    /// the regions that span several of them, so a query costs `O(chunks + k)`
    /// rather than a scan of every region in the process. A region that merely
    /// touches the box on a face is adjacent, not overlapping, and is not
    /// returned.
    ///
    /// A pathologically large box (more than a million chunk columns, which no
    /// gameplay claim is) falls back to scanning the region map so a bad caller
    /// cannot make the query spin over billions of empty buckets.
    pub fn overlapping_box(&self, world: &str, min: BlockPos, max: BlockPos) -> Vec<&RegionHandle> {
        let x0 = min.x.div_euclid(16);
        let x1 = max.x.div_euclid(16);
        let z0 = min.z.div_euclid(16);
        let z1 = max.z.div_euclid(16);
        let columns =
            (i64::from(x1) - i64::from(x0) + 1).saturating_mul(i64::from(z1) - i64::from(z0) + 1);

        let hits = |handle: &&RegionHandle| {
            same_world(&handle.world, world)
                && handle.min.x <= max.x
                && min.x <= handle.max.x
                && handle.min.y <= max.y
                && min.y <= handle.max.y
                && handle.min.z <= max.z
                && min.z <= handle.max.z
        };

        let mut found: BTreeMap<RegionId, &RegionHandle> = BTreeMap::new();
        if columns <= 1_048_576 {
            for cx in x0..=x1 {
                for cz in z0..=z1 {
                    for handle in self.overlapping_chunk(ChunkPos::new(cx, cz)) {
                        if hits(&handle) {
                            found.entry(handle.id).or_insert(handle);
                        }
                    }
                }
            }
        } else {
            for handle in self.regions.values() {
                if hits(&handle) {
                    found.entry(handle.id).or_insert(handle);
                }
            }
        }
        found.into_values().collect()
    }

    /// Decide `action` at `pos` in `world` for `actor`.
    ///
    /// Regions are considered in the index's `(priority desc, id asc)` order.
    /// The first one with an explicit flag (or a per-player grant, or a
    /// membership grant) decides;
    /// [`Verdict::Defer`] moves on to the next. If every covering region
    /// defers, the action's built-in default applies; if no region covers the
    /// position at all, the action is allowed. Only regions whose [`world`]
    /// matches are considered, so the world-agnostic index cannot leak a
    /// protection from another dimension.
    ///
    /// The return is always [`Verdict::Allow`] or [`Verdict::Deny`];
    /// [`Verdict::Defer`] is only a per-region answer (see
    /// [`Region::verdict`]) that this method consumes to move down the chain.
    ///
    /// [`world`]: Region::world
    pub fn check(&self, world: &str, pos: BlockPos, action: Action, actor: &Actor<'_>) -> Verdict {
        if actor.bypass {
            return Verdict::Allow;
        }
        let mut covered = false;
        for region in self.overlapping(pos) {
            if !same_world(&region.world, world) {
                continue;
            }
            covered = true;
            match region.verdict(action, actor) {
                Verdict::Defer => continue,
                verdict => return verdict,
            }
        }
        if covered {
            if action.default_allowed() {
                Verdict::Allow
            } else {
                Verdict::Deny
            }
        } else {
            Verdict::Allow
        }
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
pub struct RegionStore {
    /// Serialises builders so two updates cannot base their rebuild on the same
    /// stale snapshot and lose one. Held only by writers.
    builder: Mutex<()>,
    /// The current index. The read lock is held only to clone the `Arc`.
    current: RwLock<Arc<RegionIndex>>,
    /// The line a denied check reports. A plugin may replace it.
    deny_message: RwLock<Arc<str>>,
}

/// The message a denial reports until a plugin configures another.
pub const DEFAULT_DENY_MESSAGE: &str = "You do not have permission to do that here.";

/// The ownership rule shared by the atomic writers: `None` when `plugin` may
/// write this id, otherwise the reason it may not.
fn ownership_error(
    regions: &HashMap<RegionId, RegionHandle>,
    plugin: &str,
    id: RegionId,
) -> Option<RegionEditError> {
    match regions.get(&id) {
        None => None,
        Some(region) => match region.source.as_deref() {
            Some(source) if source == plugin => None,
            Some(source) => Some(RegionEditError::OwnedByOther(source.to_string())),
            None => Some(RegionEditError::OwnedByCore),
        },
    }
}

/// Why an atomic region write was refused.
///
/// A plugin may only replace or remove a region it owns; a core-created region
/// (`source: None`) and another plugin's region are both protected. The
/// ownership test and the write happen under the store's builder lock, so two
/// writers cannot both pass a check for the same id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegionEditError {
    /// The id belongs to a region created directly in the core.
    OwnedByCore,
    /// The id belongs to another plugin's region.
    OwnedByOther(String),
    /// The id belongs to a region by the same plugin. A *fresh* insert never
    /// reuses an occupied id; only an idempotent replay may replace its own.
    IdTaken,
    /// The new box overlaps an existing region the caller asked to avoid.
    Overlap,
}

impl std::fmt::Display for RegionEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegionEditError::OwnedByCore => {
                write!(f, "the region is owned by the core, not by the plugin")
            }
            RegionEditError::OwnedByOther(source) => {
                write!(f, "the region id belongs to `{source}`")
            }
            RegionEditError::IdTaken => write!(f, "the region id is already in use"),
            RegionEditError::Overlap => write!(f, "the region overlaps an existing region"),
        }
    }
}

impl Default for RegionStore {
    fn default() -> Self {
        RegionStore {
            builder: Mutex::new(()),
            current: RwLock::new(Arc::new(RegionIndex::empty())),
            deny_message: RwLock::new(Arc::from(DEFAULT_DENY_MESSAGE)),
        }
    }
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

    /// Decide `action` against a fresh snapshot. The snapshot lock is released
    /// before the query, so the check never nests with another module's lock.
    pub fn check(&self, world: &str, pos: BlockPos, action: Action, actor: &Actor<'_>) -> Verdict {
        self.snapshot().check(world, pos, action, actor)
    }

    /// The line a denied check reports.
    pub fn deny_message(&self) -> Arc<str> {
        Arc::clone(&self.deny_message.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Replace the denial line (a plugin's `[regions] deny-message`).
    pub fn set_deny_message(&self, message: impl Into<Arc<str>>) {
        *self.deny_message.write().unwrap_or_else(|e| e.into_inner()) = message.into();
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

    /// Replace the region with the same id, but only when `plugin` already
    /// owns it. A free id is inserted. A core-owned region (`source: None`) or
    /// another plugin's region is refused.
    ///
    /// The ownership test and the swap run under the builder lock, so two
    /// writers of the same id cannot both pass it.
    pub fn upsert_owned(&self, plugin: &str, mut region: Region) -> Result<(), RegionEditError> {
        let _builder = self.builder.lock().unwrap_or_else(|e| e.into_inner());
        let current = self
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(error) = ownership_error(&current.regions, plugin, region.id) {
            return Err(error);
        }
        region.source = Some(plugin.to_string());
        let mut regions = current.regions.clone();
        regions.insert(region.id, Arc::new(region));
        let next = Arc::new(RegionIndex::from_map(regions));
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    /// Insert a **fresh** region owned by `plugin`. The id must be completely
    /// free: any existing region with that id — including one this plugin
    /// already owns — is refused with [`RegionEditError::IdTaken`]. The box must
    /// also not overlap an existing region matching `conflict`.
    ///
    /// This is the atomic claim insert: the id and overlap tests and the insert
    /// share the builder lock, so two callers cannot both claim the same land or
    /// the same id. `conflict` sees an existing region and decides whether it
    /// blocks the new one — claims pass "same plugin", so a claim may still
    /// overlap a WorldGuard-style region from another plugin and let the
    /// priority chain decide.
    ///
    /// Unlike [`RegionStore::upsert_owned`], this never replaces anything. Use
    /// `upsert_owned` for an idempotent startup replay.
    pub fn insert_new_owned(
        &self,
        plugin: &str,
        mut region: Region,
        conflict: impl Fn(&Region) -> bool,
    ) -> Result<(), RegionEditError> {
        let _builder = self.builder.lock().unwrap_or_else(|e| e.into_inner());
        let current = self
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if current.regions.contains_key(&region.id) {
            return Err(RegionEditError::IdTaken);
        }
        if current
            .overlapping_box(&region.world, region.min, region.max)
            .into_iter()
            .any(|handle| conflict(handle))
        {
            return Err(RegionEditError::Overlap);
        }
        region.source = Some(plugin.to_string());
        let mut regions = current.regions.clone();
        regions.insert(region.id, Arc::new(region));
        let next = Arc::new(RegionIndex::from_map(regions));
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    /// Remove the region with `id` only when `plugin` owns it. Returns whether
    /// anything was removed; a core-owned or another plugin's region is left
    /// untouched.
    pub fn remove_owned(&self, plugin: &str, id: RegionId) -> bool {
        let _builder = self.builder.lock().unwrap_or_else(|e| e.into_inner());
        let current = self
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if current
            .regions
            .get(&id)
            .and_then(|region| region.source.as_deref())
            != Some(plugin)
        {
            return false;
        }
        let mut regions = current.regions.clone();
        regions.remove(&id);
        let next = Arc::new(RegionIndex::from_map(regions));
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = next;
        true
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

/// The process-wide region set the protection hooks and the regions plugin
/// share.
///
/// The regions plugin (CMD-34) fills it through [`RegionStore::upsert`]; with
/// no plugin the store is empty and every check allows, which is the CMD-29
/// contract that the core carries no policy of its own.
///
/// This is deliberately a small, dependency-free crate: both `aether-server`
/// (the hot-path hooks) and `plugins/regions` (the model and `/rg` commands)
/// depend on it, and neither may depend on the other. The plugin holds the
/// same process-wide store as the server, so it edits the *one* index the
/// check consults rather than a copy.
pub fn store() -> &'static RegionStore {
    #[cfg(feature = "test-store")]
    if let Some(store) = TEST_STORE.with(|slot| *slot.borrow()) {
        return store;
    }
    shared().as_ref()
}

/// The process-wide store as a shared handle, for a plugin that wants to hold
/// it as a field (and for tests that inject their own).
///
/// With the `test-store` feature, a thread-local override installed by
/// [`install_test_global`] wins: a test can give a plugin its own store without
/// passing it through the plugin's constructor, and other tests (on their own
/// threads) keep the real one.
pub fn global() -> Arc<RegionStore> {
    #[cfg(feature = "test-store")]
    if let Some(store) = TEST_GLOBAL.with(|slot| slot.borrow().clone()) {
        return store;
    }
    Arc::clone(shared())
}

fn shared() -> &'static Arc<RegionStore> {
    static STORE: std::sync::OnceLock<Arc<RegionStore>> = std::sync::OnceLock::new();
    STORE.get_or_init(|| Arc::new(RegionStore::new()))
}

// A per-test-thread store override, so a unit test can install a region
// without leaking it into another test running in parallel. Compiled only
// with the `test-store` feature (the server's dev-dependencies enable it), so
// production pays no thread-local read on the hot path.
#[cfg(feature = "test-store")]
thread_local! {
    static TEST_STORE: std::cell::RefCell<Option<&'static RegionStore>> =
        const { std::cell::RefCell::new(None) };
    static TEST_GLOBAL: std::cell::RefCell<Option<Arc<RegionStore>>> =
        const { std::cell::RefCell::new(None) };
}

/// Install a shared store for [`global`] on this test thread only.
///
/// `store()` (the server hooks) still needs [`install_test_store`]; a test that
/// drives a plugin constructed with `Default` uses this one.
#[cfg(feature = "test-store")]
pub fn install_test_global(store: Arc<RegionStore>) {
    TEST_GLOBAL.with(|slot| *slot.borrow_mut() = Some(store));
}

/// Install a store for this test thread only. The caller keeps filling it via
/// the returned handle; other tests and production keep the real store.
#[cfg(feature = "test-store")]
pub fn install_test_store(store: RegionStore) -> &'static RegionStore {
    let store: &'static RegionStore = Box::leak(Box::new(store));
    TEST_STORE.with(|slot| *slot.borrow_mut() = Some(store));
    store
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

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn priority_decides_overlapping_regions() {
        // The higher priority's explicit allow wins over the lower deny.
        let index = RegionIndex::from_regions([
            region(1, (0, 0, 0), (15, 255, 15))
                .with_priority(1)
                .with_flag("build", false),
            region(2, (0, 0, 0), (15, 255, 15))
                .with_priority(5)
                .with_flag("build", true),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Allow
        );

        // ...and the higher priority's deny wins over the lower allow.
        let index = RegionIndex::from_regions([
            region(1, (0, 0, 0), (15, 255, 15))
                .with_priority(1)
                .with_flag("build", true),
            region(2, (0, 0, 0), (15, 255, 15))
                .with_priority(5)
                .with_flag("build", false),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Deny
        );
    }

    #[test]
    fn equal_priority_is_broken_by_id_deterministically() {
        // The index orders equal priorities by ascending id; the smaller id is
        // the one the check sees first, regardless of insertion order.
        let index = RegionIndex::from_regions([
            region(9, (0, 0, 0), (15, 255, 15)).with_flag("build", false),
            region(2, (0, 0, 0), (15, 255, 15)).with_flag("build", true),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Allow,
            "id 2 decides before id 9"
        );

        let index = RegionIndex::from_regions([
            region(2, (0, 0, 0), (15, 255, 15)).with_flag("build", false),
            region(9, (0, 0, 0), (15, 255, 15)).with_flag("build", true),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Deny,
            "the same id order, whatever the insertion order"
        );
    }

    #[test]
    fn an_unset_flag_defers_to_the_next_region() {
        // The top region has a flag for another action, so it defers on build;
        // the middle region allows build; the bottom deny is never reached.
        let top = region(3, (0, 0, 0), (15, 255, 15))
            .with_priority(10)
            .with_flag("pvp", false);
        assert_eq!(
            top.verdict(Action::Break, &Actor::new("steve")),
            Verdict::Defer
        );
        let index = RegionIndex::from_regions([
            top,
            region(2, (0, 0, 0), (15, 255, 15))
                .with_priority(5)
                .with_flag("build", true),
            region(1, (0, 0, 0), (15, 255, 15))
                .with_priority(0)
                .with_flag("build", false),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Allow
        );

        // With the middle region also deferring, the bottom deny decides.
        let index = RegionIndex::from_regions([
            region(3, (0, 0, 0), (15, 255, 15))
                .with_priority(10)
                .with_flag("pvp", true),
            region(2, (0, 0, 0), (15, 255, 15)).with_priority(5),
            region(1, (0, 0, 0), (15, 255, 15))
                .with_priority(0)
                .with_flag("build", false),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Deny
        );
    }

    #[test]
    fn only_regions_in_the_queried_world_apply() {
        let index = RegionIndex::from_regions([
            region(1, (0, 0, 0), (15, 255, 15)).with_flag("build", false),
            Region::new(
                RegionId(2),
                "the_nether",
                BlockPos::new(0, 0, 0),
                BlockPos::new(15, 255, 15),
            )
            .with_flag("build", true),
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Deny
        );
        assert_eq!(
            index.check(
                "the_nether",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Allow
        );
        // The namespace is not part of the identity.
        assert!(same_world(OVERWORLD, "overworld"));
        assert!(same_world("minecraft:overworld", "OVERWORLD"));
        assert!(same_world("Minecraft:overworld", "overworld"));
        assert!(!same_world(OVERWORLD, "the_nether"));
        assert!(!same_world("", OVERWORLD));
    }

    #[test]
    fn a_position_no_region_covers_is_allowed() {
        let index = RegionIndex::from_regions([region(1, (0, 0, 0), (15, 255, 15))]);
        assert_eq!(
            index.check(
                "overworld",
                at(10_000, 64, 10_000),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_protected_position_denies_even_when_its_chunk_is_not_loaded() {
        // The index is world-agnostic and does not read the world, so a
        // chunked-out block is still protected: the answer comes from the
        // region, never from a failed block read.
        let far = at(1_000_000, 64, -1_000_000);
        let index = RegionIndex::from_regions([region(
            1,
            (far.x - 4, 0, far.z - 4),
            (far.x + 4, 255, far.z + 4),
        )
        .with_flag("build", false)]);
        assert_eq!(
            index.check("overworld", far, Action::Break, &Actor::new("steve")),
            Verdict::Deny
        );
        assert_eq!(
            index.check(
                "overworld",
                at(far.x + 100, 64, far.z),
                Action::Break,
                &Actor::new("steve")
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn members_and_owners_may_build_but_an_explicit_deny_still_wins() {
        let region = region(1, (0, 0, 0), (15, 255, 15))
            .owner("Owner")
            .member("Builder");
        let index = RegionIndex::from_regions([region.clone()]);
        for name in ["Owner", "Builder", "builder"] {
            assert_eq!(
                index.check("overworld", at(8, 64, 8), Action::Break, &Actor::new(name)),
                Verdict::Allow,
                "{name} is a member"
            );
        }
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("guest")
            ),
            Verdict::Deny
        );
        // Membership is not a pvp licence.
        assert_eq!(
            index.check("overworld", at(8, 64, 8), Action::Pvp, &Actor::new("Owner")),
            Verdict::Deny
        );
        // An explicit deny applies to members too.
        let index = RegionIndex::from_regions([region.with_flag("build", false)]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("Owner")
            ),
            Verdict::Deny
        );
    }

    #[test]
    fn per_player_grants_allow_only_the_granted_actions() {
        let flags = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>()
        };
        let container =
            region(1, (0, 0, 0), (15, 255, 15)).trusted("Container", flags(&["entry", "use"]));
        let index = RegionIndex::from_regions([container]);
        for action in [Action::Interact, Action::UseItem, Action::Entry] {
            assert_eq!(
                index.check("overworld", at(8, 64, 8), action, &Actor::new("container")),
                Verdict::Allow,
                "{action:?} is granted to a container trustee"
            );
        }
        for action in [Action::Break, Action::Place] {
            assert_eq!(
                index.check("overworld", at(8, 64, 8), action, &Actor::new("Container")),
                Verdict::Deny,
                "{action:?} is not granted to a container trustee"
            );
        }
        // The grant is per player: everyone else is still denied.
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Interact,
                &Actor::new("guest")
            ),
            Verdict::Deny
        );

        // A build trustee may break and place as well.
        let build = region(1, (0, 0, 0), (15, 255, 15))
            .trusted("Builder", flags(&["entry", "use", "build"]));
        let index = RegionIndex::from_regions([build]);
        for action in [Action::Break, Action::Place, Action::Interact] {
            assert_eq!(
                index.check("overworld", at(8, 64, 8), action, &Actor::new("Builder")),
                Verdict::Allow,
                "{action:?} is granted to a build trustee"
            );
        }

        // An explicit region-wide deny still wins over a grant.
        let denied = region(1, (0, 0, 0), (15, 255, 15))
            .with_flag("build", false)
            .trusted("Builder", flags(&["build"]));
        let index = RegionIndex::from_regions([denied]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("Builder")
            ),
            Verdict::Deny
        );
    }

    #[test]
    fn bypass_allows_an_op_and_denies_a_non_op() {
        // The *resolution* of the bypass permission lives in the server (it
        // needs the permission store and a player handle); the index only has
        // to honour the resolved flag.
        let index = RegionIndex::from_regions([
            region(1, (0, 0, 0), (15, 255, 15)).with_flag("build", false)
        ]);
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::with_bypass("RegionOp", true)
            ),
            Verdict::Allow
        );
        assert_eq!(
            index.check(
                "overworld",
                at(8, 64, 8),
                Action::Break,
                &Actor::new("RegionGuest")
            ),
            Verdict::Deny
        );
    }

    #[test]
    fn the_denial_message_is_configurable() {
        let store = RegionStore::new();
        assert_eq!(&*store.deny_message(), DEFAULT_DENY_MESSAGE);
        store.set_deny_message("This area is protected.");
        assert_eq!(&*store.deny_message(), "This area is protected.");
    }

    #[test]
    fn overlapping_box_finds_only_the_box_and_deduplicates_spans() {
        let store = RegionStore::new();
        // Spans four chunks; must be reported once.
        store.upsert(region(1, (0, 0, 0), (16, 255, 16)).with_source("claims"));
        store.upsert(region(2, (100, 0, 100), (115, 255, 115)).with_source("claims"));
        // Adjacent to the queried box on a face, not overlapping.
        store.upsert(region(3, (16, 0, 0), (31, 255, 15)).with_source("claims"));

        let index = store.snapshot();
        let found = index.overlapping_box(
            "overworld",
            BlockPos::new(0, 64, 0),
            BlockPos::new(15, 64, 15),
        );
        let ids: Vec<u64> = found.iter().map(|handle| handle.id.get()).collect();
        assert_eq!(
            ids,
            vec![1],
            "far and merely adjacent regions must not match"
        );
    }

    #[test]
    fn upsert_owned_refuses_core_and_foreign_regions() {
        let store = RegionStore::new();
        store.upsert(region(1, (0, 0, 0), (15, 255, 15))); // core-owned, source None
        assert_eq!(
            store.upsert_owned("claims", region(1, (0, 0, 0), (15, 255, 15))),
            Err(RegionEditError::OwnedByCore)
        );
        // The core region is untouched.
        assert_eq!(store.snapshot().get(RegionId(1)).unwrap().source, None);

        store.upsert(region(2, (0, 0, 0), (15, 255, 15)).with_source("regions"));
        assert_eq!(
            store.upsert_owned("claims", region(2, (0, 0, 0), (15, 255, 15))),
            Err(RegionEditError::OwnedByOther("regions".to_string()))
        );

        // The owner may replace its own region.
        store.upsert(region(3, (0, 0, 0), (15, 255, 15)).with_source("claims"));
        store
            .upsert_owned("claims", region(3, (0, 0, 0), (7, 255, 7)))
            .unwrap();
        assert_eq!(
            store.snapshot().get(RegionId(3)).unwrap().max,
            BlockPos::new(7, 255, 7)
        );
        assert!(!store.remove_owned("regions", RegionId(3)));
        assert!(store.remove_owned("claims", RegionId(3)));
    }

    #[test]
    fn insert_owned_if_free_is_atomic_across_threads() {
        // Ten competing claims for the same box: the overlap test and the
        // insert share the writer lock, so exactly one may succeed.
        let store = Arc::new(RegionStore::new());
        let workers: Vec<_> = (0..10u64)
            .map(|id| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    store.insert_new_owned(
                        "claims",
                        region(id, (0, 0, 0), (15, 255, 15)).with_source("claims"),
                        |existing| existing.source.as_deref() == Some("claims"),
                    )
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Err(RegionEditError::Overlap))
                .count(),
            9,
            "{results:?}"
        );
        assert_eq!(store.snapshot().len(), 1);
    }

    #[test]
    fn insert_new_owned_refuses_the_same_id_even_for_the_same_plugin() {
        // The same-id race: several attempts pick one fresh id before either
        // insert lands. A fresh insert must refuse every occupied id, so
        // exactly one may succeed and no earlier claim is silently replaced.
        let store = Arc::new(RegionStore::new());
        let workers: Vec<_> = (0..10u64)
            .map(|_| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    store.insert_new_owned(
                        "claims",
                        region(42, (0, 0, 0), (15, 255, 15)).with_source("claims"),
                        |existing| existing.source.as_deref() == Some("claims"),
                    )
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Err(RegionEditError::IdTaken))
                .count(),
            9,
            "{results:?}"
        );
        assert_eq!(store.snapshot().len(), 1);
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
