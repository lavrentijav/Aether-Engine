//! Tripwire circuits (`TripWireBlock` / `TripWireHookBlock`).

use aether_redstone::{props, BlockPos, Direction, MemWorld, Redstone, SetBlockFlags};

fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn signal(w: &MemWorld, pos: BlockPos, dir: Direction) -> u8 {
    Redstone::new().get_signal(w, pos, dir)
}

/// A hook at (0,0,0) facing east, wire at (1,0,0), hook at (2,0,0) facing west.
fn line() -> (Redstone, MemWorld, BlockPos, BlockPos, BlockPos) {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let a = p(0, 0, 0);
    let wire = p(1, 0, 0);
    let b = p(2, 0, 0);
    // Support for each hook (behind it).
    w.put(p(-1, 0, 0), "minecraft:stone");
    w.put(p(3, 0, 0), "minecraft:stone");
    w.put(a, "minecraft:tripwire_hook[facing=east]");
    w.put(wire, "minecraft:tripwire");
    w.put(b, "minecraft:tripwire_hook[facing=west]");
    for pos in [p(-1, 0, 0), p(3, 0, 0), a, wire, b] {
        engine.on_placed(&mut w, pos);
    }
    (engine, w, a, wire, b)
}

#[test]
fn a_tripwire_line_attaches_but_stays_off() {
    let (_engine, w, a, wire, b) = line();
    assert_eq!(props::get(w.state(wire), "attached"), Some("true"));
    assert_eq!(props::get(w.state(a), "attached"), Some("true"));
    assert_eq!(props::get(w.state(b), "attached"), Some("true"));
    assert_eq!(props::get(w.state(a), "powered"), Some("false"));
    assert_eq!(signal(&w, a, Direction::Up), 0);
}

#[test]
fn an_entity_on_the_wire_powers_both_hooks() {
    let (engine, mut w, a, wire, b) = line();
    w.set_entities(wire, 1);
    engine.on_entity_inside(&mut w, wire);

    assert_eq!(props::get(w.state(wire), "powered"), Some("true"));
    assert_eq!(props::get(w.state(a), "powered"), Some("true"));
    assert_eq!(props::get(w.state(b), "powered"), Some("true"));
    assert_eq!(signal(&w, a, Direction::Up), 15);
    assert_eq!(signal(&w, b, Direction::Up), 15);
}

#[test]
fn the_hook_only_directly_powers_its_facing() {
    let (engine, mut w, a, wire, _b) = line();
    w.set_entities(wire, 1);
    engine.on_entity_inside(&mut w, wire);
    // a faces east: strong signal only toward east.
    assert_eq!(
        Redstone::new().get_direct_signal(&w, a, Direction::East),
        15
    );
    assert_eq!(Redstone::new().get_direct_signal(&w, a, Direction::West), 0);
}

#[test]
fn the_line_turns_off_after_the_entity_leaves() {
    let (engine, mut w, a, wire, b) = line();
    w.set_entities(wire, 1);
    engine.on_entity_inside(&mut w, wire);
    assert_eq!(props::get(w.state(a), "powered"), Some("true"));

    w.set_entities(wire, 0);
    w.run(&engine, 10);
    assert_eq!(props::get(w.state(wire), "powered"), Some("false"));
    assert_eq!(props::get(w.state(a), "powered"), Some("false"));
    assert_eq!(props::get(w.state(b), "powered"), Some("false"));
}

#[test]
fn a_tripwire_hook_pops_when_its_support_is_removed() {
    let (engine, mut w, a, _wire, _b) = line();
    engine.set_block(
        &mut w,
        p(-1, 0, 0),
        aether_world::BlockStateId::AIR,
        SetBlockFlags::PLACE,
    );
    assert_eq!(w.block_name(a), Some("minecraft:air"), "the hook pops off");
}

#[test]
fn tripwire_wire_connects_to_a_facing_hook() {
    let (_engine, w, _a, wire, _b) = line();
    assert_eq!(props::get(w.state(wire), "west"), Some("true"));
    assert_eq!(props::get(w.state(wire), "east"), Some("true"));
    assert_eq!(props::get(w.state(wire), "north"), Some("false"));
}

#[test]
fn two_adjacent_hooks_do_not_attach() {
    // F3: vanilla's `bl5 &= j > 1` — a line needs at least one wire between the
    // hooks. Two back-to-back hooks (no wire) must not latch attached.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let a = p(0, 0, 0);
    let b = p(1, 0, 0);
    w.put(p(-1, 0, 0), "minecraft:stone");
    w.put(p(2, 0, 0), "minecraft:stone");
    w.put(a, "minecraft:tripwire_hook[facing=east]");
    w.put(b, "minecraft:tripwire_hook[facing=west]");
    for pos in [p(-1, 0, 0), p(2, 0, 0), a, b] {
        engine.on_placed(&mut w, pos);
    }

    assert_eq!(props::get(w.state(a), "attached"), Some("false"));
    assert_eq!(props::get(w.state(b), "attached"), Some("false"));
    assert_eq!(props::get(w.state(a), "powered"), Some("false"));
}

#[test]
fn removing_a_powered_wire_releases_both_hooks() {
    // F1: `TripWireBlock.affectNeighborsAfterRemoval` recomputes the line with
    // the removed wire substituted at its distance `i`, so the scan reaches the
    // far hook and both ends are re-derived. Vanilla keeps both powered for the
    // 10-tick recheck, then releases both. The engine used to leave the far hook
    // stuck `powered`/`attached` forever.
    let (engine, mut w, a, wire, b) = line();
    w.set_entities(wire, 1);
    engine.on_entity_inside(&mut w, wire);
    assert_eq!(props::get(w.state(a), "powered"), Some("true"));
    assert_eq!(props::get(w.state(b), "powered"), Some("true"));

    engine.set_block(
        &mut w,
        wire,
        aether_world::BlockStateId::AIR,
        SetBlockFlags::PLACE,
    );

    // The 10-tick recheck runs; both hooks must release.
    w.run(&engine, 10);
    assert_eq!(
        props::get(w.state(a), "powered"),
        Some("false"),
        "near hook must release"
    );
    assert_eq!(
        props::get(w.state(b), "powered"),
        Some("false"),
        "far hook must release"
    );
    assert_eq!(
        props::get(w.state(b), "attached"),
        Some("false"),
        "far hook must detach"
    );
    assert_eq!(
        signal(&w, b, Direction::Up),
        0,
        "far hook must stop emitting"
    );

    // And it stays off.
    w.run(&engine, 100);
    assert_eq!(props::get(w.state(b), "powered"), Some("false"));
}

#[test]
fn removing_an_unpowered_wire_detaches_the_far_hook() {
    // With no entity, removing the wire must detach both hooks after the 10-tick
    // recheck (the old bug left the far one attached permanently).
    let (engine, mut w, a, wire, b) = line();
    assert_eq!(props::get(w.state(b), "attached"), Some("true"));

    engine.set_block(
        &mut w,
        wire,
        aether_world::BlockStateId::AIR,
        SetBlockFlags::PLACE,
    );

    w.run(&engine, 10);
    assert_eq!(props::get(w.state(a), "attached"), Some("false"));
    assert_eq!(
        props::get(w.state(b), "attached"),
        Some("false"),
        "far hook must detach"
    );
}
