//! Block-Entity arena (spec §5.5).
//!
//! Heavy per-block objects — chests with inventories, furnaces, spawners, signs
//! — are **not** stored inside the [`AvxCell`](crate::AvxCell)/[`SubChunk`](crate::SubChunk)
//! block arrays. Keeping variable-sized payloads out of the 64-byte cache-line
//! cells is what lets the hot block/mask scans stay dense and SIMD-friendly.
//!
//! Instead each sub-chunk owns a [`BlockEntityArena`]: the block itself is just
//! an ordinary block id in the cell, and its rich state lives in a slot arena
//! addressed by the block's **Morton position** (`0..4096`). A free-list reuses
//! slots as block entities come and go, so churn (breaking/placing chests)
//! doesn't fragment the arena.
//!
//! ```
//! use aether_world::block_entity::{BlockEntity, BlockEntityArena};
//!
//! let mut arena = BlockEntityArena::new();
//! let pos = arena.insert_at(2, 3, 4, BlockEntity::Chest);
//! assert_eq!(arena.get(pos), Some(&BlockEntity::Chest));
//! assert_eq!(arena.len(), 1);
//! assert_eq!(arena.remove(pos), Some(BlockEntity::Chest));
//! assert!(arena.get(pos).is_none());
//! ```

use crate::subchunk::SubChunk;
use std::collections::HashMap;

/// A heavy per-block object attached to a block position.
///
/// A small, engine-side model of the Vanilla "block entity" — enough to carry
/// the state that does not fit in a block id. Real inventories/NBT hang off
/// these variants as the storage layer grows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockEntity {
    /// A container (chest, barrel, hopper …).
    Chest,
    /// A furnace / smoker / blast furnace.
    Furnace,
    /// A mob spawner for the named entity type.
    Spawner {
        /// Namespaced entity id the spawner produces.
        mob: String,
    },
    /// A sign carrying up to a few lines of text.
    Sign {
        /// The sign's text lines.
        lines: Vec<String>,
    },
    /// Any other block-entity kind, identified by its type name.
    Other {
        /// Namespaced block-entity type id.
        kind: String,
    },
}

/// A per-sub-chunk arena of [`BlockEntity`]s, keyed by Morton block position.
///
/// Slots are recycled through a free-list, and a `position → slot` map keeps
/// lookups by block position O(1). The arena never touches the serialized
/// sub-chunk blob, so adding it changes no on-disk format.
#[derive(Debug, Clone, Default)]
pub struct BlockEntityArena {
    slots: Vec<Option<BlockEntity>>,
    free: Vec<u32>,
    by_pos: HashMap<u16, u32>,
}

