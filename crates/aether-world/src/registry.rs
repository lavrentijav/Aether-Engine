//! Canonical block registry.
//!
//! The engine assigns its own **dense, stable** [`BlockStateId`]s. A small set
//! of well-known blocks get fixed ids (see [`ids`]) so world generation, the
//! vanilla proxy and tests can refer to them by name; any other block name is
//! interned on first sight. Each entry carries the [`BlockProperties`] that
//! drive the SoA masks.
//!
//! This is the *engine-side* registry. The offline `aether-convert` tool keeps
//! its own mapping for migration; both agree on air = id 0.

use crate::block::{BlockProperties, BlockStateId};
use std::collections::HashMap;

/// Fixed ids for the well-known blocks the registry always seeds.
pub mod ids {
    use crate::block::BlockStateId;
    /// `minecraft:air`
    pub const AIR: BlockStateId = BlockStateId(0);
    /// `minecraft:stone`
    pub const STONE: BlockStateId = BlockStateId(1);
    /// `minecraft:dirt`
    pub const DIRT: BlockStateId = BlockStateId(2);
    /// `minecraft:grass_block`
    pub const GRASS_BLOCK: BlockStateId = BlockStateId(3);
    /// `minecraft:bedrock`
    pub const BEDROCK: BlockStateId = BlockStateId(4);
    /// `minecraft:water`
    pub const WATER: BlockStateId = BlockStateId(5);
    /// `minecraft:sand`
    pub const SAND: BlockStateId = BlockStateId(6);
    /// `minecraft:gravel`
    pub const GRAVEL: BlockStateId = BlockStateId(7);
    /// `minecraft:oak_log`
    pub const OAK_LOG: BlockStateId = BlockStateId(8);
    /// `minecraft:oak_leaves`
    pub const OAK_LEAVES: BlockStateId = BlockStateId(9);
    /// `minecraft:redstone_wire`
    pub const REDSTONE_WIRE: BlockStateId = BlockStateId(10);
}

// (name, properties) in id order. Index == numeric id.
const SEED: &[(&str, BlockProperties)] = &[
    ("minecraft:air", BlockProperties::AIR),
    ("minecraft:stone", BlockProperties::SOLID),
    ("minecraft:dirt", BlockProperties::SOLID),
    ("minecraft:grass_block", BlockProperties::SOLID),
    ("minecraft:bedrock", BlockProperties::SOLID),
    // Water: no collision, not a full opaque cube.
    (
        "minecraft:water",
        BlockProperties {
            solid: false,
            collision: false,
            redstone: false,
        },
    ),
    ("minecraft:sand", BlockProperties::SOLID),
    ("minecraft:gravel", BlockProperties::SOLID),
    ("minecraft:oak_log", BlockProperties::SOLID),
    // Leaves: collidable but not a full opaque cube.
    (
        "minecraft:oak_leaves",
        BlockProperties {
            solid: false,
            collision: true,
            redstone: false,
        },
    ),
    // Redstone wire: no collision, carries power.
    (
        "minecraft:redstone_wire",
        BlockProperties {
            solid: false,
            collision: false,
            redstone: true,
        },
    ),
];

