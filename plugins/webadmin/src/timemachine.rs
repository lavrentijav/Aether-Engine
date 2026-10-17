//! The time machine: render and inspect the world *as it was* by replaying the
//! journal.
//!
//! The store's model is `world = generator(seed) + journal` (see
//! `aether_world::journal`). The generator is deterministic and the journal is
//! append-only, so the state at any past sequence number is not stored — it is
//! *computed*: for a column, take the generator baseline and apply every event
//! up to the chosen time. That is what lets one slider scrub a year of edits
//! with no snapshots and no extra disk.
//!
//! The one subtlety is the baseline. The live world already has every edit
//! applied, so `World::get_block` cannot answer "what would the generator have
//! put here". The first event at a position carries that baseline as its
//! `from`; a position with no events is untouched, and its live block *is* the
//! baseline. So a column's baseline is rebuilt from its own event list, not
//! read from the live world.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use aether_world::journal::ActorId;
use aether_world::{BlockStateId, StorageError};

use crate::map::{blocks_per_pixel, Surface, Tile, TileKey};
use crate::source::{block_name_of, is_air_state, HistorySource, MapSource};

/// The largest horizontal radius `/changes` will scan, in blocks. Guards the
/// `(2r/16)^2`-column loop against a single hostile query.
pub const MAX_CHANGE_RADIUS: u32 = 256;
/// The most events `/changes` will examine in one request, before it stops
/// early and returns what it has.
pub const MAX_CHANGE_SCAN: usize = 100_000;
/// The most rows `/changes` will return.
pub const MAX_CHANGE_RESULTS: usize = 4_096;
/// Replayed tiles kept in the time machine's cache.
const MAX_TIME_CACHE: usize = 256;

/// One change to one block, as the history pages present it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockChange {
    pub seq: u64,
    pub at_ms: u64,
    pub actor: ActorId,
    pub from: BlockStateId,
    pub to: BlockStateId,
}

/// One block change with its coordinates, for an area/time listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRecord {
    pub seq: u64,
    pub at_ms: u64,
    pub actor: ActorId,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub from: BlockStateId,
    pub to: BlockStateId,
}

/// A point on the timeline: replay every event numbered `at most` `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtSeq(pub u64);

/// The per-position replay state of one column.
///
/// Only positions anyone has ever edited appear; a position that is absent is
/// exactly the live world's block (and therefore the generator's baseline).
struct ColumnTimeline {
    /// `y` → `(state_at_time, baseline)`, where `state_at_time` is the last
    /// event at or before the time and `baseline` is the first event's `from`.
    states: BTreeMap<i32, (BlockStateId, BlockStateId)>,
}

/// The time machine over a live world's journal and blocks.
pub struct TimeMachine {
    history: Arc<dyn HistorySource>,
    source: Arc<dyn MapSource>,
    y_min: i32,
    y_max: i32,
    /// Replayed tiles, keyed by `(tile, at)`. The live tile cache is in
    /// [`crate::map::MapRenderer`]; this one caches the *time-travelled*
    /// tiles, which are far more expensive (a journal scan per pixel) and so
    /// must not be recomputed on every scrub step.
    cache: RwLock<HashMap<(TileKey, u64), Arc<Tile>>>,
}

