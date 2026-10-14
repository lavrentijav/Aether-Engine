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

use crate::dir::Direction;

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

/// The `power` property (0–15), or 0.
pub fn power(state: BlockStateId) -> u8 {
    int_or(state, "power", 0)
}

/// The `lit` property, or `false`.
pub fn lit(state: BlockStateId) -> bool {
    bool_or(state, "lit", false)
}

/// The `powered` property, or `false`.
pub fn powered(state: BlockStateId) -> bool {
    bool_or(state, "powered", false)
}

/// The `locked` property, or `false`.
pub fn locked(state: BlockStateId) -> bool {
    bool_or(state, "locked", false)
}

/// The `delay` property (1–4), or 1.
pub fn delay(state: BlockStateId) -> u8 {
    int_or(state, "delay", 1)
}

/// The `facing` property as a direction, defaulting to north.
pub fn facing(state: BlockStateId) -> Direction {
    match get(state, "facing") {
        Some("north") => Direction::North,
        Some("south") => Direction::South,
        Some("west") => Direction::West,
        Some("east") => Direction::East,
        _ => Direction::North,
    }
}

/// The attachment direction of a face-attached block (`getConnectedDirection`):
/// a floor block attaches upward, a ceiling block downward, a wall block
/// horizontally, and anything else falls back to `facing`.
pub fn connected_direction(state: BlockStateId) -> Direction {
    match get(state, "face") {
        Some("floor") => Direction::Up,
        Some("ceiling") => Direction::Down,
        _ => facing(state),
    }
}

/// The property name for a horizontal direction's wire side.
pub fn side_prop(dir: Direction) -> &'static str {
    match dir {
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
        _ => "north",
    }
}

/// Whether wire connects on `dir` (its `north`/`south`/`west`/`east` side is
/// not `none`).
pub fn side_connected(state: BlockStateId, dir: Direction) -> bool {
    get(state, side_prop(dir)).is_some_and(|v| v != "none")
}

/// A copy of `state` with wire side `dir` set.
pub fn set_side(state: BlockStateId, dir: Direction, value: &str) -> BlockStateId {
    with(state, side_prop(dir), value).unwrap_or(state)
}

/// Whether no side of a wire connects (`isDot`).
pub fn is_dot(state: BlockStateId) -> bool {
    Direction::HORIZONTAL
        .iter()
        .all(|&d| !side_connected(state, d))
}

/// Whether all four sides of a wire connect (`isCross`).
pub fn is_cross(state: BlockStateId) -> bool {
    Direction::HORIZONTAL
        .iter()
        .all(|&d| side_connected(state, d))
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
