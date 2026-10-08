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

    /// Properties for a block name.
    ///
    /// The vanilla table first — it is the real data, and it covers every
    /// block a vanilla world can contain, which is all this tool ever reads.
    /// The name heuristic below it survives only for modded blocks, where
    /// there is nothing else to go on.
    ///
    /// This used to be a heuristic for *everything*, duplicated from the
    /// engine registry's own. Two independent guesses at the same question
    /// disagree eventually, and the disagreement shows up as a converted world
    /// whose collision differs from a generated one.
    fn infer_props(name: &str) -> BlockProperties {
        // A vanilla name may carry state properties; the table is keyed by the
        // block, not the state.
        let bare = name.split_once('[').map_or(name, |(n, _)| n);
        // A vanilla name — bare or fully qualified — is answered by the state
        // table. Anvil stores fully-qualified states, so the qualified form is
        // the common case here, not the exception.
        use aether_world::registry::blocks;
        if let Some(state) = blocks::default_state(name).or_else(|| blocks::default_state(bare)) {
            if let Some(p) = blocks::props_of_state(state) {
                return p;
            }
        }
        Self::guess_props(bare)
    }

    /// Last-resort properties for a name no table contains — a modded block.
    fn guess_props(name: &str) -> BlockProperties {
        let base = name.split(':').next_back().unwrap_or(name);
        // Match air variants exactly: a `contains("air")` would also catch
        // `st`+`air`+`s` and similar names.
        if matches!(base, "air" | "void_air" | "cave_air") {
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
            || base == "target"
            || base == "lightning_rod";
        BlockProperties {
            solid: !non_solid,
            collision: !non_solid,
            redstone,
            light_emission: 0,
            // An unknown block blocks light rather than leaking it: one that
            // turns out to be a lamp is a cosmetic error, one that turns out
            // to be a wall is a hole in every light calculation around it.
            light_opacity: if non_solid { 0 } else { 15 },
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
    fn vanilla_blocks_take_their_real_properties() {
        // These come from the block table now, not from a guess at the name.
        // Two of them changed answer when it switched over, and both were
        // *wrong* before: a stair and a slab are collidable but are not full
        // opaque cubes, so calling them `solid` made them occlude light and
        // hide the faces of whatever is behind them.
        let mut r = BlockRegistry::new();
        assert!(r.intern("minecraft:stone").1.solid);
        assert!(r.intern("minecraft:redstone_wire").1.redstone);
        assert!(r.intern("minecraft:repeater").1.redstone);
        assert!(!r.intern("minecraft:water").1.collision);
        assert!(!r.intern("minecraft:torch").1.solid);
        assert!(r.intern("minecraft:grass_block").1.solid);
        assert!(!r.intern("minecraft:tall_grass").1.collision);

        // Collidable, and *not* a full cube.
        for partial in ["minecraft:oak_stairs", "minecraft:oak_slab", "minecraft:oak_fence"] {
            let p = r.intern(partial).1;
            assert!(p.collision, "{partial} must collide");
            assert!(!p.solid, "{partial} is not a full opaque cube");
        }

        // And the light data the table brought with it.
        assert_eq!(r.intern("minecraft:glowstone").1.light_emission, 15);
        assert_eq!(r.intern("minecraft:water").1.light_opacity, 1);
    }

    #[test]
    fn a_modded_name_still_falls_back_to_the_guess() {
        // The heuristic is not gone, it is demoted: it now only runs for names
        // no vanilla table contains.
        let mut r = BlockRegistry::new();
        assert!(r.intern("modid:fancy_block").1.solid);
        assert!(!r.intern("modid:fancy_torch").1.collision);
    }
}
