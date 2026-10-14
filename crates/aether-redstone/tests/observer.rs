//! Observer circuits (`ObserverBlock`).

use aether_redstone::{
    props, BlockPos, Direction, MemWorld, Redstone, RedstoneView, SetBlockFlags,
};
use aether_world::registry::BlockRegistry;
use aether_world::BlockStateId;

fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn state(name: &str) -> BlockStateId {
    BlockRegistry::new().get(name).unwrap()
}

/// The observer's output toward its watcher is read via `getSignal` in the
/// facing direction.
fn output(w: &MemWorld, observer: BlockPos, facing: Direction) -> u8 {
    Redstone::new().get_signal(w, observer, facing)
}

#[test]
fn an_observer_pulses_when_the_watched_block_changes() {
    // An observer facing east watches (1,0,0). A change there starts a pulse:
    // after two ticks it is powered, after two more it is not.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let observer = p(0, 0, 0);
    w.put(observer, "minecraft:observer[facing=east]");
    engine.on_placed(&mut w, observer);

    engine.set_block(
        &mut w,
        p(1, 0, 0),
        state("minecraft:stone"),
        SetBlockFlags::PLACE,
    );

    w.run(&engine, 1);
    assert_eq!(props::get(w.state(observer), "powered"), Some("false"));
    assert_eq!(output(&w, observer, Direction::East), 0);

    w.run(&engine, 1);
    assert_eq!(
        props::get(w.state(observer), "powered"),
        Some("true"),
        "powered at t+2"
    );
    assert_eq!(output(&w, observer, Direction::East), 15);

    w.run(&engine, 2);
    assert_eq!(
        props::get(w.state(observer), "powered"),
        Some("false"),
        "off at t+4"
    );
    assert_eq!(output(&w, observer, Direction::East), 0);
}

#[test]
fn an_observer_powers_a_lamp_behind_it() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let observer = p(0, 0, 0);
    let lamp = p(-1, 0, 0); // behind the observer (facing east)
    w.put(observer, "minecraft:observer[facing=east]");
    w.put(lamp, "minecraft:redstone_lamp");
    engine.on_placed(&mut w, observer);
    engine.on_placed(&mut w, lamp);

    engine.set_block(
        &mut w,
        p(1, 0, 0),
        state("minecraft:stone"),
        SetBlockFlags::PLACE,
    );
    w.run(&engine, 2);
    assert!(
        props::get(w.state(lamp), "lit") == Some("true"),
        "the pulse lights the lamp"
    );
    // The pulse ends at t+4; the lamp then waits its own 4 ticks to go out.
    w.run(&engine, 6);
    assert!(
        props::get(w.state(lamp), "lit") == Some("false"),
        "the lamp goes out after the pulse"
    );
}

#[test]
fn wire_connects_to_an_observer_regardless_of_powered() {
    // `RedStoneWireBlock.shouldConnectTo` special-cases the observer: the wire
    // connects on the observer's facing side whether it is powered or not.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let wire = p(0, 0, 0);
    let observer = p(1, 0, 0);
    w.put(wire, "minecraft:redstone_wire");
    w.put(observer, "minecraft:observer[facing=west,powered=false]");
    engine.on_placed(&mut w, wire);
    engine.on_placed(&mut w, observer);
    assert_eq!(
        props::get(w.state(wire), "east"),
        Some("side"),
        "wire connects to the unpowered observer's facing side"
    );
}

#[test]
fn an_observer_ignores_a_change_behind_it() {
    // Only the watched (facing) side triggers the pulse.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let observer = p(0, 0, 0);
    w.put(observer, "minecraft:observer[facing=east]");
    engine.on_placed(&mut w, observer);

    // Change the block behind (west): must not pulse.
    engine.set_block(
        &mut w,
        p(-1, 0, 0),
        state("minecraft:stone"),
        SetBlockFlags::PLACE,
    );
    w.run(&engine, 4);
    assert_eq!(props::get(w.state(observer), "powered"), Some("false"));
}