impl TimeMachine {
    pub fn new(history: Arc<dyn HistorySource>, source: Arc<dyn MapSource>) -> Self {
        let (y_min, y_max) = (source.y_min(), source.y_max());
        Self {
            history,
            source,
            y_min,
            y_max,
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_height_range(mut self, y_min: i32, y_max: i32) -> Self {
        self.y_min = y_min;
        self.y_max = y_max.max(y_min);
        self
    }

    /// The journal's head: the sequence number that means "now".
    pub fn head(&self) -> u64 {
        self.history.head()
    }

    /// Every recorded change to the block at `(x, y, z)`, oldest first.
    pub fn block_history(&self, x: i32, y: i32, z: i32) -> Result<Vec<BlockChange>, StorageError> {
        let mut out = Vec::new();
        for e in self.history.column_events(x >> 4, z >> 4)? {
            // Expands a bulk revision into one entry per change at this block,
            // so a `/fill` shows up in the block's timeline like any edit.
            e.body.for_each_block_change(&mut |cx, cy, cz, from, to| {
                if (cx, cy, cz) == (x, y, z) {
                    out.push(BlockChange {
                        seq: e.seq,
                        at_ms: e.at_ms,
                        actor: e.actor,
                        from,
                        to,
                    });
                }
            });
        }
        Ok(out)
    }

    /// The block at `(x, y, z)` as it was at `at`.
    pub fn block_at(
        &self,
        x: i32,
        y: i32,
        z: i32,
        at: AtSeq,
    ) -> Result<BlockStateId, StorageError> {
        let timeline = self.column_timeline(x, z, at)?;
        Ok(match timeline.states.get(&y) {
            Some((state, _)) => *state,
            None => self.source.block(x, y, z),
        })
    }

    /// The block changes in a horizontal radius at or after `since_ms`, oldest
    /// first, capped at `limit` (the newest `limit` in the window).
    ///
    /// The radius is clamped to [`MAX_CHANGE_RADIUS`] and the scan is bounded
    /// by [`MAX_CHANGE_SCAN`] events, so one request cannot walk an unbounded
    /// area or an unbounded journal.
    pub fn changes_in_area(
        &self,
        x: i32,
        z: i32,
        radius: u32,
        since_ms: u64,
        until_ms: u64,
        limit: usize,
    ) -> Result<Vec<ChangeRecord>, StorageError> {
        let r = radius.min(MAX_CHANGE_RADIUS) as i64;
        let limit = limit.clamp(1, MAX_CHANGE_RESULTS);
        let budget = MAX_CHANGE_SCAN.max(limit * 32);
        let (cx0, cx1) = ((x as i64 - r) >> 4, (x as i64 + r) >> 4);
        let (cz0, cz1) = ((z as i64 - r) >> 4, (z as i64 + r) >> 4);
        let mut out = Vec::new();
        let mut scanned = 0usize;
        // A bulk revision is indexed under **every** column it touches, so a
        // multi-column revision is met once per column in this scan. Expand it
        // only on the first sighting: otherwise each in-radius change would be
        // returned k times and the scan budget spent k times over.
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        'scan: for ccx in cx0..=cx1 {
            for ccz in cz0..=cz1 {
                for e in self.history.column_events(ccx as i32, ccz as i32)? {
                    if !seen.insert(e.seq) {
                        continue;
                    }
                    if scanned >= budget {
                        break 'scan;
                    }
                    scanned += 1;
                    if e.at_ms < since_ms || e.at_ms > until_ms {
                        continue;
                    }
                    // A bulk revision contributes one record per change.
                    e.body.for_each_block_change(&mut |ex, y, ez, from, to| {
                        let (dx, dz) = ((ex as i64 - x as i64), (ez as i64 - z as i64));
                        if dx * dx + dz * dz <= r * r {
                            out.push(ChangeRecord {
                                seq: e.seq,
                                at_ms: e.at_ms,
                                actor: e.actor,
                                x: ex,
                                y,
                                z: ez,
                                from,
                                to,
                            });
                        }
                    });
                }
            }
        }
        out.sort_by_key(|c| c.seq);
        if out.len() > limit {
            out.drain(0..out.len() - limit);
        }
        Ok(out)
    }

    /// The surface of the column at `(x, z)` as it was at `at`.
    pub fn surface_at(&self, x: i32, z: i32, at: AtSeq) -> Result<Option<Surface>, StorageError> {
        let timeline = self.column_timeline(x, z, at)?;
        let mut y = self.y_max;
        while y >= self.y_min {
            let id = match timeline.states.get(&y) {
                Some((state, _)) => *state,
                None => self.source.block(x, y, z),
            };
            let name = self
                .source
                .block_name(id)
                .unwrap_or_else(|| block_name_of(id));
            if !is_air_state(&name) {
                let is_water = name.starts_with("minecraft:water");
                let mut depth = 0;
                if is_water {
                    let mut d = y;
                    while d >= self.y_min {
                        let below = match timeline.states.get(&d) {
                            Some((state, _)) => *state,
                            None => self.source.block(x, d, z),
                        };
                        let bn = self
                            .source
                            .block_name(below)
                            .unwrap_or_else(|| block_name_of(below));
                        if bn.starts_with("minecraft:water") {
                            d -= 1;
                            depth += 1;
                        } else {
                            break;
                        }
                    }
                }
                return Ok(Some(Surface {
                    id,
                    y,
                    water_depth: depth,
                    is_water,
                }));
            }
            y -= 1;
        }
        Ok(None)
    }

    /// Render a tile as of `at`, with the same geometry as [`crate::map`].
    ///
    /// Replayed tiles are cached by `(tile, at)`: a scrub re-requests the same
    /// tiles repeatedly, and each is a journal scan per pixel, so recomputing
    /// them would be the map's costliest path.
    pub fn tile_at(
        &self,
        key: TileKey,
        at: AtSeq,
        tile_px: u32,
    ) -> Result<Arc<Tile>, StorageError> {
        if let Some(t) = self.cache.read().unwrap().get(&(key, at.0)) {
            return Ok(Arc::clone(t));
        }
        let tile = Arc::new(self.render_tile_at(key, at, tile_px)?);
        let mut cache = self.cache.write().unwrap();
        if cache.len() >= MAX_TIME_CACHE {
            cache.clear();
        }
        cache.insert((key, at.0), Arc::clone(&tile));
        Ok(tile)
    }

    /// How many replayed tiles are cached.
    pub fn cached_tiles(&self) -> usize {
        self.cache.read().unwrap().len()
    }

    fn render_tile_at(&self, key: TileKey, at: AtSeq, tile_px: u32) -> Result<Tile, StorageError> {
        let px = tile_px.max(1);
        let bpp = blocks_per_pixel(key.zoom);
        let mut rgba = vec![0u8; (px * px * 4) as usize];
        let mut surface_y = vec![i16::MIN; (px * px) as usize];
        if let Some((ox, oz)) = key.checked_origin(px) {
            for pz in 0..px {
                for pix in 0..px {
                    let x = ox + pix as i32 * bpp;
                    let z = oz + pz as i32 * bpp;
                    let Some(s) = self.surface_at(x, z, at)? else {
                        continue;
                    };
                    let idx = (pz * px + pix) as usize;
                    surface_y[idx] = s.y.clamp(i16::MIN as i32 + 1, i16::MAX as i32) as i16;
                    let c = s.color(1.0);
                    rgba[idx * 4..idx * 4 + 4].copy_from_slice(&c);
                }
            }
        }
        Ok(Tile {
            key,
            px,
            rgba,
            surface_y,
        })
    }

    /// The per-position replay state of column `(x, z)` at `at`.
    fn column_timeline(&self, x: i32, z: i32, at: AtSeq) -> Result<ColumnTimeline, StorageError> {
        let mut events: BTreeMap<i32, Vec<(u64, BlockStateId, BlockStateId)>> = BTreeMap::new();
        for e in self.history.column_events(x >> 4, z >> 4)? {
            // A bulk revision contributes a `(seq, from, to)` row for every
            // change at this column, so the time machine replays it.
            e.body.for_each_block_change(&mut |ex, y, ez, from, to| {
                if ex == x && ez == z {
                    events.entry(y).or_default().push((e.seq, from, to));
                }
            });
        }
        let mut states = BTreeMap::new();
        for (y, list) in events {
            // `column_events` is oldest first, so the first entry's `from` is
            // the generator's baseline and later entries overwrite forward.
            let baseline = list[0].1;
            let mut state = baseline;
            for (seq, _, to) in &list {
                // `includes` needs an `Event`; compare the sequence directly.
                if *seq <= at.0 {
                    state = *to;
                }
            }
            states.insert(y, (state, baseline));
        }
        Ok(ColumnTimeline { states })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::World;
    use aether_world::journal::{ActorId, EventBody};
    use aether_world::registry::ids;
    use aether_world::{BlockStateId, MemStore};
    use aether_worldgen::FlatGenerator;

    const ALICE: ActorId = ActorId(0xA11CE);
    const BOB: ActorId = ActorId(0xB0B);

    fn flat() -> Arc<World<MemStore, FlatGenerator>> {
        Arc::new(World::new(MemStore::new(), FlatGenerator::classic()))
    }

    fn tm(world: Arc<World<MemStore, FlatGenerator>>) -> TimeMachine {
        TimeMachine::new(world.clone(), world)
    }

    /// Write a block with its registry properties (a mismatched `BlockProperties`
    /// is refused by the palette, so tests must not invent them).
    fn set(
        w: &World<MemStore, FlatGenerator>,
        actor: ActorId,
        x: i32,
        y: i32,
        z: i32,
        id: BlockStateId,
    ) -> Option<u64> {
        let props = w.props_of(id);
        w.set_block_by(actor, x, y, z, id, props)
    }

    #[test]
    fn block_history_is_complete_and_ordered() {
        let w = flat();
        let _ = set(&w, ALICE, 0, 10, 0, ids::STONE);
        let _ = set(&w, BOB, 0, 10, 0, ids::DIRT);
        let t = tm(w);
        let h = t.block_history(0, 10, 0).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].actor, ALICE);
        assert_eq!(h[0].to, ids::STONE);
        assert_eq!(h[1].actor, BOB);
        assert_eq!(h[1].to, ids::DIRT);
        assert!(h[0].seq < h[1].seq);
    }