/// Maps block names ⇄ dense engine ids and stores per-block properties.
#[derive(Debug, Clone)]
pub struct BlockRegistry {
    names: Vec<String>,
    props: Vec<BlockProperties>,
    lookup: HashMap<String, BlockStateId>,
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRegistry {
    /// A registry seeded with the well-known blocks at their fixed [`ids`].
    pub fn new() -> Self {
        let mut r = Self {
            names: Vec::with_capacity(SEED.len()),
            props: Vec::with_capacity(SEED.len()),
            lookup: HashMap::with_capacity(SEED.len()),
        };
        for (name, props) in SEED {
            let id = BlockStateId(r.names.len() as u32);
            r.names.push((*name).to_owned());
            r.props.push(*props);
            r.lookup.insert((*name).to_owned(), id);
        }
        r
    }

    /// Properties inferred for an unknown block name (same heuristic family the
    /// migration tool uses): air-like → air, fluids/plants → non-solid, the
    /// redstone family → redstone, otherwise a plain solid block.
    fn infer(name: &str) -> BlockProperties {
        let base = name.split(':').next_back().unwrap_or(name);
        // Match air variants exactly: a `contains("air")` would also catch
        // names like `oak_stairs`.
        if matches!(base, "air" | "void_air" | "cave_air") {
            return BlockProperties::AIR;
        }
        // Note: match grass *plants* exactly so solid terrain like `grass_block`
        // is not misclassified by a broad `contains("grass")`.
        let non_solid = base.ends_with("water")
            || base.ends_with("lava")
            || base.contains("sapling")
            || base.contains("torch")
            || base.contains("rail")
            || base.contains("flower")
            || base == "grass"
            || base == "tall_grass"
            || base == "short_grass"
            || base == "seagrass"
            || base == "fern"
            || base == "large_fern"
            || base.contains("carpet");
        let redstone = base.contains("redstone")
            || base.contains("repeater")
            || base.contains("comparator")
            || base.contains("piston")
            || base.contains("observer")
            || base.contains("lever")
            || base.contains("button")
            || base.contains("pressure_plate");
        BlockProperties {
            solid: !non_solid,
            collision: !non_solid,
            redstone,
        }
    }

    /// Resolve `name` to its id, interning (with inferred properties) if new.
    pub fn intern(&mut self, name: &str) -> BlockStateId {
        if let Some(&id) = self.lookup.get(name) {
            return id;
        }
        let id = BlockStateId(self.names.len() as u32);
        self.names.push(name.to_owned());
        self.props.push(Self::infer(name));
        self.lookup.insert(name.to_owned(), id);
        id
    }

    /// Resolve `name` to id + properties, interning if new.
    pub fn intern_full(&mut self, name: &str) -> (BlockStateId, BlockProperties) {
        let id = self.intern(name);
        (id, self.props[id.raw() as usize])
    }

    /// Look up an already-known name (no interning).
    pub fn get(&self, name: &str) -> Option<BlockStateId> {
        self.lookup.get(name).copied()
    }

    /// Name of a block id, if in range.
    pub fn name_of(&self, id: BlockStateId) -> Option<&str> {
        self.names.get(id.raw() as usize).map(String::as_str)
    }

    /// Properties of a block id (defaults to air's if out of range).
    pub fn props_of(&self, id: BlockStateId) -> BlockProperties {
        self.props
            .get(id.raw() as usize)
            .copied()
            .unwrap_or(BlockProperties::AIR)
    }

    /// Number of registered blocks.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the registry is empty (never true after [`BlockRegistry::new`]).
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_ids_are_fixed() {
        let r = BlockRegistry::new();
        // Every seeded id is part of the persisted world-storage contract, so
        // check the whole table to catch any reordering of `SEED`.
        for (name, id) in [
            ("minecraft:air", ids::AIR),
            ("minecraft:stone", ids::STONE),
            ("minecraft:dirt", ids::DIRT),
            ("minecraft:grass_block", ids::GRASS_BLOCK),
            ("minecraft:bedrock", ids::BEDROCK),
            ("minecraft:water", ids::WATER),
            ("minecraft:sand", ids::SAND),
            ("minecraft:gravel", ids::GRAVEL),
            ("minecraft:oak_log", ids::OAK_LOG),
            ("minecraft:oak_leaves", ids::OAK_LEAVES),
            ("minecraft:redstone_wire", ids::REDSTONE_WIRE),
        ] {
            assert_eq!(r.get(name), Some(id), "name->id for {name}");
            assert_eq!(r.name_of(id), Some(name), "id->name for {name}");
        }
        assert!(!r.props_of(ids::WATER).collision);
        assert!(r.props_of(ids::STONE).solid);
    }

    #[test]
    fn unknown_blocks_intern_after_seed() {
        let mut r = BlockRegistry::new();
        let base = r.len();
        let id = r.intern("modid:fancy_block");
        assert_eq!(id, BlockStateId(base as u32));
        assert!(r.props_of(id).solid);
        // Idempotent.
        assert_eq!(r.intern("modid:fancy_block"), id);
        assert_eq!(r.len(), base + 1);
    }

    #[test]
    fn inference_matches_family() {
        let mut r = BlockRegistry::new();
        assert_eq!(r.intern_full("minecraft:cave_air").1, BlockProperties::AIR);
        assert!(!r.intern_full("minecraft:cobblestone").1.redstone);
        assert!(r.intern_full("minecraft:cobblestone").1.solid);
        assert!(r.intern_full("minecraft:sticky_piston").1.redstone);
        assert!(!r.intern_full("minecraft:tall_grass").1.collision);
        // `grass_block` is solid terrain, not a plant.
        assert!(r.intern_full("modid:grass_block_variant").1.solid);
        let gb = BlockRegistry::infer("minecraft:grass_block");
        assert!(gb.solid && gb.collision);
        // `*_stairs` contains "air" but must stay solid/collidable.
        assert!(BlockRegistry::infer("minecraft:oak_stairs").collision);
    }
}
