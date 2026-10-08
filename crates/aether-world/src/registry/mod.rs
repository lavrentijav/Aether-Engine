//! Canonical block registry.
//!
//! The engine assigns its own **dense, stable** [`BlockStateId`]s, and they
//! are vanilla 1.21.11's block ids: every block that version has is registered
//! up front, in its order, with its real properties (see [`blocks`]). Names the
//! table does not contain — a modded block — are still interned on first sight,
//! but that is now the exception rather than the rule it used to be.
//!
//! Each entry carries the [`BlockProperties`] that drive the SoA masks and the
//! light propagation.
//!
//! # The three places a block's properties live
//!
//! 1. **In the sub-chunk**, as SoA bit masks (`solid`, `collision`,
//!    `redstone`), written when a block is placed. This is what physics and
//!    occlusion actually read: one Morton-indexed bit per cell, no indirection.
//! 2. **Here**, as a `Vec<BlockProperties>` indexed by id — a few kilobytes for
//!    the whole vanilla set, so the hot path is an L1-resident array index.
//! 3. **On disk**, as the name table saved beside the world, which is what
//!    makes an id mean the same block after a restart.
//!
//! Tier 1 is a *derivation* of tier 2 and tier 2 of tier 3; only tier 3 is
//! authoritative. That ordering is the reason the vanilla table matters: a
//! wrong property in tier 2 is copied into tier 1 at placement time and
//! persisted there, where nothing ever revisits it.
//!
//! This is the *engine-side* registry. The offline `aether-convert` tool keeps
//! its own mapping for migration; both agree on air = id 0.

use crate::block::{BlockProperties, BlockStateId};
use std::collections::HashMap;

pub mod blocks;
pub mod props;

/// Fixed ids for the blocks the engine itself refers to by name.
pub mod ids {
    //! Default states of the blocks the engine itself refers to by name.
    //!
    //! These are vanilla 1.21.11 **state** ids, not block ids — see
    //! `docs/DESIGN_NOTES.md` §9. They are safe as compile-time constants
    //! because the id space is fixed and complete: it comes from a pinned
    //! version rather than from whatever a session happened to intern first.

    use super::BlockStateId;

    /// `minecraft:air` in its default state.
    pub const AIR: BlockStateId = BlockStateId(0);
    /// `minecraft:stone` in its default state.
    pub const STONE: BlockStateId = BlockStateId(1);
    /// `minecraft:dirt` in its default state.
    pub const DIRT: BlockStateId = BlockStateId(10);
    /// `minecraft:grass_block` in its default state.
    pub const GRASS_BLOCK: BlockStateId = BlockStateId(9);
    /// `minecraft:bedrock` in its default state.
    pub const BEDROCK: BlockStateId = BlockStateId(85);
    /// `minecraft:water` in its default state.
    pub const WATER: BlockStateId = BlockStateId(86);
    /// `minecraft:sand` in its default state.
    pub const SAND: BlockStateId = BlockStateId(118);
    /// `minecraft:gravel` in its default state.
    pub const GRAVEL: BlockStateId = BlockStateId(124);
    /// `minecraft:oak_log` in its default state.
    pub const OAK_LOG: BlockStateId = BlockStateId(137);
    /// `minecraft:oak_leaves` in its default state.
    pub const OAK_LEAVES: BlockStateId = BlockStateId(279);
    /// `minecraft:redstone_wire` in its default state.
    pub const REDSTONE_WIRE: BlockStateId = BlockStateId(4970);
}

// (name, properties) in id order. Index == numeric id.
/// The blocks every world starts with: the entire vanilla set.
///
/// Was a hand-written list of eleven, with anything else's properties guessed
/// from its name at first sighting. See [`blocks`] for why that was wrong in
/// two directions at once.
pub use blocks::BLOCKS as SEED;

/// Maps block names ⇄ dense engine ids and stores per-block properties.
#[derive(Debug, Clone)]
pub struct BlockRegistry {
    /// Names of blocks beyond the vanilla table, in id order starting at
    /// [`BEYOND`].
    names: Vec<String>,
    props: Vec<BlockProperties>,
    lookup: HashMap<String, BlockStateId>,
}