    #[test]
    fn block_at_replays_between_edits() {
        let w = flat();
        // Grass at (0, 3, 0) is the flat baseline.
        let s1 = set(&w, ALICE, 0, 3, 0, ids::STONE).unwrap();
        let s2 = set(&w, BOB, 0, 3, 0, ids::DIRT).unwrap();

        let t = tm(w);
        // Before any edit: the generated grass.
        assert_eq!(t.block_at(0, 3, 0, AtSeq(0)).unwrap(), ids::GRASS_BLOCK);
        // After Alice, before Bob.
        assert_eq!(t.block_at(0, 3, 0, AtSeq(s1)).unwrap(), ids::STONE);
        // After Bob: current.
        assert_eq!(t.block_at(0, 3, 0, AtSeq(s2)).unwrap(), ids::DIRT);
    }

    #[test]
    fn surface_follows_replay() {
        let w = flat();
        // Dig a hole: remove the grass surface.
        let s = set(&w, ALICE, 5, 3, 5, ids::AIR).unwrap();
        let t = tm(w);
        let now = t.surface_at(5, 5, AtSeq(s)).unwrap().unwrap();
        assert_eq!(now.y, 2, "surface drops to the dirt below");
        assert_eq!(now.id, ids::DIRT);
        let before = t.surface_at(5, 5, AtSeq(0)).unwrap().unwrap();
        assert_eq!(before.y, 3);
        assert_eq!(before.id, ids::GRASS_BLOCK);
    }

