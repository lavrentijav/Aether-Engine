//! Table-driven redstone circuits on [`MemWorld`].
//!
//! Each test builds a tiny circuit in code, drives it, and pins one piece of
//! vanilla behaviour. The comment on each test says which rule it fixes, so a
//! failure points at a rule rather than at a fixture.

use aether_redstone::props;
use aether_redstone::{BlockPos, Direction, MemWorld, Redstone};
use aether_world::BlockStateId;

fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn power(w: &MemWorld, pos: BlockPos) -> u8 {
    props::get(w.state(pos), "power")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn is_lit(w: &MemWorld, pos: BlockPos) -> bool {
    props::get(w.state(pos), "lit") == Some("true")
}

fn is_powered(w: &MemWorld, pos: BlockPos) -> bool {
    props::get(w.state(pos), "powered") == Some("true")
}

/// A lever west of a straight run of wire, then a lamp.
fn wire_line(engine: &Redstone, length: i32) -> (MemWorld, BlockPos, BlockPos) {
    let mut w = MemWorld::new();
    let lever = p(0, 64, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    for x in 1..=length {
        w.put(p(x, 64, 0), "minecraft:redstone_wire");
    }
    let lamp = p(length + 1, 64, 0);
    w.put(lamp, "minecraft:redstone_lamp");

    engine.on_placed(&mut w, lever);
    for x in 1..=length {
        engine.on_placed(&mut w, p(x, 64, 0));
    }
    engine.on_placed(&mut w, lamp);
    engine.on_use(&mut w, lever);
    (w, p(length, 64, 0), lamp)
}

#[test]
fn wire_fifteen_reaches_a_lamp_sixteen_does_not() {
    // Redstone wire transmits 15 blocks and decays by one per block: the 15th
    // wire holds power 1 and lights the lamp, the 16th holds 0 and does not.
    let engine = Redstone::new();

    let (w, last, lamp) = wire_line(&engine, 15);
    assert_eq!(power(&w, last), 1, "15th wire should hold power 1");
    assert!(is_lit(&w, lamp), "lamp within 15 blocks should be lit");

    let (w, last, lamp) = wire_line(&engine, 16);
    assert_eq!(power(&w, last), 0, "16th wire should be dead");
    assert!(!is_lit(&w, lamp), "lamp past 15 blocks must be off");
}

#[test]
fn wire_power_counts_down_from_the_source() {
    // The wire adjacent to a 15 source is itself 15, the next 14, and so on.
    let engine = Redstone::new();
    let (w, _, _) = wire_line(&engine, 15);
    assert_eq!(power(&w, p(1, 64, 0)), 15);
    assert_eq!(power(&w, p(2, 64, 0)), 14);
    assert_eq!(power(&w, p(15, 64, 0)), 1);
}

#[test]
fn a_torch_on_a_powered_block_inverts_it() {
    // A redstone torch attached to a block strongly powered by a lever turns
    // off: the classic NOT gate. Torch off depowers the wire it fed.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let block = p(0, 0, 0);
    let lever = p(0, 1, 0);
    let torch = p(1, 0, 0);
    let wire = p(2, 0, 0);
    let lamp = p(3, 0, 0);
    w.put(block, "minecraft:stone");
    w.put(lever, "minecraft:lever[face=floor,facing=north]");
    w.put(torch, "minecraft:redstone_wall_torch[facing=east]");
    w.put(wire, "minecraft:redstone_wire");
    w.put(lamp, "minecraft:redstone_lamp");
    for pos in [block, lever, torch, wire, lamp] {
        engine.on_placed(&mut w, pos);
    }

    // Unpowered input: the torch is lit and feeds the wire and lamp.
    assert!(is_lit(&w, torch), "torch starts lit");
    assert_eq!(power(&w, wire), 15);
    assert!(is_lit(&w, lamp));

    // Power the block; the torch notices two ticks later and turns off.
    engine.on_use(&mut w, lever);
    w.run(&engine, 2);
    assert!(!is_lit(&w, torch), "a powered block turns the torch off");
    assert_eq!(power(&w, wire), 0, "the wire loses its source");
    w.run(&engine, 4);
    assert!(!is_lit(&w, lamp), "the lamp follows, one tick 4 later");
}

#[test]
fn repeater_four_tick_delay_is_exact() {
    // A repeater with delay 4 waits 4 × 2 = 8 game ticks — not 7, not 9.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, 0);
    let repeater = p(1, 0, 0);
    let lamp = p(2, 0, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(repeater, "minecraft:repeater[facing=west,delay=4]");
    w.put(lamp, "minecraft:redstone_lamp");
    for pos in [lever, repeater, lamp] {
        engine.on_placed(&mut w, pos);
    }

    engine.on_use(&mut w, lever);
    w.run(&engine, 7);
    assert!(!is_powered(&w, repeater), "must still be off after 7 ticks");
    assert!(!is_lit(&w, lamp));
    w.run(&engine, 1);
    assert!(is_powered(&w, repeater), "on at exactly 8 ticks");
    assert!(is_lit(&w, lamp));
}

#[test]
fn a_locking_repeater_freezes_its_neighbour() {
    // A repeater or comparator facing into a repeater's side locks it: it
    // ignores its input until unlocked.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, 0);
    let repeater = p(1, 0, 0);
    let lamp = p(2, 0, 0);
    let lock = p(1, 0, -1);
    let lock_source = p(1, 0, -2);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(repeater, "minecraft:repeater[facing=west,delay=1]");
    w.put(lock, "minecraft:repeater[facing=north,delay=1]");
    w.put(lock_source, "minecraft:redstone_block");
    w.put(lamp, "minecraft:redstone_lamp");
    for pos in [lever, repeater, lock, lock_source, lamp] {
        engine.on_placed(&mut w, pos);
    }

    // Let the side repeater turn on and lock the target.
    w.run(&engine, 2);
    assert!(is_powered(&w, lock), "the lock repeater is on");
    assert!(props::get(w.state(repeater), "locked") == Some("true"));

    engine.on_use(&mut w, lever);
    w.run(&engine, 10);
    assert!(
        !is_powered(&w, repeater),
        "a locked repeater ignores the input"
    );
    assert!(!is_lit(&w, lamp));
}

#[test]
fn a_one_tick_pulse_is_extended_to_the_repeater_delay() {
    // A pulse shorter than the delay still produces a full-delay output pulse:
    // the repeater latches on at its scheduled tick even after the input drops.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, 0);
    let repeater = p(1, 0, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(repeater, "minecraft:repeater[facing=west,delay=4]");
    engine.on_placed(&mut w, lever);
    engine.on_placed(&mut w, repeater);

    engine.on_use(&mut w, lever); // pulse high for one tick
    w.run(&engine, 1);
    engine.on_use(&mut w, lever); // low again

    w.run(&engine, 7); // t = 8: the repeater latches on
    assert!(is_powered(&w, repeater), "on despite a 1-tick input");

    w.run(&engine, 7); // t = 15: still on, its own pulse not finished
    assert!(is_powered(&w, repeater));
    w.run(&engine, 1); // t = 16: now off
    assert!(!is_powered(&w, repeater));
}

#[test]
fn wire_climbs_a_block_and_over_it() {
    // Redstone wire reaches a wire on top of an adjacent block, one level up.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, -1);
    let source = p(0, 0, 0);
    let block = p(1, 0, 0);
    let over = p(1, 1, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=south]");
    w.put(source, "minecraft:redstone_wire");
    w.put(block, "minecraft:stone");
    w.put(over, "minecraft:redstone_wire");
    for pos in [lever, source, block, over] {
        engine.on_placed(&mut w, pos);
    }
    engine.on_use(&mut w, lever);

    assert_eq!(power(&w, source), 15, "the lever powers the ground wire");
    assert_eq!(power(&w, over), 14, "and the wire one block up, minus one");
}

#[test]
fn a_conductor_above_the_lower_wire_cuts_the_climb() {
    // The block "between" the two wires must be air or non-conductive; a
    // conductive block above the lower wire cuts the connection.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, -1);
    let source = p(0, 0, 0);
    let block = p(1, 0, 0);
    let over = p(1, 1, 0);
    let cut = p(0, 1, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=south]");
    w.put(source, "minecraft:redstone_wire");
    w.put(cut, "minecraft:stone");
    w.put(block, "minecraft:stone");
    w.put(over, "minecraft:redstone_wire");
    for pos in [lever, source, cut, block, over] {
        engine.on_placed(&mut w, pos);
    }
    engine.on_use(&mut w, lever);

    assert_eq!(power(&w, source), 15);
    assert_eq!(power(&w, over), 0, "the cut blocks the up-and-over");
}

#[test]
fn a_lever_on_a_block_powers_wire_through_it_but_wire_does_not() {
    // Strong vs weak: a lever strongly powers its block, and that block weakly
    // powers adjacent wire. A block powered only by wire is weak, so it cannot
    // power further wire.
    let engine = Redstone::new();

    // Lever on a block, wire beside the block: powered.
    let mut w = MemWorld::new();
    let block = p(0, 0, 0);
    let lever = p(0, 1, 0);
    let wire = p(1, 0, 0);
    w.put(block, "minecraft:stone");
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(wire, "minecraft:redstone_wire");
    for pos in [block, lever, wire] {
        engine.on_placed(&mut w, pos);
    }
    engine.on_use(&mut w, lever);
    assert_eq!(power(&w, wire), 15, "strong power passes through the block");

    // Wire pointing at a block does not power wire on the far side.
    let mut w = MemWorld::new();
    let lever = p(0, 0, -1);
    let wire1 = p(0, 0, 0);
    let block = p(1, 0, 0);
    let wire2 = p(2, 0, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=south]");
    w.put(wire1, "minecraft:redstone_wire");
    w.put(block, "minecraft:stone");
    w.put(wire2, "minecraft:redstone_wire");
    for pos in [lever, wire1, block, wire2] {
        engine.on_placed(&mut w, pos);
    }
    engine.on_use(&mut w, lever);
    assert_eq!(power(&w, wire1), 15);
    assert_eq!(
        power(&w, wire2),
        0,
        "wire power is weak — it stops at the block"
    );
}

#[test]
fn a_button_presses_and_releases_on_its_own() {
    // A stone button stays pressed 20 ticks, a wooden one 30.
    let engine = Redstone::new();
    for (name, ticks) in [
        ("minecraft:stone_button[face=floor,facing=east]", 20u32),
        ("minecraft:oak_button[face=floor,facing=east]", 30),
    ] {
        let mut w = MemWorld::new();
        let button = p(0, 0, 0);
        let wire = p(1, 0, 0);
        w.put(button, name);
        w.put(wire, "minecraft:redstone_wire");
        engine.on_placed(&mut w, button);
        engine.on_placed(&mut w, wire);

        engine.on_use(&mut w, button);
        assert_eq!(power(&w, wire), 15, "{name}: pressed powers the wire");
        w.run(&engine, ticks - 1);
        assert_eq!(power(&w, wire), 15, "{name}: still pressed");
        w.run(&engine, 1);
        assert_eq!(power(&w, wire), 0, "{name}: released");
    }
}

#[test]
fn a_torch_burns_out_after_repeated_toggling() {
    // More than 8 toggles inside 60 ticks burns a redstone torch out: it stays
    // off for 160 ticks and only recovers once the toggle history ages out.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let block = p(0, 0, 0);
    let lever = p(0, 1, 0);
    let torch = p(1, 0, 0);
    w.put(block, "minecraft:stone");
    w.put(lever, "minecraft:lever[face=floor,facing=north]");
    w.put(torch, "minecraft:redstone_wall_torch[facing=east]");
    for pos in [block, lever, torch] {
        engine.on_placed(&mut w, pos);
    }

    // Eight off-transitions, each given two ticks to settle.
    for _ in 0..8 {
        engine.on_use(&mut w, lever); // block powered -> torch off
        w.run(&engine, 2);
        engine.on_use(&mut w, lever); // block unpowered -> torch on again
        w.run(&engine, 2);
    }

    assert!(!is_lit(&w, torch), "the eighth toggle burns it out");
    // With a clean input it would relight at once; burnt out, it does not.
    w.run(&engine, 100);
    assert!(!is_lit(&w, torch), "still burnt out");
    // After the 160-tick cooldown it recovers.
    w.run(&engine, 65);
    assert!(is_lit(&w, torch), "recovers after the cooldown");
}

#[test]
fn the_same_circuit_produces_the_same_trace() {
    // Determinism: the engine is a pure function of the view, so two identical
    // circuits give byte-for-byte identical tick-by-tick state.
    fn build() -> (MemWorld, Vec<BlockPos>) {
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let positions = vec![
            p(0, 0, 0),
            p(1, 0, 0),
            p(2, 0, 0),
            p(3, 0, 0),
            p(4, 0, 0),
            p(3, 0, -1),
        ];
        w.put(p(0, 0, 0), "minecraft:lever[face=floor,facing=east]");
        w.put(p(1, 0, 0), "minecraft:redstone_wire");
        w.put(p(2, 0, 0), "minecraft:repeater[facing=west,delay=2]");
        w.put(p(3, 0, 0), "minecraft:redstone_wire");
        w.put(p(4, 0, 0), "minecraft:redstone_lamp");
        w.put(p(3, 0, -1), "minecraft:redstone_torch");
        for pos in &positions {
            engine.on_placed(&mut w, *pos);
        }
        engine.on_use(&mut w, p(0, 0, 0));
        (w, positions)
    }

    fn trace() -> Vec<Vec<BlockStateId>> {
        let (mut w, positions) = build();
        let engine = Redstone::new();
        let mut out = Vec::new();
        for _ in 0..24 {
            w.step(&engine);
            out.push(positions.iter().map(|&pos| w.state(pos)).collect());
        }
        out
    }

    assert_eq!(trace(), trace(), "the run must be reproducible");
}

#[test]
fn every_direction_is_reachable_and_symmetric() {
    // A wire is powered from any side; the engine must read all six directions.
    let engine = Redstone::new();
    for dir in Direction::ALL {
        let mut w = MemWorld::new();
        let lever = p(0, 0, 0);
        let wire = lever.offset(dir);
        w.put(lever, "minecraft:lever[face=floor,facing=north]");
        w.put(wire, "minecraft:redstone_wire");
        engine.on_placed(&mut w, lever);
        engine.on_placed(&mut w, wire);
        engine.on_use(&mut w, lever);
        assert_eq!(power(&w, wire), 15, "wire {dir:?} of the lever");
    }
}
