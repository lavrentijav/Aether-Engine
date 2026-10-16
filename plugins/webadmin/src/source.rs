//! The two read-only views the web interface needs of a running world.
//!
//! The map logic never touches the concrete `World`; it reads blocks and the
//! journal through these two traits. That keeps the renderer and the time
//! machine unit-testable against a tiny mock, and it keeps WEB2 from growing a
//! second dependency on the server's internals: when the server bootstrap
//! (WEB1) hands the plugin a world, one blanket impl per trait is all that is
//! needed.
//!
//! Both traits are implemented for [`aether_api::World`] in this module, so in
//! practice a caller shares `Arc<World<_, _>>` and coerces it to either view.

use std::collections::HashMap;
use std::sync::Arc;

use aether_world::journal::{ActorId, Event};
use aether_world::registry::blocks;
use aether_world::{BlockStateId, KvBackend, StorageError};
use aether_worldgen::ChunkGenerator;

/// Read-only block access with an explicit vertical band to scan.
pub trait MapSource: Send + Sync {
    /// The block at `(x, y, z)`; air outside the world.
    fn block(&self, x: i32, y: i32, z: i32) -> BlockStateId;

    /// The full state name of a block id, when the world knows it.
    fn block_name(&self, id: BlockStateId) -> Option<String>;

    /// Lowest world Y the map samples (inclusive).
    fn y_min(&self) -> i32;

    /// Highest world Y the map samples (inclusive).
    fn y_max(&self) -> i32;

    /// Whether `id` is air, resolved through the world's own name registry.
    ///
    /// The single name authority: a modded world's registry is what decides,
    /// and the global fallback is only used for an id the world does not know.
    fn is_air(&self, id: BlockStateId) -> bool {
        let name = self.block_name(id).unwrap_or_else(|| block_name_of(id));
        is_air_state(&name)
    }

    /// The topmost non-air block of a column within `[y_min, y_max]`, as
    /// `(id, y, water_depth)`. `water_depth` is the number of water blocks
    /// directly above the floor, so deep water can be tinted; `0` when dry.
    ///
    /// The default scans with [`MapSource::block`]; [`aether_api::World`]
    /// overrides it to snapshot the whole column once (one column latch, one
    /// cache read lock) rather than a lock per block.
    fn surface_in(
        &self,
        x: i32,
        z: i32,
        y_min: i32,
        y_max: i32,
    ) -> Option<(BlockStateId, i32, i32)> {
        let mut y = y_max;
        while y >= y_min {
            let id = self.block(x, y, z);
            if !self.is_air(id) {
                let mut depth = 0;
                if self.is_water(id) {
                    let mut d = y;
                    while d >= y_min {
                        if self.is_water(self.block(x, d, z)) {
                            d -= 1;
                            depth += 1;
                        } else {
                            break;
                        }
                    }
                }
                return Some((id, y, depth));
            }
            y -= 1;
        }
        None
    }

    /// The topmost non-air block of a column in the source's own band.
    fn surface(&self, x: i32, z: i32) -> Option<(BlockStateId, i32, i32)> {
        self.surface_in(x, z, self.y_min(), self.y_max())
    }

    /// Whether `id` is water (state name based, like air).
    fn is_water(&self, id: BlockStateId) -> bool {
        self.block_name(id)
            .unwrap_or_else(|| block_name_of(id))
            .starts_with("minecraft:water")
    }
}

/// Whether a full state name is one of the air states.
///
/// Matches the bare block name too, because a state name carries properties
/// (`minecraft:water[level=0]`) while air never does.
pub(crate) fn is_air_state(name: &str) -> bool {
    let bare = name
        .strip_prefix("minecraft:")
        .unwrap_or(name)
        .split('[')
        .next()
        .unwrap_or(name);
    matches!(bare, "air" | "cave_air" | "void_air")
}

/// The read-only journal queries the time machine and the history pages use.
pub trait HistorySource: Send + Sync {
    /// The next sequence number the journal will hand out.
    fn head(&self) -> u64;

    /// Every event affecting column `(cx, cz)`, oldest first.
    fn column_events(&self, cx: i32, cz: i32) -> Result<Vec<Event>, StorageError>;