    #[test]
    fn changes_in_area_filters_by_time_and_radius() {
        let w = flat();
        let e1 = w
            .journal()
            .append(
                ALICE,
                EventBody::BlockSet {
                    x: 1,
                    y: 4,
                    z: 1,
                    from: ids::AIR,
                    to: ids::STONE,
                },
            )
            .unwrap();
        let e2 = w
            .journal()
            .append(
                BOB,
                EventBody::BlockSet {
                    x: 40,
                    y: 4,
                    z: 40,
                    from: ids::AIR,
                    to: ids::DIRT,
                },
            )
            .unwrap();
        assert!(e2 > e1);
        let t = tm(w);
        let near = t.changes_in_area(0, 0, 8, 0, u64::MAX, 100).unwrap();
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].x, 1);
        assert_eq!(near[0].actor, ALICE);
    }

    #[test]
    fn tile_at_renders_a_past_tile() {
        let w = flat();
        let _ = set(&w, ALICE, 0, 3, 0, ids::STONE);
        let t = tm(w).with_height_range(0, 8);
        let past = t.tile_at(TileKey::new(0, 0, 0), AtSeq(0), 2).unwrap();
        assert_eq!(past.pixel(0, 0), [96, 158, 64, 255]);
        let now = t
            .tile_at(TileKey::new(0, 0, 0), AtSeq(u64::MAX), 2)
            .unwrap();
        assert_eq!(now.pixel(0, 0), [125, 125, 125, 255]);
    }

    #[test]
    fn replayed_tiles_are_cached() {
        let w = flat();
        let _ = set(&w, ALICE, 0, 3, 0, ids::STONE);
        let t = tm(w).with_height_range(0, 8);
        assert_eq!(t.cached_tiles(), 0);
        let _ = t.tile_at(TileKey::new(0, 0, 0), AtSeq(0), 2).unwrap();
        assert_eq!(t.cached_tiles(), 1, "a replayed tile is cached");
        // A second request for the same (tile, time) uses the cache.
        let _ = t.tile_at(TileKey::new(0, 0, 0), AtSeq(0), 2).unwrap();
        assert_eq!(t.cached_tiles(), 1);
        // A different time is a different cache entry.
        let _ = t.tile_at(TileKey::new(0, 0, 0), AtSeq(1), 2).unwrap();
        assert_eq!(t.cached_tiles(), 2);
    }

    #[test]
    fn a_bulk_revision_replays_in_the_time_machine() {
        // Finding 5: block history, `block_at` and area changes all expand a
        // bulk revision, so the time machine does not show the pre-bulk state
        // as "now".
        let w = flat();
        let seq = w
            .journal()
            .append(
                ALICE,
                EventBody::BlockGroup {
                    changes: vec![
                        aether_world::journal::BlockChange {
                            x: 0,
                            y: 3,
                            z: 0,
                            from: ids::GRASS_BLOCK,
                            to: ids::STONE,
                        },
                        // A second column, so the revision is indexed under
                        // both and an area scan meets it twice.
                        aether_world::journal::BlockChange {
                            x: 20,
                            y: 3,
                            z: 0,
                            from: ids::GRASS_BLOCK,
                            to: ids::DIRT,
                        },
                    ],
                },
            )
            .unwrap();
        let t = tm(w);
        let h = t.block_history(0, 3, 0).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].to, ids::STONE);
        assert_eq!(t.block_at(0, 3, 0, AtSeq(0)).unwrap(), ids::GRASS_BLOCK);
        assert_eq!(t.block_at(0, 3, 0, AtSeq(seq)).unwrap(), ids::STONE);
        // The multi-column revision must be expanded once, not once per
        // column it is indexed under.
        let ch = t.changes_in_area(10, 0, 32, 0, u64::MAX, 100).unwrap();
        assert_eq!(ch.len(), 2, "both changes, no duplicates: {ch:?}");
        let xs: Vec<i32> = ch.iter().map(|c| c.x).collect();
        assert_eq!(xs, vec![0, 20]);
    }

    #[test]
    fn changes_radius_and_scan_are_clamped() {
        let w = flat();
        for i in 0..10 {
            let _ = w.journal().append(
                ALICE,
                EventBody::BlockSet {
                    x: i,
                    y: 4,
                    z: 0,
                    from: ids::AIR,
                    to: ids::STONE,
                },
            );
        }
        let t = tm(w);
        // A giant radius and limit do not run away; the result is bounded.
        let out = t
            .changes_in_area(0, 0, u32::MAX, 0, u64::MAX, usize::MAX)
            .unwrap();
        assert!(out.len() <= MAX_CHANGE_RESULTS);
        assert_eq!(out.len(), 10);
    }
}