impl BlockEntityArena {
    /// An empty arena.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of block entities currently stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.by_pos.len()
    }

    /// Whether the arena holds no block entities.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.by_pos.is_empty()
    }

    /// Insert or replace the block entity at Morton position `pos`, returning
    /// the previous one if the slot was occupied.
    pub fn insert(&mut self, pos: u16, entity: BlockEntity) -> Option<BlockEntity> {
        if let Some(&slot) = self.by_pos.get(&pos) {
            return self.slots[slot as usize].replace(entity);
        }
        let slot = if let Some(slot) = self.free.pop() {
            self.slots[slot as usize] = Some(entity);
            slot
        } else {
            let slot = self.slots.len() as u32;
            self.slots.push(Some(entity));
            slot
        };
        self.by_pos.insert(pos, slot);
        None
    }

    /// Insert at local block coordinates `(x, y, z)` (each `0..16`), returning
    /// the Morton position key so the caller can address it later.
    pub fn insert_at(&mut self, x: usize, y: usize, z: usize, entity: BlockEntity) -> u16 {
        let pos = SubChunk::index(x, y, z);
        self.insert(pos, entity);
        pos
    }

    /// Borrow the block entity at `pos`, if present.
    #[inline]
    pub fn get(&self, pos: u16) -> Option<&BlockEntity> {
        let slot = *self.by_pos.get(&pos)?;
        self.slots[slot as usize].as_ref()
    }

    /// Mutably borrow the block entity at `pos`, if present.
    #[inline]
    pub fn get_mut(&mut self, pos: u16) -> Option<&mut BlockEntity> {
        let slot = *self.by_pos.get(&pos)?;
        self.slots[slot as usize].as_mut()
    }

    /// Whether a block entity exists at `pos`.
    #[inline]
    pub fn contains(&self, pos: u16) -> bool {
        self.by_pos.contains_key(&pos)
    }

    /// Remove and return the block entity at `pos`, freeing its slot for reuse.
    pub fn remove(&mut self, pos: u16) -> Option<BlockEntity> {
        let slot = self.by_pos.remove(&pos)?;
        let taken = self.slots[slot as usize].take();
        self.free.push(slot);
        taken
    }

    /// Iterate `(morton_pos, &block_entity)` over every stored entity.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &BlockEntity)> + '_ {
        self.by_pos
            .iter()
            .filter_map(move |(&pos, &slot)| self.slots[slot as usize].as_ref().map(|be| (pos, be)))
    }

    /// Number of arena slots currently allocated (live + free).
    #[inline]
    pub fn slots(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove_round_trip() {
        let mut a = BlockEntityArena::new();
        assert!(a.is_empty());
        let pos = a.insert_at(1, 2, 3, BlockEntity::Furnace);
        assert_eq!(a.len(), 1);
        assert!(a.contains(pos));
        assert_eq!(a.get(pos), Some(&BlockEntity::Furnace));
        assert_eq!(a.remove(pos), Some(BlockEntity::Furnace));
        assert!(!a.contains(pos));
        assert_eq!(a.len(), 0);
        assert_eq!(a.remove(pos), None);
    }

    #[test]
    fn insert_replaces_and_returns_old() {
        let mut a = BlockEntityArena::new();
        let pos = a.insert_at(0, 0, 0, BlockEntity::Chest);
        let old = a.insert(
            pos,
            BlockEntity::Spawner {
                mob: "minecraft:zombie".into(),
            },
        );
        assert_eq!(old, Some(BlockEntity::Chest));
        assert_eq!(a.len(), 1); // replaced, not added
        assert!(matches!(a.get(pos), Some(BlockEntity::Spawner { .. })));
    }

    #[test]
    fn get_mut_edits_in_place() {
        let mut a = BlockEntityArena::new();
        let pos = a.insert_at(5, 5, 5, BlockEntity::Sign { lines: vec![] });
        if let Some(BlockEntity::Sign { lines }) = a.get_mut(pos) {
            lines.push("hello".into());
        }
        assert_eq!(
            a.get(pos),
            Some(&BlockEntity::Sign {
                lines: vec!["hello".to_string()]
            })
        );
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut a = BlockEntityArena::new();
        let p0 = a.insert_at(0, 0, 0, BlockEntity::Chest);
        let _p1 = a.insert_at(1, 0, 0, BlockEntity::Furnace);
        assert_eq!(a.slots(), 2);
        a.remove(p0);
        // A new insert reuses the freed slot rather than growing the arena.
        a.insert_at(2, 0, 0, BlockEntity::Chest);
        assert_eq!(a.slots(), 2);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn distinct_positions_are_independent() {
        let mut a = BlockEntityArena::new();
        let p1 = a.insert_at(1, 1, 1, BlockEntity::Chest);
        let p2 = a.insert_at(2, 2, 2, BlockEntity::Furnace);
        assert_ne!(p1, p2);
        assert_eq!(a.get(p1), Some(&BlockEntity::Chest));
        assert_eq!(a.get(p2), Some(&BlockEntity::Furnace));
    }

    #[test]
    fn iter_visits_all_entities() {
        let mut a = BlockEntityArena::new();
        a.insert_at(0, 0, 0, BlockEntity::Chest);
        a.insert_at(15, 15, 15, BlockEntity::Furnace);
        let mut kinds: Vec<_> = a.iter().map(|(_, be)| be.clone()).collect();
        kinds.sort_by_key(|be| format!("{be:?}"));
        assert_eq!(kinds, vec![BlockEntity::Chest, BlockEntity::Furnace]);
    }
}