    /// The `limit` newest events overall, oldest first.
    fn recent_events(&self, limit: usize) -> Result<Vec<Event>, StorageError>;

    /// Stream every event, oldest first.
    fn for_each_event(
        &self,
        f: &mut dyn FnMut(&Event) -> Result<(), StorageError>,
    ) -> Result<(), StorageError>;

    /// Stream `actor`'s events, oldest first.
    fn for_each_actor_event(
        &self,
        actor: ActorId,
        f: &mut dyn FnMut(&Event) -> Result<(), StorageError>,
    ) -> Result<(), StorageError>;
}

impl<B, G> MapSource for aether_api::World<B, G>
where
    B: KvBackend + Send + Sync + 'static,
    G: ChunkGenerator + Send + Sync + 'static,
{
    fn block(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        aether_api::World::get_block(self, x, y, z)
    }

    fn block_name(&self, id: BlockStateId) -> Option<String> {
        // The full state name is what a human reads in a tooltip; colour is
        // resolved from the id directly, so a state name never confuses the
        // palette.
        aether_api::World::block_name_of(self, id)
    }

    fn y_min(&self) -> i32 {
        -64
    }

    fn y_max(&self) -> i32 {
        319
    }

    /// One column snapshot (one latch, one cache read lock) instead of a lock
    /// per block; names are memoised per distinct state so the registry lock
    /// is taken a handful of times per column, not once per cell.
    fn surface_in(
        &self,
        x: i32,
        z: i32,
        y_min: i32,
        y_max: i32,
    ) -> Option<(BlockStateId, i32, i32)> {
        let column = aether_api::World::column_snapshot(self, x, z, y_min, y_max);
        if column.is_empty() {
            return None;
        }
        let mut memo: HashMap<BlockStateId, (bool, bool)> = HashMap::new();
        let classify = |id: BlockStateId, memo: &mut HashMap<BlockStateId, (bool, bool)>| {
            *memo.entry(id).or_insert_with(|| {
                let name = self.block_name(id).unwrap_or_else(|| block_name_of(id));
                (is_air_state(&name), name.starts_with("minecraft:water"))
            })
        };
        for (i, &id) in column.iter().enumerate().rev() {
            let (air, water) = classify(id, &mut memo);
            if air {
                continue;
            }
            let mut depth = 0;
            if water {
                let mut j = i;
                while j > 0 {
                    let below = column[j - 1];
                    if classify(below, &mut memo).1 {
                        j -= 1;
                        depth += 1;
                    } else {
                        break;
                    }
                }
            }
            return Some((id, y_min + i as i32, depth));
        }
        None
    }
}

impl<B, G> HistorySource for aether_api::World<B, G>
where
    B: KvBackend + Send + Sync + 'static,
    G: ChunkGenerator + Send + Sync + 'static,
{
    fn head(&self) -> u64 {
        self.journal().head()
    }

    fn column_events(&self, cx: i32, cz: i32) -> Result<Vec<Event>, StorageError> {
        self.journal().column_events(cx, cz)
    }

    fn recent_events(&self, limit: usize) -> Result<Vec<Event>, StorageError> {
        self.journal().recent_events(limit)
    }

    fn for_each_event(
        &self,
        f: &mut dyn FnMut(&Event) -> Result<(), StorageError>,
    ) -> Result<(), StorageError> {
        self.journal().for_each_event(|e| f(e))
    }

    fn for_each_actor_event(
        &self,
        actor: ActorId,
        f: &mut dyn FnMut(&Event) -> Result<(), StorageError>,
    ) -> Result<(), StorageError> {
        self.journal().for_each_actor_event(actor, |e| f(e))
    }
}

/// The name of the *block* a state belongs to, properties stripped, for a
/// tooltip. Falls back to the raw id for a block the table does not know.
pub fn block_name_of(id: BlockStateId) -> String {
    blocks::block_of_state(id)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| format!("minecraft:air#{}", id.raw()))
}

/// Type-erased shared map view.
pub type MapView = Arc<dyn MapSource>;
/// Type-erased shared history view.
pub type HistoryView = Arc<dyn HistorySource>;
