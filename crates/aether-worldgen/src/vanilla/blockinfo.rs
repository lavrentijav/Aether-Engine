//! Per-state facts the generator asks about blocks: is it air, does it hold a
//! fluid, does it block motion, is it leaves.
//!
//! The engine's registry is vanilla's own state-id space, so these are
//! computed once over every state from the block name and properties and then
//! answered with one table load.

use std::sync::OnceLock;

use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

const AIR: u8 = 1;
const WATER: u8 = 2;
const LAVA: u8 = 4;
const MOTION: u8 = 8;
const LEAVES: u8 = 16;
const SOURCE: u8 = 32;

fn table() -> &'static [u8] {
    static T: OnceLock<Vec<u8>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0u8; blocks::STATE_COUNT];
        for (id, slot) in t.iter_mut().enumerate() {
            let state = BlockStateId(id as u32);
            let Some((_, name)) = blocks::block_of_state(state) else {
                continue;
            };
            let short = name.strip_prefix("minecraft:").unwrap_or(name);
            let full = props::state_name(id as u32).unwrap_or_default();
            let mut f = 0u8;
            match short {
                "air" | "cave_air" | "void_air" => f |= AIR,
                "water" | "bubble_column" | "kelp" | "kelp_plant" | "seagrass"
                | "tall_seagrass" => f |= WATER,
                "lava" => f |= LAVA,
                _ => {}
            }
            if full.contains("waterlogged=true") {
                f |= WATER;
            }
            if (short == "water" || short == "lava") && full.contains("level=0]") {
                f |= SOURCE;
            }
            if short.ends_with("_leaves") {
                f |= LEAVES;
            }
            let p = blocks::props_of_state(state).unwrap_or_default();
            // `blocksMotion()`: a collision shape, except the two blocks the
            // game singles out.
            if (p.collision
                && short != "cobweb"
                && short != "bamboo_sapling"
                && f & (WATER | LAVA) == 0
                || (p.collision && full.contains("waterlogged=true")))
                && legacy_solid_shape(short, &full)
            {
                f |= MOTION;
            }
            *slot = f;
        }
        t
    })
}

#[inline]
fn flags(s: BlockStateId) -> u8 {
    table().get(s.0 as usize).copied().unwrap_or(0)
}

/// `BlockState.isAir()`.
#[inline]
pub fn is_air(s: BlockStateId) -> bool {
    flags(s) & AIR != 0
}

/// Whether the state carries any fluid.
#[inline]
pub fn has_fluid(s: BlockStateId) -> bool {
    flags(s) & (WATER | LAVA) != 0
}

/// Whether the state carries water.
#[inline]
pub fn has_water(s: BlockStateId) -> bool {
    flags(s) & WATER != 0
}

/// Whether the state is lava.
#[inline]
pub fn is_lava(s: BlockStateId) -> bool {
    flags(s) & LAVA != 0
}

/// A full-strength water or lava block (`level=0`).
#[inline]
pub fn is_fluid_source(s: BlockStateId) -> bool {
    flags(s) & SOURCE != 0
}

/// `BlockState.blocksMotion()`.
#[inline]
pub fn blocks_motion(s: BlockStateId) -> bool {
    flags(s) & MOTION != 0
}

/// `instanceof LeavesBlock`.
#[inline]
pub fn is_leaves(s: BlockStateId) -> bool {
    flags(s) & LEAVES != 0
}

/// The block name of a state, `minecraft:` prefixed.
pub fn name(s: BlockStateId) -> &'static str {
    blocks::block_of_state(s)
        .map(|(_, n)| n)
        .unwrap_or("minecraft:air")
}

/// The block id (not state) of a state.
#[inline]
pub fn block_of(s: BlockStateId) -> u16 {
    blocks::block_of_state(s).map(|(b, _)| b).unwrap_or(0)
}

/// Parse `minecraft:name[k=v,...]` into a state.
pub fn parse_state(s: &str) -> Option<BlockStateId> {
    let (name, props_str) = match s.split_once('[') {
        Some((n, rest)) => (n, rest.strip_suffix(']')?),
        None => (s, ""),
    };
    let name = if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    };
    let block = blocks::block_id_of(&name)?;
    let pairs: Vec<(&str, &str)> = props_str
        .split(',')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.split_once('='))
        .collect();
    props::state_with(block, &pairs).map(BlockStateId)
}

/// The state with one property changed, or the state itself when it does not
/// have that property.
pub fn with_prop(s: BlockStateId, key: &str, value: &str) -> BlockStateId {
    let Some((block, _)) = blocks::block_of_state(s) else {
        return s;
    };
    let lo = blocks::states_of(name(s)).map(|r| *r.start()).unwrap_or(0);
    let mut vals = props::values_of(block, s.0 - lo);
    let Some(slot) = vals.iter_mut().find(|(k, _)| *k == key) else {
        return s;
    };
    slot.1 = value;
    let owned: Vec<(&str, &str)> = vals.iter().map(|(k, v)| (*k, *v)).collect();
    props::state_with(block, &owned)
        .map(BlockStateId)
        .unwrap_or(s)
}

/// One property's value.
pub fn prop(s: BlockStateId, key: &str) -> Option<&'static str> {
    let (block, _) = blocks::block_of_state(s)?;
    let lo = *blocks::states_of(name(s))?.start();
    props::values_of(block, s.0 - lo)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_classification() {
        let stone = parse_state("minecraft:stone").unwrap();
        let water = parse_state("minecraft:water[level=0]").unwrap();
        let grass = parse_state("minecraft:short_grass").unwrap();
        let air = parse_state("minecraft:air").unwrap();
        assert!(blocks_motion(stone));
        assert!(!blocks_motion(water) && has_water(water) && is_fluid_source(water));
        assert!(!blocks_motion(grass));
        assert!(is_air(air) && !is_air(stone));
        let leaves =
            parse_state("minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]")
                .unwrap();
        assert!(is_leaves(leaves) && blocks_motion(leaves));
        let log = parse_state("minecraft:oak_log[axis=x]").unwrap();
        assert_eq!(prop(log, "axis"), Some("x"));
        assert_eq!(prop(with_prop(log, "axis", "z"), "axis"), Some("z"));
    }
}

/// Whether a block with a collision shape is also "solid" in the legacy
/// sense `blocksMotion` uses: the game counts a shape solid when its bounding
/// box averages at least 0.729 of a block per axis, or is a full block tall.
/// The thin shapes that fail that test, by name — lily pads above all, whose
/// pads would otherwise lift every swamp tree's heightmap by one.
fn legacy_solid_shape(short: &str, full: &str) -> bool {
    if short == "snow" {
        return !(full.contains("layers=1]")
            || full.contains("layers=1,")
            || full.contains("layers=2"));
    }
    !(short == "lily_pad"
        || short.ends_with("_carpet")
        || short == "cocoa"
        || short == "sea_pickle"
        || short == "turtle_egg"
        || short.starts_with("potted_")
        || short == "flower_pot"
        || short.ends_with("candle")
        || short.ends_with("_candle_cake")
        || short == "cake"
        || short == "lantern"
        || short == "soul_lantern"
        || short.ends_with("_button")
        || short.ends_with("_head")
        || short.ends_with("_skull")
        || short.ends_with("amethyst_bud")
        || short == "small_dripleaf"
        || short == "repeater"
        || short == "comparator"
        || short == "daylight_detector"
        || short == "end_rod"
        || short == "conduit")
}
