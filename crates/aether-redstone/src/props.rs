//! Reading and writing block-state properties through `aether-world`.
//!
//! There is **no second property system here**. A block state's properties are
//! the ones the registry already knows: the same mixed-radix decomposition in
//! [`aether_world::registry::props`] that turns a flattened state id back into
//! `power=7`, `facing=east`, and so on. This module is a thin, redstone-shaped
//! veneer over that code so the engine can say "set `power` to 9" without
//! re-deriving the schema.

use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

/// The block a state belongs to, as `(block id, block name)`.
pub fn block_of(state: BlockStateId) -> Option<(u16, &'static str)> {
    blocks::block_of_state(state)
}

/// The name of the *block* a state belongs to, without properties.
pub fn name_of(state: BlockStateId) -> Option<&'static str> {
    block_of(state).map(|(_, name)| name)
}

/// `(block id, offset of this state within the block's state range)`.
fn decompose(state: BlockStateId) -> Option<(u16, u32)> {
    let (block, _) = block_of(state)?;
    let (lo, _) = blocks::STATE_RANGE[block as usize];
    Some((block, state.raw() - lo))
}

/// The value of property `key` in `state`, or `None` if the block lacks it.
pub fn get(state: BlockStateId, key: &str) -> Option<&'static str> {
    let (block, offset) = decompose(state)?;
    props::values_of(block, offset)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// A copy of `state` with property `key` set to `value`.
///
/// Every other property keeps the value it had in `state` (not the block's
/// default), which is what makes this safe to call on a wire that already has
/// its four connection sides worked out. `None` if the block has no such
/// property, or the value is not one it takes.
pub fn with(state: BlockStateId, key: &str, value: &str) -> Option<BlockStateId> {
    let (block, offset) = decompose(state)?;
    let mut values: Vec<(&str, &str)> = props::values_of(block, offset);
    let slot = values.iter_mut().find(|(k, _)| *k == key)?;
    slot.1 = value;
    props::state_with(block, &values).map(BlockStateId)
}

/// Whether a state's block has a property named `key`.
pub fn has(state: BlockStateId, key: &str) -> bool {
    get(state, key).is_some()
}

/// A boolean property's value, defaulting to `false` if the block lacks it.
pub fn bool_or(state: BlockStateId, key: &str, default: bool) -> bool {
    match get(state, key) {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    }
}

/// An integer property's value, defaulting to `0` if the block lacks it.
pub fn int_or(state: BlockStateId, key: &str, default: u8) -> u8 {
    get(state, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use aether_world::BlockRegistry;

    use super::*;

    #[test]
    fn a_partial_property_change_keeps_the_rest() {
        let reg = BlockRegistry::new();
        let wire = reg
            .get("minecraft:redstone_wire[north=side,south=side,power=3]")
            .unwrap();
        let more = with(wire, "power", "9").unwrap();
        assert_eq!(get(more, "power"), Some("9"));
        assert_eq!(get(more, "north"), Some("side"));
        assert_eq!(get(more, "south"), Some("side"));
        assert_eq!(get(more, "east"), Some("none"));
    }

    #[test]
    fn a_property_a_block_lacks_is_refused() {
        let reg = BlockRegistry::new();
        let stone = reg.get("minecraft:stone").unwrap();
        assert_eq!(get(stone, "power"), None);
        assert_eq!(with(stone, "power", "9"), None);
    }
}
