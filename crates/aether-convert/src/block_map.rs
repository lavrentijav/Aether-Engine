//! Maps Anvil block palette names (`minecraft:stone`, …) to dense engine
//! [`BlockStateId`]s and infers the [`BlockProperties`] that drive the SoA
//! masks.
//!
//! Phase 1 needs a *stable, deterministic* mapping, not a perfect Vanilla
//! registry: air-like blocks become air, a small keyword set is flagged as
//! redstone / non-collidable, and everything else is treated as a plain solid
//! block. Unknown names are assigned fresh ids on first sight so no block is
//! lost — the exact numeric ids are an internal detail recorded in the report.

use aether_world::{BlockProperties, BlockStateId};
use std::collections::HashMap;

/// Anything that can resolve a block name to an engine id + properties.
///
/// Implemented directly by [`BlockRegistry`] for single-threaded use and by the
/// thread-local cache in [`crate::convert`] for parallel conversion.
pub trait Interner {
    /// Resolve (interning on first sight) a block name.
    fn intern(&mut self, name: &str) -> (BlockStateId, BlockProperties);
}

/// Interns block names into dense engine ids and infers their properties.
pub struct BlockRegistry {
    ids: HashMap<String, BlockStateId>,
    names: Vec<String>,
    props: Vec<BlockProperties>,
    next_id: u32,
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRegistry {
    /// A registry pre-seeded with air at id 0.
    pub fn new() -> Self {
        let mut r = Self {
            ids: HashMap::new(),
            names: Vec::new(),
            props: Vec::new(),
            next_id: 0,
        };
        // Reserve id 0 for air so it matches the engine's default fill.
        r.ids.insert("minecraft:air".to_string(), BlockStateId::AIR);
        r.names.push("minecraft:air".to_string());
        r.props.push(BlockProperties::AIR);
        r.next_id = 1;
        r
    }

    /// Infer properties from a block's base name (namespace stripped).
    fn infer_props(name: &str) -> BlockProperties {
        let base = name.split(':').next_back().unwrap_or(name);
        if base.contains("air") || base == "void_air" || base == "cave_air" {
            return BlockProperties::AIR;
        }
        // Non-solid, non-colliding decoration / plants / fluids-ish. Grass
        // *plants* are matched exactly so solid `grass_block` stays collidable.
        let non_solid = base.ends_with("water")
            || base.ends_with("lava")
            || base.contains("sapling")
            || base.contains("torch")
            || base.contains("rail")
            || base == "vine"
            || base == "grass"
            || base == "tall_grass"
            || base == "short_grass"
            || base == "seagrass"
            || base == "fern"
            || base == "large_fern"
            || base.contains("flower")
            || base.contains("carpet");
        let redstone = base.contains("redstone")
            || base.contains("repeater")
            || base.contains("comparator")
            || base.contains("piston")
            || base.contains("observer")
            || base.contains("lever")
            || base.contains("button")
            || base.contains("pressure_plate")
            || base.contains("_torch") && base.contains("redstone")
            || base == "target"
            || base == "lightning_rod";
        BlockProperties {
            solid: !non_solid,
            collision: !non_solid,
            redstone,
        }
    }

    /// Get (or assign) the engine id and properties for a block name.
    pub fn intern(&mut self, name: &str) -> (BlockStateId, BlockProperties) {
        if let Some(&id) = self.ids.get(name) {
            return (id, self.props[id.raw() as usize]);
        }
        let id = BlockStateId(self.next_id);
        self.next_id += 1;
        let props = Self::infer_props(name);
        self.ids.insert(name.to_string(), id);
        self.names.push(name.to_string());
        self.props.push(props);
        (id, props)
    }

    /// Number of distinct block names seen (including air).
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether only the seeded air entry exists.
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }

    /// Iterate `(name, id)` pairs in id order — used to emit the mapping table.
    pub fn mapping(&self) -> impl Iterator<Item = (&str, BlockStateId)> {
        self.names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), BlockStateId(i as u32)))
    }
}

impl Interner for BlockRegistry {
    fn intern(&mut self, name: &str) -> (BlockStateId, BlockProperties) {
        BlockRegistry::intern(self, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_is_always_zero() {
        let mut r = BlockRegistry::new();
        let (id, props) = r.intern("minecraft:air");
        assert_eq!(id, BlockStateId::AIR);
        assert_eq!(props, BlockProperties::AIR);
        assert_eq!(r.intern("minecraft:cave_air").1, BlockProperties::AIR);
    }

    #[test]
    fn ids_are_dense_and_stable() {
        let mut r = BlockRegistry::new();
        let stone = r.intern("minecraft:stone").0;
        let dirt = r.intern("minecraft:dirt").0;
        assert_eq!(stone, BlockStateId(1));
        assert_eq!(dirt, BlockStateId(2));
        // Re-interning returns the same id.
        assert_eq!(r.intern("minecraft:stone").0, stone);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn property_inference() {
        let mut r = BlockRegistry::new();
        assert!(r.intern("minecraft:stone").1.solid);
        assert!(r.intern("minecraft:redstone_wire").1.redstone);
        assert!(r.intern("minecraft:repeater").1.redstone);
        assert!(!r.intern("minecraft:water").1.collision);
        assert!(!r.intern("minecraft:torch").1.solid);
        // grass_block is solid terrain; tall_grass is a passable plant.
        assert!(r.intern("minecraft:grass_block").1.solid);
        assert!(!r.intern("minecraft:tall_grass").1.collision);
    }
}
