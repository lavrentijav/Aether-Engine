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
    /// `minecraft:torch` — a non-solid block-light source (emits 14).
    pub const TORCH: BlockStateId = BlockStateId(11);
    /// `minecraft:glowstone` — an opaque block-light source (emits 15).
    pub const GLOWSTONE: BlockStateId = BlockStateId(12);
    /// `minecraft:cobblestone`
    pub const COBBLESTONE: BlockStateId = BlockStateId(13);
    /// `minecraft:oak_planks`
    pub const OAK_PLANKS: BlockStateId = BlockStateId(14);
    /// `minecraft:mossy_cobblestone`
    pub const MOSSY_COBBLESTONE: BlockStateId = BlockStateId(15);
    /// `minecraft:chest`
    pub const CHEST: BlockStateId = BlockStateId(16);
    /// `minecraft:spawner`
    pub const SPAWNER: BlockStateId = BlockStateId(17);
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
    // Torch: non-solid light source (emission inferred as 14).
    (
        "minecraft:torch",
        BlockProperties {
            solid: false,
            collision: false,
            redstone: false,
        },
    ),
    // Glowstone: full opaque cube *and* a light source (emission inferred 15).
    ("minecraft:glowstone", BlockProperties::SOLID),
    // Building blocks for clean-room procedural structures.
    ("minecraft:cobblestone", BlockProperties::SOLID),
    ("minecraft:oak_planks", BlockProperties::SOLID),
    ("minecraft:mossy_cobblestone", BlockProperties::SOLID),
    // Chest: collidable but not a full opaque cube.
    (
        "minecraft:chest",
        BlockProperties {
            solid: false,
            collision: true,
            redstone: false,
        },
    ),
    // Mob spawner: a full solid block.
    ("minecraft:spawner", BlockProperties::SOLID),
];

/// Maps block names ⇄ dense engine ids and stores per-block properties and the
/// block-light level each block emits (0 for non-emitters).
#[derive(Debug, Clone)]
pub struct BlockRegistry {
    names: Vec<String>,
    props: Vec<BlockProperties>,
    emission: Vec<u8>,
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
            emission: Vec::with_capacity(SEED.len()),
            lookup: HashMap::with_capacity(SEED.len()),
        };
        for (name, props) in SEED {
            let id = BlockStateId(r.names.len() as u32);
            r.names.push((*name).to_owned());
            r.props.push(*props);
            r.emission.push(Self::infer_emission(name));
            r.lookup.insert((*name).to_owned(), id);
        }
        r
    }

    /// Infer the block-light level a block emits from its name (Vanilla-ish).
    ///
    /// Ordering matters: more specific families (`soul_*`, `redstone_torch`,
    /// `sea_lantern`) are checked before the broader `torch` / `lantern` /
    /// `fire` catches so they are not swallowed.
    fn infer_emission(name: &str) -> u8 {
        let base = name.split(':').next_back().unwrap_or(name);
        // Full-strength (15) light sources.
        if base.ends_with("lava")
            || base == "glowstone"
            || base == "sea_lantern"
            || base == "jack_o_lantern"
            || base == "shroomlight"
            || base == "beacon"
            || base == "conduit"
            || base == "lantern"
            || base == "campfire"
            || base == "froglight"
            || base.ends_with("_froglight")
            || base == "lava_cauldron"
        {
            return 15;
        }
        // Soul variants burn dimmer (10) than their normal counterparts.
        if base.starts_with("soul_") || base == "crying_obsidian" {
            return 10;
        }
        // End rod.
        if base == "end_rod" {
            return 14;
        }
        // Redstone torches are weak; check before the generic torch match
        // (covers both `redstone_torch` and `redstone_wall_torch`).
        if base.contains("redstone") && base.contains("torch") {
            return 7;
        }
        if base.contains("torch") {
            return 14;
        }
        if base == "fire" {
            return 15;
        }
        if base == "glow_lichen" || base == "sculk_catalyst" {
            return 7;
        }
        if base == "magma_block" {
            return 3;
        }
        if base == "brewing_stand" || base == "brown_mushroom" {
            return 1;
        }
        0
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
        self.emission.push(Self::infer_emission(name));
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

    /// Block-light level emitted by a block id (`0..=15`; 0 if out of range or
    /// a non-emitter). Feeds the lighting engine's emission oracle.
    pub fn emission_of(&self, id: BlockStateId) -> u8 {
        self.emission.get(id.raw() as usize).copied().unwrap_or(0)
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
            ("minecraft:torch", ids::TORCH),
            ("minecraft:glowstone", ids::GLOWSTONE),
            ("minecraft:cobblestone", ids::COBBLESTONE),
            ("minecraft:oak_planks", ids::OAK_PLANKS),
            ("minecraft:mossy_cobblestone", ids::MOSSY_COBBLESTONE),
            ("minecraft:chest", ids::CHEST),
            ("minecraft:spawner", ids::SPAWNER),
        ] {
            assert_eq!(r.get(name), Some(id), "name->id for {name}");
            assert_eq!(r.name_of(id), Some(name), "id->name for {name}");
        }
        assert!(!r.props_of(ids::WATER).collision);
        assert!(r.props_of(ids::STONE).solid);
        // Seeded emitters carry their light level.
        assert_eq!(r.emission_of(ids::TORCH), 14);
        assert_eq!(r.emission_of(ids::GLOWSTONE), 15);
        assert!(!r.props_of(ids::TORCH).solid);
        assert!(r.props_of(ids::GLOWSTONE).solid);
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

    #[test]
    fn emission_inference_matches_light_sources() {
        let mut r = BlockRegistry::new();
        // Seeded blocks emit nothing.
        assert_eq!(r.emission_of(ids::STONE), 0);
        assert_eq!(r.emission_of(ids::REDSTONE_WIRE), 0);
        // Interned emitters get their Vanilla-ish level. `intern_full` is
        // &mut, so intern first, then read the (&self) emission.
        for (name, want) in [
            ("minecraft:glowstone", 15u8),
            ("minecraft:torch", 14),
            ("minecraft:wall_torch", 14),
            ("minecraft:lantern", 15),
            ("minecraft:sea_lantern", 15),
            ("minecraft:end_rod", 14),
            ("minecraft:magma_block", 3),
            ("minecraft:lava", 15),
            ("minecraft:cobblestone", 0),
            // More specific families win over broad matches.
            ("minecraft:redstone_torch", 7),
            ("minecraft:redstone_wall_torch", 7),
            ("minecraft:soul_torch", 10),
            ("minecraft:soul_lantern", 10),
        ] {
            let id = r.intern(name);
            assert_eq!(r.emission_of(id), want, "emission for {name}");
        }
        // Out-of-range id is safe.
        assert_eq!(r.emission_of(BlockStateId(9_999)), 0);
    }
}
