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

    /// The topmost non-air block of a column as `(id, y, water_depth)`.
    ///
    /// `water_depth` is the number of water blocks directly above the floor,
    /// so the renderer can tint deep water; it is 0 for a dry surface.
    fn surface(&self, x: i32, z: i32) -> Option<(BlockStateId, i32, i32)> {
        let mut y = self.y_max();
        let min = self.y_min();
        while y >= min {
            let id = self.block(x, y, z);
            let air = self
                .block_name(id)
                .map(|n| is_air_state(&n))
                .unwrap_or(id == BlockStateId::AIR);
            if !air {
                // Count a water column's depth above the floor tile.
                let mut depth = 0;
                if let Some(name) = self.block_name(id) {
                    if name.starts_with("minecraft:water") || name == "minecraft:water" {
                        let mut d = y;
                        while d >= min {
                            let below = self.block(x, d, z);
                            if self.block_name(below).as_deref() == Some("minecraft:water") {
                                d -= 1;
                                depth += 1;
                            } else {
                                break;
                            }
                        }
                    }
                }
                return Some((id, y, depth));
            }
            y -= 1;
        }
        None
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
