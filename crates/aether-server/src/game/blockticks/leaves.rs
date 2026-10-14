//! Leaf decay (C3): a leaf far from any log turns `distance` up to seven and
//! then, on a random tick, drops and disappears.
//!
//! Transcribed from `LeavesBlock` in the 1.21.11 server. The `distance`
//! property already exists in the block table; what this adds is the two
//! halves vanilla runs:
//!
//! * a **scheduled** update, one tick after any neighbour changes, that
//!   recomputes `distance` as the minimum over the six touching cells of
//!   `distanceAt(neighbour) + 1` (a log being zero, any other block seven);
//! * a **random tick** that removes a leaf whose `distance` is seven and which
//!   is not `persistent`. Player-placed leaves are persistent and so never
//!   decay, exactly as in vanilla.

use aether_api::block_ids;
use aether_world::BlockStateId;

use super::{name_of, short, Cells, Pos, AROUND};
use crate::game::tables::{self, Rng};
use crate::inventory::Stack;
use crate::players::SharedRegistry;
use crate::session::DemoWorld;

/// The `LOGS` tag, spelled out — the engine has no block-tag table. A leaf
/// beside any of these is distance one.
fn is_log(name: &str) -> bool {
    let n = short(name);
    n.ends_with("_log")
        || n.ends_with("_wood")
        || n.ends_with("_stem")
        || n.ends_with("_hyphae")
        || n == "bamboo_block"
        || n == "stripped_bamboo_block"
}

/// `LeavesBlock.getOptionalDistanceAt`, defaulted to seven.
fn distance_at(state: BlockStateId) -> i32 {
    if let Some(name) = name_of(state) {
        if is_log(name) {
            return 0;
        }
    }
    super::prop(state, "distance")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(7)
}

/// `LeavesBlock.updateDistance`: the distance a leaf at `pos` should hold.
pub(super) fn next_distance(get: &Cells<'_>, pos: Pos) -> i32 {
    let (x, y, z) = pos;
    let mut d = 7;
    for (dx, dy, dz) in AROUND {
        d = d.min(distance_at(get(x + dx, y + dy, z + dz)) + 1);
        if d == 1 {
            break;
        }
    }
    d
}

/// A leaf is randomly ticking, and will decay, exactly when it is not
/// persistent and sits at distance seven.
pub(super) fn is_decaying(state: BlockStateId) -> bool {
    super::prop(state, "persistent") != Some("true") && super::prop(state, "distance") == Some("7")
}

/// The scheduled half: recompute `distance` and write it if it changed.
pub(super) fn update(world: &DemoWorld, registry: &SharedRegistry, pos: Pos, state: BlockStateId) {
    let get = |x, y, z| world.get_block(x, y, z);
    let want = next_distance(&get, pos);
    if super::prop(state, "distance").and_then(|v| v.parse::<i32>().ok()) == Some(want) {
        return;
    }
    if let Some(next) = super::with_prop(state, "distance", &want.to_string()) {
        super::set(world, registry, pos, next);
    }
}

/// The random half: a decaying leaf drops its loot and is removed.
pub(super) fn decay(
    world: &DemoWorld,
    registry: &SharedRegistry,
    rng: &mut Rng,
    pos: Pos,
    state: BlockStateId,
) {
    if !is_decaying(state) {
        return;
    }
    let Some(name) = name_of(state) else {
        return;
    };
    let stacks: Vec<Stack> = tables::block_drops(name, None, rng)
        .into_iter()
        .map(|(item, n)| Stack::named(&item, n))
        .collect();
    if !stacks.is_empty() {
        crate::game::entities::drop_at_block(
            pos.0,
            pos.1,
            pos.2,
            stacks,
            aether_world::journal::ActorId::SERVER,
            None,
        );
    }
    super::set(world, registry, pos, block_ids::AIR);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::registry::blocks;
    use std::collections::HashMap;

    struct Map(HashMap<Pos, BlockStateId>);

    impl Map {
        fn new() -> Self {
            Self(HashMap::new())
        }
        fn put(mut self, pos: Pos, name: &str) -> Self {
            self.0.insert(pos, blocks::default_state(name).unwrap());
            self
        }
        fn set(mut self, pos: Pos, state: BlockStateId) -> Self {
            self.0.insert(pos, state);
            self
        }
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| self.0.get(&(x, y, z)).copied().unwrap_or(block_ids::AIR)
        }
    }

    fn leaf(distance: i32) -> BlockStateId {
        super::super::with_prop(
            blocks::default_state("minecraft:oak_leaves").unwrap(),
            "distance",
            &distance.to_string(),
        )
        .unwrap()
    }

    #[test]
    fn a_leaf_beside_a_log_has_distance_one() {
        let m = Map::new().put((1, 0, 0), "minecraft:oak_log");
        assert_eq!(next_distance(&m.get(), (0, 0, 0)), 1);
    }

    #[test]
    fn distance_propagates_outwards_and_caps_at_seven() {
        let m = Map::new()
            .put((0, 0, 0), "minecraft:oak_log")
            .set((1, 0, 0), leaf(1))
            .set((2, 0, 0), leaf(7));
        let get = m.get();
        // Beside the log, and one further out along the chain of distances.
        assert_eq!(next_distance(&get, (1, 0, 0)), 1);
        assert_eq!(next_distance(&get, (2, 0, 0)), 2);
        // With nothing to reach, a lone leaf stays at the cap.
        assert_eq!(next_distance(&get, (5, 0, 0)), 7);
    }

    #[test]
    fn only_distance_seven_non_persistent_leaves_decay() {
        assert!(is_decaying(leaf(7)));
        assert!(!is_decaying(leaf(6)));
        let persistent = super::super::with_prop(leaf(7), "persistent", "true").unwrap();
        assert!(!is_decaying(persistent));
    }

    #[test]
    fn the_log_family_is_recognised() {
        for name in [
            "minecraft:oak_log",
            "minecraft:stripped_spruce_wood",
            "minecraft:crimson_stem",
            "minecraft:warped_hyphae",
            "minecraft:bamboo_block",
        ] {
            assert!(is_log(name), "{name} is a log");
        }
        assert!(!is_log("minecraft:oak_leaves"));
        assert!(!is_log("minecraft:oak_planks"));
    }
}
