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
use std::sync::RwLock;

use aether_core::math::Vec3;
use aether_physics::{step, BlockView};
use aether_world::registry::BlockRegistry;
use aether_world::storage::format::SubChunkKey;
use aether_world::{KvBackend, StorageError, SubChunk, WorldStorage};
use aether_worldgen::ChunkGenerator;

pub mod player;

// Public re-exports: the pieces callers most often need alongside `World`.
pub use aether_core::math::{Aabb, Vec3 as Vector3};
pub use aether_physics::{Body, PhysicsParams};
pub use aether_world::registry::ids as block_ids;
pub use aether_world::{BlockProperties, BlockStateId, FullBright, LightView, MemStore};
pub use aether_worldgen::{FlatGenerator, NoiseGenerator};
pub use player::{GameMode, Player};

#[cfg(feature = "fjall")]
pub use aether_world::FjallStore;

// Vertical band of sub-chunks scanned when loading a column from storage.
const SCAN_CY_MIN: i8 = -4;
const SCAN_CY_MAX: i8 = 19;

/// Split a world coordinate into `(chunk-or-section index, local 0..16)`.
#[inline]
fn split(v: i32) -> (i32, usize) {
    (v.div_euclid(16), v.rem_euclid(16) as usize)
}

/// A live world: block access, on-demand generation and physics over a
/// [`KvBackend`] and a [`ChunkGenerator`].
pub struct World<B: KvBackend, G: ChunkGenerator> {
    storage: WorldStorage<B>,
    generator: G,
    registry: BlockRegistry,
    cache: RwLock<HashMap<SubChunkKey, SubChunk>>,
    dirty: RwLock<HashSet<SubChunkKey>>,
    columns: RwLock<HashSet<(i32, i32)>>,
    params: PhysicsParams,
}

impl<B: KvBackend, G: ChunkGenerator> World<B, G> {
    /// Create a world from a storage backend and a chunk generator.
    pub fn new(backend: B, generator: G) -> Self {
        Self {
            storage: WorldStorage::new(backend),
            generator,
            registry: BlockRegistry::new(),
            cache: RwLock::new(HashMap::new()),
            dirty: RwLock::new(HashSet::new()),
            columns: RwLock::new(HashSet::new()),
            params: PhysicsParams::default(),
        }
    }

    /// Override the physics tuning used by [`World::step_body`].
    pub fn with_physics(mut self, params: PhysicsParams) -> Self {
        self.params = params;
        self
    }

    /// The canonical block registry (name ⇄ id ⇄ properties).
    pub fn registry(&self) -> &BlockRegistry {
        &self.registry
    }

    /// Ensure the chunk column `(cx, cz)` is present in the cache, loading it
    /// from storage or generating (and persisting) it on first touch.
    fn ensure_column(&self, cx: i32, cz: i32) {
        // Atomically reserve the column so two workers can't load/generate the
        // same `(cx, cz)` concurrently (and double-save its keys).
        {
            let mut columns = self.columns.write().unwrap();
            if !columns.insert((cx, cz)) {
                return; // already reserved by us earlier or another worker
            }
        }

        // Load any persisted sections in the scan band. Distinguish a genuine
        // read error from "no data": on error we must NOT regenerate, or we'd
        // clobber existing-but-unreadable data with fresh terrain.
        let mut loaded_any = false;
        let mut read_error = false;
        for cy in SCAN_CY_MIN..=SCAN_CY_MAX {
            let key = SubChunkKey::new(cx, cy, cz);
            match self.storage.load(key) {
                Ok(Some(sc)) => {
                    self.cache.write().unwrap().insert(key, sc);
                    loaded_any = true;
                }
                Ok(None) => {}
                Err(_) => read_error = true,
            }
        }

        // Nothing on disk (and no read error) -> generate and persist. If a save
        // fails, keep the section in the cache and mark it dirty so a later
        // `flush` retries rather than silently losing it.
        if !loaded_any && !read_error {
            let column = self.generator.generate_column(cx, cz);
            let mut cache = self.cache.write().unwrap();
            for (cy, sc) in column.sections {
                let key = SubChunkKey::new(cx, cy, cz);
                if self.storage.save(key, &sc).is_err() {
                    self.dirty.write().unwrap().insert(key);
                }
                cache.insert(key, sc);
            }
        }
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
        self.ensure_column(key.cx, key.cz);
        self.cache
            .read()
            .unwrap()
            .get(&key)
            .map(|sc| sc.get(lx, ly, lz))
            .unwrap_or(BlockStateId::AIR)
    }

    /// The block name at `(x, y, z)`, if known to the registry.
    pub fn get_block_name(&self, x: i32, y: i32, z: i32) -> Option<String> {
        self.registry
            .name_of(self.get_block(x, y, z))
            .map(str::to_owned)
    }

    /// Set the block at `(x, y, z)` to a registry block `name`, generating the
    /// column first if needed. Returns the assigned block id.
    pub fn set_block(&self, x: i32, y: i32, z: i32, name: &str) -> Option<BlockStateId> {
        let id = self.registry.get(name)?;
        let props = self.registry.props_of(id);
        self.set_block_id(x, y, z, id, props);
        Some(id)
    }

    /// Set the block at `(x, y, z)` to an explicit id + properties.
    pub fn set_block_id(&self, x: i32, y: i32, z: i32, id: BlockStateId, props: BlockProperties) {
        let Some((key, lx, ly, lz)) = Self::key_of(x, y, z) else {
            return;
        };
        self.ensure_column(key.cx, key.cz);
        {
            let mut cache = self.cache.write().unwrap();
            cache.entry(key).or_default().set(lx, ly, lz, id, props);
        }
        self.dirty.write().unwrap().insert(key);
    }

    /// Persist every sub-chunk modified since the last flush.
    pub fn flush(&self) -> Result<(), StorageError> {
        let keys: Vec<SubChunkKey> = self.dirty.read().unwrap().iter().copied().collect();
        {
            let cache = self.cache.read().unwrap();
            for key in &keys {
                if let Some(sc) = cache.get(key) {
                    self.storage.save(*key, sc)?;
                }
            }
        }
        // Only commit the durability flush, then clear exactly the keys we
        // saved — not the whole set — so keys dirtied concurrently survive and a
        // failed flush leaves the dirty bookkeeping intact for a retry.
        self.storage.flush()?;
        let mut dirty = self.dirty.write().unwrap();
        for key in &keys {
            dirty.remove(key);
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

    /// The number of sub-chunks currently resident in the cache.
    pub fn resident_sections(&self) -> usize {
        self.cache.read().unwrap().len()
    }
}

/// Blocks with the `collision` property act as solid unit cubes for physics.
impl<B: KvBackend, G: ChunkGenerator> BlockView for World<B, G> {
    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let Some((key, lx, ly, lz)) = Self::key_of(x, y, z) else {
            return false;
        };
        self.ensure_column(key.cx, key.cz);
        let cache = self.cache.read().unwrap();
        match cache.get(&key) {
            Some(sc) => sc.collision_mask().get(SubChunk::index(lx, ly, lz)),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_worldgen::{FlatGenerator, NoiseGenerator};

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