/// The first id that is not a vanilla block state.
///
/// Everything below is answered from the generated tables; everything at or
/// above is a modded block this build has interned, and is what a saved world's
/// name table has to carry.
pub const BEYOND: u32 = blocks::STATE_COUNT as u32;

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRegistry {
    /// A registry seeded with the well-known blocks at their fixed [`ids`].
    pub fn new() -> Self {
        let r = Self {
            names: Vec::with_capacity(SEED.len()),
            props: Vec::with_capacity(SEED.len()),
            lookup: HashMap::with_capacity(SEED.len()),
        };
        // Nothing is copied into `names`/`props` for the vanilla set: those
        // are compile-time tables, identical in every build that pins this
        // version, and duplicating 29,671 entries into a HashMap at startup
        // would cost megabytes to answer a question an array index already
        // answers. The `Vec`s hold *only* what is interned beyond the table —
        // modded blocks — and `BEYOND` is where their ids start.
        r
    }

    /// Properties for a name the vanilla table does not contain.
    ///
    /// Only reachable for a block this build has never heard of — a modded
    /// one, or a state string the client invented. A plain solid cube is the
    /// least surprising answer: it collides and it blocks light, so an unknown
    /// block cannot become an invisible hole in the world or a light leak.
    fn unknown() -> BlockProperties {
        BlockProperties {
            solid: true,
            collision: true,
            redstone: false,
            light_emission: 0,
            light_opacity: 15,
        }
    }

    /// Resolve `name` to its id, interning (with inferred properties) if new.
    pub fn intern(&mut self, name: &str) -> BlockStateId {
        if let Some(id) = self.get(name) {
            return id;
        }
        let id = BlockStateId(BEYOND + self.names.len() as u32);
        self.names.push(name.to_owned());
        self.props.push(Self::unknown());
        self.lookup.insert(name.to_owned(), id);
        id
    }

    /// Resolve `name` to id + properties, interning if new.
    pub fn intern_full(&mut self, name: &str) -> (BlockStateId, BlockProperties) {
        let id = self.intern(name);
        (id, self.props_of(id))
    }

    /// Look up an already-known name (no interning).
    ///
    /// Accepts both a bare block name — which resolves to that block's
    /// **default** state — and a fully-qualified state name such as
    /// `minecraft:oak_stairs[facing=east,half=top,shape=straight,waterlogged=false]`.
    pub fn get(&self, name: &str) -> Option<BlockStateId> {
        if let Some(id) = blocks::default_state(name) {
            return Some(id);
        }
        if name.contains('[') {
            if let Some((base, rest)) = name.split_once('[') {
                if let Some(block) = blocks::block_id_of(base) {
                    let pairs: Vec<(&str, &str)> = rest
                        .trim_end_matches(']')
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .filter_map(|kv| kv.split_once('='))
                        .collect();
                    if let Some(state) = props::state_with(block, &pairs) {
                        return Some(BlockStateId(state));
                    }
                }
            }
        }
        self.lookup.get(name).copied()
    }

    /// Name of a block state.
    ///
    /// Vanilla states get their full name, properties and all, reconstructed
    /// from the schema rather than looked up — see [`props`].
    pub fn name_of(&self, id: BlockStateId) -> Option<String> {
        if id.raw() < BEYOND {
            return props::state_name(id.raw());
        }
        self.names.get((id.raw() - BEYOND) as usize).cloned()
    }

    /// The *block* a state belongs to, without its properties.
    ///
    /// What a human wants to read in a rollback report: "oak_stairs", not
    /// "oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]".
    pub fn block_name_of(&self, id: BlockStateId) -> Option<&'static str> {
        blocks::block_of_state(id).map(|(_, n)| n)
    }

    /// Properties of a block state.
    ///
    /// One indexed load for anything vanilla; the `Vec` is consulted only for
    /// modded ids.
    pub fn props_of(&self, id: BlockStateId) -> BlockProperties {
        if let Some(p) = blocks::props_of_state(id) {
            return p;
        }
        self.props
            .get((id.raw().saturating_sub(BEYOND)) as usize)
            .copied()
            .unwrap_or(BlockProperties::AIR)
    }

    /// Every *interned* name, in id order starting at [`BEYOND`].
    ///
    /// Vanilla states are absent on purpose: they are a compile-time table, so
    /// there is nothing about them a save needs to carry. This used to be
    /// thousands of names long and *was* the save-format contract; it is now
    /// usually empty.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Rebuild a registry from a saved [`BlockRegistry::names`] table.
    ///
    /// The table holds only modded blocks now, so restoring is re-interning
    /// them in order. A world written against a different *version pin* is a
    /// different question, and is caught by the pin recorded beside the table
    /// rather than by inspecting names.
    pub fn restore(table: &[String]) -> Option<Self> {
        let mut r = Self::new();
        for name in table {
            r.intern(name);
        }
        Some(r)
    }

    /// Number of addressable block states: the whole vanilla set plus
    /// anything interned beyond it.
    pub fn len(&self) -> usize {
        BEYOND as usize + self.names.len()
    }

    /// Never true: the vanilla table is always present.
    pub fn is_empty(&self) -> bool {
        false
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
            // A state's name carries its properties; the block name does not.
            assert_eq!(r.block_name_of(id), Some(name), "id->name for {name}");
            assert!(
                r.name_of(id).unwrap().starts_with(name),
                "{name}: full state name should begin with the block name"
            );
        }
        assert!(!r.props_of(ids::WATER).collision);
        assert!(r.props_of(ids::STONE).solid);
    }

    #[test]
    fn unknown_blocks_intern_beyond_the_state_space() {
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
    fn restore_reproduces_modded_ids_exactly() {
        let mut original = BlockRegistry::new();
        let a = original.intern("modid:alpha");
        let b = original.intern("modid:beta");

        let restored = BlockRegistry::restore(original.names()).expect("a table we just wrote");
        // Same names, same ids, in the order they were first met — which is
        // what the stored sub-chunk ids refer to.
        assert_eq!(restored.get("modid:alpha"), Some(a));
        assert_eq!(restored.get("modid:beta"), Some(b));
        assert_eq!(restored.len(), original.len());
    }

    #[test]
    fn the_saved_table_holds_only_what_is_not_vanilla() {
        // This used to be thousands of names and *was* the save-format
        // contract: growing the seed by one entry invalidated every world.
        // With a fixed, complete state space there is nothing about a vanilla
        // block a save needs to carry.
        let mut r = BlockRegistry::new();
        assert!(r.names().is_empty(), "a fresh world saves no names");
        r.intern("minecraft:stone");
        r.intern("minecraft:oak_stairs");
        assert!(r.names().is_empty(), "vanilla names are never interned");

        let modded = r.intern("modid:fancy_block");
        assert!(modded.raw() >= BEYOND);
        assert_eq!(r.names(), ["modid:fancy_block"]);

        let restored = BlockRegistry::restore(r.names()).unwrap();
        assert_eq!(restored.get("modid:fancy_block"), Some(modded));
    }

    #[test]
    fn the_whole_vanilla_state_set_is_addressable() {
        let r = BlockRegistry::new();
        assert_eq!(r.len(), blocks::STATE_COUNT);
        assert!(r.len() > 29_000, "got {} states", r.len());
        // Items are deliberately absent: an item never occupies a cell.
        assert_eq!(r.get("minecraft:diamond_sword"), None);
        assert_eq!(r.get("minecraft:stick"), None);
    }

    #[test]
    fn a_bare_block_name_resolves_to_its_default_state() {
        // The anchor the codecs translate through. If this drifts, every
        // client renders every block as something else.
        let r = BlockRegistry::new();
        assert_eq!(r.get("minecraft:air"), Some(BlockStateId(0)));
        assert_eq!(r.get("minecraft:stone"), Some(BlockStateId(1)));
        for (i, row) in blocks::BLOCKS.iter().enumerate() {
            assert_eq!(
                r.get(row.0),
                Some(BlockStateId(blocks::DEFAULT_STATE[i])),
                "{}",
                row.0
            );
        }
    }

    #[test]
    fn a_qualified_state_name_round_trips() {
        // The property that makes states usable at all: a name goes to an id
        // and the id comes back as the same name, for every state there is.
        let r = BlockRegistry::new();
        for state in 0..blocks::STATE_COUNT as u32 {
            let id = BlockStateId(state);
            let name = r.name_of(id).expect("every state has a name");
            assert_eq!(r.get(&name), Some(id), "{name} did not round-trip");
        }
    }

    #[test]
    fn properties_can_be_named_partially_and_in_any_order() {
        // `state("water", &[("level", "0")])` has to mean what it looks like:
        // everything unnamed keeps its default. Otherwise every caller has to
        // spell out all four properties of a stair to change one.
        let r = BlockRegistry::new();
        let block = blocks::block_id_of("minecraft:oak_stairs").unwrap();
        let east = props::state_with(block, &[("facing", "east")]).unwrap();
        let name = r.name_of(BlockStateId(east)).unwrap();
        assert!(name.contains("facing=east"), "{name}");
        assert!(name.contains("half=bottom"), "unnamed properties keep the default: {name}");

        // Order must not matter.
        let a = props::state_with(block, &[("facing", "west"), ("half", "top")]);
        let b = props::state_with(block, &[("half", "top"), ("facing", "west")]);
        assert_eq!(a, b);
        assert!(a.is_some());
    }

    #[test]
    fn a_property_or_value_a_block_lacks_is_refused_rather_than_guessed() {
        let block = blocks::block_id_of("minecraft:oak_stairs").unwrap();
        assert_eq!(props::state_with(block, &[("colour", "red")]), None);
        assert_eq!(props::state_with(block, &[("facing", "upside")]), None);
    }

    #[test]
    fn properties_come_from_the_real_table_not_from_the_name() {
        // Each of these was wrong under the old name heuristic, and each was
        // wrong in a way that got baked into a sub-chunk mask and persisted.
        let r = BlockRegistry::new();
        let p = |n: &str| r.props_of(r.get(n).unwrap_or_else(|| panic!("no {n}")));

        // Glass is a full cube you can see through: it collides, it is not
        // opaque. The heuristic called it solid.
        let glass = p("minecraft:glass");
        assert!(glass.collision && !glass.solid);
        assert_eq!(glass.light_opacity, 0);

        // A slab is a collidable half-block, not a full opaque cube.
        let slab = p("minecraft:oak_slab");
        assert!(slab.collision && !slab.solid);

        // A fence collides but does not fill its cell.
        assert!(p("minecraft:oak_fence").collision);
        assert!(!p("minecraft:oak_fence").solid);

        // Water is neither solid nor collidable, and dims light by one.
        let water = p("minecraft:water");
        assert!(!water.solid && !water.collision);
        assert_eq!(water.light_opacity, 1);

        // Stairs contain the letters "air" and are solid terrain.
        assert!(p("minecraft:oak_stairs").collision);
    }

    #[test]
    fn light_emission_is_the_vanilla_value() {
        // What the lighting engine will read. A wrong emission here is a
        // wrong light level everywhere that block appears.
        let r = BlockRegistry::new();
        let emit = |n: &str| r.props_of(r.get(n).unwrap()).light_emission;
        assert_eq!(emit("minecraft:glowstone"), 15);
        assert_eq!(emit("minecraft:sea_lantern"), 15);
        assert_eq!(emit("minecraft:lava"), 15);
        assert_eq!(emit("minecraft:torch"), 14);
        assert_eq!(emit("minecraft:stone"), 0);
        assert_eq!(emit("minecraft:air"), 0);
    }

    #[test]
    fn opacity_and_solidity_agree_where_they_must() {
        // `solid` *is* "blocks all light", so the two cannot disagree — a
        // block that is solid but transparent would be an occluder that leaks
        // light, and the masks and the light propagation would then be reading
        // two different worlds.
        for row in blocks::BLOCKS {
            let p = blocks::props_of_row(row);
            assert_eq!(
                p.solid,
                p.light_opacity == 15,
                "{} disagrees: solid={} opacity={}",
                row.0,
                p.solid,
                p.light_opacity
            );
            assert!(p.light_emission <= 15 && p.light_opacity <= 15, "{}", row.0);
        }
    }

    #[test]
    fn a_modded_block_is_assumed_solid_and_opaque() {
        // The safe way to be wrong: an unknown block that turns out to be a
        // lamp looks dull, one that turns out to be a wall is a hole in every
        // light calculation around it.
        let mut r = BlockRegistry::new();
        let (_, p) = r.intern_full("modid:mystery");
        assert!(p.solid && p.collision);
        assert_eq!(p.light_opacity, 15);
        assert_eq!(p.light_emission, 0);
    }
}
