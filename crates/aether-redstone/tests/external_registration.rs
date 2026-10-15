//! Proves the public registration API (QR-review-3 finding D5): this is an
//! **external crate** (an integration test), and it adds a redstone component
//! with no edit to the library's registry.

use aether_redstone::behaviour::register;
use aether_redstone::blocks::BlockBehaviour;
use aether_redstone::{props, BlockPos, Direction, MemWorld, Redstone, RedstoneView};
use aether_world::registry::BlockRegistry;
use aether_world::BlockStateId;

/// A stand-in whose wire connection is a fixed face, impossible with the
/// default rule (a signal source connecting on any side).
struct OnlyEast;

impl BlockBehaviour for OnlyEast {
    fn connects_to_wire(&self, _state: BlockStateId, dir: Option<Direction>) -> bool {
        dir == Some(Direction::East)
    }
}

static ONLY_EAST: OnlyEast = OnlyEast;

#[test]
fn an_external_crate_registers_a_block_behaviour() {
    register("minecraft:sticky_piston", &ONLY_EAST);

    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let wire = BlockPos::new(0, 0, 0);
    let observer = BlockPos::new(1, 0, 0);
    w.put(wire, "minecraft:redstone_wire");
    let observer_state = BlockRegistry::new()
        .get("minecraft:sticky_piston[facing=east,extended=false]")
        .expect("observer state");
    w.set_block(observer, observer_state);

    engine.on_placed(&mut w, wire);
    assert_eq!(
        props::get(w.state(wire), "east"),
        Some("side"),
        "the externally-registered rule must drive the wire connection"
    );
}
