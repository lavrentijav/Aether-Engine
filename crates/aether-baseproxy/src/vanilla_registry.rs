//! Mapping from vanilla **protocol block-state ids** to engine block ids.
//!
//! Over the network a chunk section identifies blocks by numeric block-state id
//! (the flattened registry index the client and server agree on). The proxy
//! needs to turn those into engine [`BlockStateId`]s. Because the vanilla
//! registry has tens of thousands of states, callers register the states they
//! care about by name; anything unregistered falls back to a single interned
//! "unknown" solid block so the world is never left with accidental holes.
//!
//! Vanilla state id `0` is always air and is mapped as such unconditionally.

use aether_world::registry::BlockRegistry;
use aether_world::{BlockProperties, BlockStateId};
use std::collections::HashMap;

/// Translates vanilla protocol block-state ids to engine ids + properties.
pub struct VanillaRegistry {
    blocks: BlockRegistry,
    map: HashMap<u32, (BlockStateId, BlockProperties)>,
    unknown: (BlockStateId, BlockProperties),
}

impl Default for VanillaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl VanillaRegistry {
    /// A registry that knows only air; register the rest with [`Self::register`].
    pub fn new() -> Self {
        let mut blocks = BlockRegistry::new();
        let unknown = blocks.intern_full("aether:unknown");
        let mut map = HashMap::new();
        map.insert(0u32, (BlockStateId::AIR, BlockProperties::AIR));
        Self {
            blocks,
            map,
            unknown,
        }
    }

    /// Map vanilla protocol `state_id` to the engine block named `name`
    /// (interning the engine id / inferring properties on first sight).
    pub fn register(&mut self, state_id: u32, name: &str) {
        let full = self.blocks.intern_full(name);
        self.map.insert(state_id, full);
    }

    /// Bulk-register `(state_id, name)` pairs.
    pub fn register_all<'a, I>(&mut self, pairs: I)
    where
        I: IntoIterator<Item = (u32, &'a str)>,
    {
        for (id, name) in pairs {
            self.register(id, name);
        }
    }

    /// Resolve a vanilla `state_id` to an engine id + properties, falling back
    /// to the "unknown" solid block if it was never registered.
    pub fn resolve(&self, state_id: u32) -> (BlockStateId, BlockProperties) {
        if state_id == 0 {
            return (BlockStateId::AIR, BlockProperties::AIR);
        }
        *self.map.get(&state_id).unwrap_or(&self.unknown)
    }

    /// The engine registry behind this proxy (for name lookups).
    pub fn blocks(&self) -> &BlockRegistry {
        &self.blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_is_always_zero() {
        let r = VanillaRegistry::new();
        assert_eq!(r.resolve(0).0, BlockStateId::AIR);
    }

    #[test]
    fn registered_states_resolve_by_name() {
        let mut r = VanillaRegistry::new();
        r.register(1, "minecraft:stone");
        let (id, props) = r.resolve(1);
        assert_eq!(r.blocks().name_of(id), Some("minecraft:stone"));
        assert!(props.solid);
    }

    #[test]
    fn unknown_states_fall_back_to_solid() {
        let r = VanillaRegistry::new();
        let (_, props) = r.resolve(9999);
        assert!(props.solid, "unknown blocks should be solid, not holes");
    }
}
