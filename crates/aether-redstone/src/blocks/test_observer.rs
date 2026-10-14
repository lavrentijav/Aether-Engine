//! A **test-only** dummy block, registered over the observer's states to prove
//! that a new component can decide its own redstone-wire connection without any
//! edit to [`crate::blocks::wire`] (QR-review-2 finding N2).
//!
//! It mirrors `ObserverBlock`: `isSignalSource` is `POWERED`, and wire connects
//! only on the observer's facing side — *regardless* of `POWERED`. That last
//! part is exactly what the default `connects_to_wire` cannot express, so the
//! test below could not pass without the trait hook.

use aether_world::BlockStateId;

use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::props;

/// A stand-in for the observer, used only in tests.
pub struct TestObserver;

impl BlockBehaviour for TestObserver {
    fn is_signal_source(&self, state: BlockStateId) -> bool {
        // `ObserverBlock.isSignalSource` = POWERED.
        props::powered(state)
    }

    fn connects_to_wire(&self, state: BlockStateId, dir: Option<Direction>) -> bool {
        // `RedStoneWireBlock.shouldConnectTo`: observer connects when the query
        // direction is its facing, powered or not.
        dir == Some(props::facing(state))
    }
}

#[cfg(test)]
mod tests {
    use aether_world::BlockRegistry;

    use super::*;
    use crate::view::RedstoneView;
    use crate::{BlockPos, MemWorld, Redstone};

    fn p(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn a_custom_block_decides_its_own_wire_connection() {
        // Wire west of an unpowered observer facing east. Vanilla's wire
        // connects to the observer's facing side (`wire.east = side`) even
        // though the observer is unpowered; the default `connects_to_wire`
        // (a signal source, and the observer is not one while unpowered) would
        // leave it `none`. Only the trait override produces vanilla's shape.
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let wire = p(0, 0, 0);
        let observer = p(1, 0, 0);
        let observer_state = BlockRegistry::new()
            .get("minecraft:observer[facing=east,powered=false]")
            .expect("observer state");
        w.put(wire, "minecraft:redstone_wire");
        w.set_block(observer, observer_state);
        assert_eq!(
            w.get_block(observer),
            observer_state,
            "the dummy is registered over the observer states"
        );

        engine.on_placed(&mut w, wire);
        assert_eq!(
            props::get(w.state(wire), "east"),
            Some("side"),
            "the wire must connect to the observer's facing side"
        );
    }
}
