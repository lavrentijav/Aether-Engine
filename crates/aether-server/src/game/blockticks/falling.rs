//! Falling blocks (C1): sand, red sand, gravel and concrete powder.
//!
//! Vanilla's `FallingBlock` schedules a tick two ticks after the block, or one
//! of its six neighbours, changes; on that tick, if the cell below is free, it
//! replaces itself with a `FallingBlockEntity` that falls a block at a time.
//! This engine skips the entity: on its tick the block is moved to the cell it
//! would have come to rest in, in one step. The blocks end up where vanilla
//! leaves them, without the animation — see `KNOWN_ISSUES` A.11.

use aether_api::block_ids;
use aether_world::{registry::blocks, BlockStateId};

use super::{
    is_concrete_powder, is_free, is_water, name_of, short, Cells, Pos, AROUND, WORLD_BOTTOM,
};
use crate::players::SharedRegistry;
use crate::session::DemoWorld;

/// One scheduled falling-block tick.
pub(super) fn fall(world: &DemoWorld, registry: &SharedRegistry, pos: Pos, state: BlockStateId) {
    let get = |x, y, z| world.get_block(x, y, z);
    let name = name_of(state).unwrap_or("minecraft:sand");
    let powder = is_concrete_powder(name);
    match landing(&get, pos) {
        // Already supported: a concrete powder beside water sets where it is.
        None => {
            if powder && touches_water(&get, pos) {
                if let Some(concrete) = concrete_of(state) {
                    super::set(world, registry, pos, concrete);
                }
            }
        }
        Some(land) => {
            super::set(world, registry, pos, block_ids::AIR);
            // A concrete powder that touches water on its way down sets on
            // landing rather than coming to rest as powder.
            let target = if powder && touches_water(&get, land) {
                concrete_of(state).unwrap_or(state)
            } else {
                state
            };
            super::set(world, registry, land, target);
        }
    }
}

/// Where an unsupported falling block at `pos` settles, or `None` if it is
/// already supported.
pub(super) fn landing(get: &Cells<'_>, pos: Pos) -> Option<Pos> {
    let (x, y, z) = pos;
    if !is_free(get(x, y - 1, z)) {
        return None;
    }
    let mut ly = y;
    while ly > WORLD_BOTTOM && is_free(get(x, ly - 1, z)) {
        ly -= 1;
    }
    Some((x, ly, z))
}

/// Whether any of the six cells around `pos` holds water.
///
/// Vanilla's `ConcretePowderBlock` skips the cell below unless the powder's own
/// cell is water; including it costs nothing here and matches a powder landing
/// on water, which the falling entity sets on contact.
pub(super) fn touches_water(get: &Cells<'_>, pos: Pos) -> bool {
    AROUND
        .iter()
        .any(|(dx, dy, dz)| is_water(get(pos.0 + dx, pos.1 + dy, pos.2 + dz)))
}

/// The concrete a `*_concrete_powder` state sets into.
pub(super) fn concrete_of(state: BlockStateId) -> Option<BlockStateId> {
    let color = short(name_of(state)?).strip_suffix("_concrete_powder")?;
    blocks::default_state(&format!("minecraft:{color}_concrete"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| self.0.get(&(x, y, z)).copied().unwrap_or(block_ids::AIR)
        }
    }

    #[test]
    fn concrete_powder_sets_when_a_neighbour_is_water() {
        let m = Map::new().put((1, 0, 0), "minecraft:water");
        let get = m.get();
        assert!(touches_water(&get, (0, 0, 0)));
        assert!(!touches_water(&get, (0, 0, 5)));

        let powder = blocks::default_state("minecraft:white_concrete_powder").unwrap();
        let concrete = concrete_of(powder).unwrap();
        assert_eq!(name_of(concrete), Some("minecraft:white_concrete"));
    }
}
