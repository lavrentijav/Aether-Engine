//! Table-driven redstone circuits on [`MemWorld`].
//!
//! Each test builds a tiny circuit in code, drives it, and pins one piece of
//! vanilla behaviour. The comment on each test says which rule it fixes, so a
//! failure points at a rule rather than at a fixture.

use aether_redstone::props;
use aether_redstone::{BlockPos, Direction, MemWorld, Redstone, SetBlockFlags};
use aether_world::registry::BlockRegistry;
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
    // The engine must read the lever's signal in all six directions. A floor
    // lever and the wires beside it each need their own support (R13), so the
    // six-way check reads the signal directly and the wire check is horizontal.
    let engine = Redstone::new();
    for dir in Direction::ALL {
        let mut w = MemWorld::new();
        let lever = p(0, 0, 0);
        let under = lever.offset(Direction::Down);
        w.put(under, "minecraft:stone");
        w.put(lever, "minecraft:lever[face=floor,facing=north]");
        engine.on_placed(&mut w, under);
        engine.on_placed(&mut w, lever);
        engine.on_use(&mut w, lever);
        assert_eq!(engine.get_signal(&w, lever, dir), 15, "lever {dir:?}");
    }
    for dir in Direction::HORIZONTAL {
        let mut w = MemWorld::new();
        let lever = p(0, 0, 0);
        let wire = lever.offset(dir);
        w.put(lever.offset(Direction::Down), "minecraft:stone");
        w.put(wire.offset(Direction::Down), "minecraft:stone");
        w.put(lever, "minecraft:lever[face=floor,facing=north]");
        w.put(wire, "minecraft:redstone_wire");
        engine.on_placed(&mut w, lever.offset(Direction::Down));
        engine.on_placed(&mut w, wire.offset(Direction::Down));
        engine.on_placed(&mut w, lever);
        engine.on_placed(&mut w, wire);
        engine.on_use(&mut w, lever);
        assert_eq!(power(&w, wire), 15, "wire {dir:?} of the lever");
    }
}

#[test]
fn a_wire_pops_off_when_its_support_is_removed() {
    // N8/R13: `RedStoneWireBlock.updateShape(DOWN)` returns air when the block
    // below is gone.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let support = p(0, 0, 0);
    let wire = p(0, 1, 0);
    w.put(support, "minecraft:stone");
    w.put(wire, "minecraft:redstone_wire");
    engine.on_placed(&mut w, support);
    engine.on_placed(&mut w, wire);
    assert_eq!(w.block_name(wire), Some("minecraft:redstone_wire"));

    engine.set_block(&mut w, support, BlockStateId::AIR, SetBlockFlags::PLACE);
    assert_eq!(
        w.block_name(wire),
        Some("minecraft:air"),
        "the wire must pop off with its support"
    );
}

#[test]
fn a_repeater_pops_off_when_its_support_is_removed() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let support = p(0, 0, 0);
    let repeater = p(0, 1, 0);
    w.put(support, "minecraft:stone");
    w.put(repeater, "minecraft:repeater[facing=west,delay=1]");
    engine.on_placed(&mut w, support);
    engine.on_placed(&mut w, repeater);
    assert_eq!(w.block_name(repeater), Some("minecraft:repeater"));

    engine.set_block(&mut w, support, BlockStateId::AIR, SetBlockFlags::PLACE);
    assert_eq!(
        w.block_name(repeater),
        Some("minecraft:air"),
        "the repeater must pop off"
    );
}

#[test]
fn a_torch_pops_off_when_its_support_is_removed() {
    let engine = Redstone::new();
    // Standing torch on a block.
    let mut w = MemWorld::new();
    let support = p(0, 0, 0);
    let torch = p(0, 1, 0);
    w.put(support, "minecraft:stone");
    w.put(torch, "minecraft:redstone_torch");
    engine.on_placed(&mut w, support);
    engine.on_placed(&mut w, torch);
    engine.set_block(&mut w, support, BlockStateId::AIR, SetBlockFlags::PLACE);
    assert_eq!(w.block_name(torch), Some("minecraft:air"), "standing torch");

    // Wall torch on the side of a block.
    let mut w = MemWorld::new();
    let support = p(0, 0, 0);
    let torch = p(1, 0, 0);
    w.put(support, "minecraft:stone");
    w.put(torch, "minecraft:redstone_wall_torch[facing=east]");
    engine.on_placed(&mut w, support);
    engine.on_placed(&mut w, torch);
    engine.set_block(&mut w, support, BlockStateId::AIR, SetBlockFlags::PLACE);
    assert_eq!(w.block_name(torch), Some("minecraft:air"), "wall torch");
}

#[test]
fn a_lever_and_button_pop_off_when_their_support_is_removed() {
    let engine = Redstone::new();
    for block in [
        "minecraft:lever[face=floor,facing=east]",
        "minecraft:stone_button[face=floor,facing=east]",
    ] {
        let mut w = MemWorld::new();
        let support = p(0, 0, 0);
        let device = p(0, 1, 0);
        w.put(support, "minecraft:stone");
        w.put(device, block);
        engine.on_placed(&mut w, support);
        engine.on_placed(&mut w, device);
        engine.set_block(&mut w, support, BlockStateId::AIR, SetBlockFlags::PLACE);
        assert_eq!(w.block_name(device), Some("minecraft:air"), "{block}");
    }
}

// ---------------------------------------------------------------------------
// R9 — the divergences the stage-1 review found
// ---------------------------------------------------------------------------

#[test]
fn a_lone_wire_is_a_cross_not_a_dot() {
    // Finding 8: `RedStoneWireBlock.getStateForPlacement` builds the shape from
    // `crossState`, so an isolated dust is a cross (all four sides `side`), not
    // a dot. This also fixes the cross/dot use toggle parity.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let wire = p(0, 0, 0);
    w.put(wire, "minecraft:redstone_wire");
    engine.on_placed(&mut w, wire);
    for dir in Direction::HORIZONTAL {
        assert_eq!(
            props::get(w.state(wire), props::side_prop(dir)),
            Some("side"),
            "{dir:?} should be a side"
        );
    }
}

#[test]
fn wire_connects_to_a_comparator_on_every_side() {
    // Finding 3: `shouldConnectTo` special-cases only the repeater. A comparator
    // is an ordinary signal source, so the wire draws a side toward it.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let wire = p(0, 0, 0);
    let comparator = p(1, 0, 0);
    w.put(wire, "minecraft:redstone_wire");
    w.put(comparator, "minecraft:comparator[facing=north]");
    engine.on_placed(&mut w, wire);
    engine.on_placed(&mut w, comparator);
    assert_eq!(
        props::get(w.state(wire), "east"),
        Some("side"),
        "the wire must connect to the comparator"
    );
}

#[test]
fn wire_climbs_a_slab_and_a_hopper_but_not_their_sides() {
    // Finding 2: the up/side decision uses `isFaceSturdy`, not `isRedstoneConductor`.
    // A bottom slab's top face is sturdy, so the wire climbs; the slab's west
    // face is not, so the connection is `side`, not `up`. A hopper is
    // explicitly allowed by `canSurviveOn` even though its top face is not full.
    let engine = Redstone::new();
    for support in ["minecraft:oak_slab", "minecraft:hopper"] {
        let mut w = MemWorld::new();
        let source = p(0, 0, 0);
        let block = p(1, 0, 0);
        let over = p(1, 1, 0);
        w.put(source, "minecraft:redstone_wire");
        w.put(block, support);
        w.put(over, "minecraft:redstone_wire");
        engine.on_placed(&mut w, source);
        engine.on_placed(&mut w, block);
        engine.on_placed(&mut w, over);
        assert_eq!(
            props::get(w.state(source), "east"),
            Some("side"),
            "{support}: the wire should connect sideways, not up"
        );
    }
}

#[test]
fn a_repeater_placed_on_a_powered_input_waits_one_tick() {
    // Finding 7: `DiodeBlock.setPlacedBy` schedules a tick with delay 1, not
    // `2 * DELAY`. A delay-4 repeater placed on a live input turns on at t = 1.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, 0);
    let repeater = p(1, 0, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    engine.on_placed(&mut w, lever);
    engine.on_use(&mut w, lever);
    w.put(repeater, "minecraft:repeater[facing=west,delay=4]");
    engine.on_placed(&mut w, repeater);

    w.run(&engine, 1);
    assert!(
        is_powered(&w, repeater),
        "a placed diode turns on after exactly one tick"
    );
}

#[test]
fn changing_a_repeaters_delay_notifies_the_front() {
    // Finding 10: `RepeaterBlock.useWithoutItem` writes with flag 3
    // (`UPDATE_NEIGHBORS|UPDATE_CLIENTS`), which runs `onPlace` and so
    // `updateNeighborsInFront`. A stale wire in front is corrected.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let repeater = p(1, 0, 0);
    let wire = p(2, 0, 0);
    w.put(repeater, "minecraft:repeater[facing=west,delay=1]");
    w.put(wire, "minecraft:redstone_wire");
    engine.on_placed(&mut w, repeater);
    engine.on_placed(&mut w, wire);

    // Make the wire's shape stale: a bare dot, as if it never saw the repeater.
    w.put(wire, "minecraft:redstone_wire");
    engine.on_use(&mut w, repeater); // cycles delay 1 -> 2

    assert_eq!(
        props::get(w.state(wire), "west"),
        Some("side"),
        "the delay change must notify the wire in front"
    );
}

#[test]
fn a_stale_tick_is_dropped_when_the_block_changed() {
    // Finding 1: ticks are keyed by (block, pos) and guarded with `is(block)`.
    // A lamp's pending off-tick must not run as the stone button that replaced
    // it, and the button's own tick must still be allowed to coexist.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, 0);
    let lamp = p(1, 0, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(lamp, "minecraft:redstone_lamp");
    engine.on_placed(&mut w, lever);
    engine.on_placed(&mut w, lamp);

    engine.on_use(&mut w, lever); // on -> lamp lit
    assert!(is_lit(&w, lamp));
    engine.on_use(&mut w, lever); // off -> lamp schedules its 4-tick off
    assert!(is_lit(&w, lamp), "the lamp stays lit until its tick");

    // Replace the lamp with a stone button and press it.
    let button = BlockRegistry::new()
        .get("minecraft:stone_button[face=floor,facing=east]")
        .unwrap();
    engine.set_block(&mut w, lamp, button, SetBlockFlags::PLACE);
    engine.on_use(&mut w, lamp);

    // The stale lamp tick would fire at t+4; it must be dropped, and the button
    // must still be held for its full 20 ticks.
    w.run(&engine, 4);
    assert!(
        is_powered(&w, lamp),
        "the stale lamp tick must not release the button"
    );
    w.run(&engine, 16);
    assert!(
        !is_powered(&w, lamp),
        "the button releases after its own 20 ticks"
    );
}

#[test]
fn two_worlds_do_not_share_torch_burnout_history() {
    // N1: vanilla keys the torch toggle log per level (`RECENT_TOGGLES` is a
    // `WeakHashMap<BlockGetter, ...>`). One engine driving two worlds must keep
    // their logs apart: firing a torch in world A must not burn out a fresh
    // torch at the same position in world B.
    let engine = Redstone::new();

    let build = || {
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
        (w, lever, torch)
    };

    // World A: seven off-toggles (the eighth burns out; seven does not).
    let (mut a, lever_a, torch_a) = build();
    for _ in 0..7 {
        engine.on_use(&mut a, lever_a);
        a.run(&engine, 2);
        engine.on_use(&mut a, lever_a);
        a.run(&engine, 2);
    }
    assert!(is_lit(&a, torch_a), "seven toggles leave A's torch lit");

    // World B: one off-toggle at the same position. If the log were shared this
    // would be A's eighth and burn the torch out.
    let (mut b, lever_b, torch_b) = build();
    engine.on_use(&mut b, lever_b);
    b.run(&engine, 2);
    engine.on_use(&mut b, lever_b);
    b.run(&engine, 2);
    assert!(is_lit(&b, torch_b), "B's fresh torch must not be burnt out");
}

#[test]
fn a_shape_change_wakes_a_diagonal_wire() {
    // N4: `RedStoneWireBlock.updateIndirectNeighbourShapes` wakes the wire one
    // block up-and-over. When A's power changes, B (diagonally above A, over a
    // block) must adopt its connection; a direct shape update never reaches B.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(-1, 0, 0);
    let a = p(0, 0, 0);
    let block = p(1, 0, 0);
    let b = p(1, 1, 0);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(a, "minecraft:redstone_wire");
    w.put(block, "minecraft:stone");
    w.put(b, "minecraft:redstone_wire");
    for pos in [lever, a, block, b] {
        engine.on_placed(&mut w, pos);
    }

    // Make B stale (all sides `none`), as if it never saw A.
    w.put(b, "minecraft:redstone_wire");
    // Power A: A changes, and its indirect pass must wake the diagonal B.
    engine.on_use(&mut w, lever);
    assert_eq!(
        props::get(w.state(b), "west"),
        Some("side"),
        "the diagonal wire must wake on A's change"
    );
}

#[test]
fn a_repeater_recomputes_its_lock_on_a_vertical_update() {
    // N5: `RepeaterBlock.updateShape` recomputes LOCKED for any direction off
    // the facing axis, including UP/DOWN.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let lever = p(0, 0, 0);
    let repeater = p(1, 0, 0);
    let lock = p(1, 0, -1);
    let lock_source = p(1, 0, -2);
    w.put(lever, "minecraft:lever[face=floor,facing=east]");
    w.put(repeater, "minecraft:repeater[facing=west,delay=1]");
    w.put(lock, "minecraft:repeater[facing=north,delay=1]");
    w.put(lock_source, "minecraft:redstone_block");
    for pos in [lever, repeater, lock, lock_source] {
        engine.on_placed(&mut w, pos);
    }
    w.run(&engine, 2);
    assert!(is_powered(&w, lock));

    // Reset `locked` behind the engine's back, then nudge the cell above the
    // repeater; only a vertical recompute restores `locked`.
    w.put(repeater, "minecraft:repeater[facing=west,delay=1]");
    assert_eq!(props::get(w.state(repeater), "locked"), Some("false"));
    w.put(p(1, 1, 0), "minecraft:stone");
    engine.on_placed(&mut w, p(1, 1, 0));
    assert_eq!(
        props::get(w.state(repeater), "locked"),
        Some("true"),
        "a vertical update must recompute the lock"
    );
}

#[test]
fn skip_shape_update_on_wire_suppresses_the_wire_shape_update() {
    // N7: `NeighborUpdater.executeShapeUpdate` skips redstone wire when the
    // `UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE` (128) flag is set.
    let engine = Redstone::new();
    let rb = BlockRegistry::new()
        .get("minecraft:redstone_block")
        .unwrap();

    // With the flag: the wire next to the new block is left untouched.
    let mut w = MemWorld::new();
    let wire = p(0, 0, 0);
    let block = p(1, 0, 0);
    w.put(wire, "minecraft:redstone_wire"); // stale, all sides `none`
    engine.set_block(
        &mut w,
        block,
        rb,
        SetBlockFlags::CLIENTS.union(SetBlockFlags::UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE),
    );
    assert_eq!(
        props::get(w.state(wire), "east"),
        Some("none"),
        "the wire's shape update must be skipped"
    );

    // Without the flag: the wire connects to the new signal source.
    let mut w = MemWorld::new();
    w.put(wire, "minecraft:redstone_wire");
    engine.set_block(&mut w, block, rb, SetBlockFlags::CLIENTS);
    assert_eq!(
        props::get(w.state(wire), "east"),
        Some("side"),
        "without the flag the wire must update"
    );
}

#[test]
fn a_twenty_by_twenty_wire_patch_resolves_to_vanilla_levels() {
    // D1: a single cascade larger than the old 512-entry cap must resolve fully.
    // A redstone block in the middle of a flat wire patch powers every wire,
    // with power = 16 - Manhattan distance (15 adjacent, one less per block,
    // zero past 15). This is ~400 wire cells in one cascade.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    const N: i32 = 20;
    let center = (N / 2, N / 2);
    for x in 0..N {
        for z in 0..N {
            w.put(p(x, 0, z), "minecraft:redstone_wire");
        }
    }
    let block = p(center.0, 0, center.1);
    w.put(block, "minecraft:redstone_block");
    engine.on_placed(&mut w, block);

    for x in 0..N {
        for z in 0..N {
            if (x, z) == center {
                continue;
            }
            let dist = (x - center.0).abs() + (z - center.1).abs();
            let expected = if (1..=15).contains(&dist) {
                (16 - dist) as u8
            } else {
                0
            };
            assert_eq!(
                power(&w, p(x, 0, z)),
                expected,
                "wire at ({x},{z}), Manhattan {dist}"
            );
        }
    }
}

#[test]
fn the_neighbor_update_chain_limit_is_configurable() {
    // D1: the cap is the world's `maxChainedNeighborUpdates`, not a hard 512.
    // The same patch under a deliberately tiny limit leaves cells uncharged.
    fn charged(limit: Option<usize>) -> u32 {
        let engine = Redstone::new();
        let mut w = match limit {
            Some(n) => MemWorld::new().with_max_chained(n),
            None => MemWorld::new(),
        };
        const N: i32 = 20;
        for x in 0..N {
            for z in 0..N {
                w.put(p(x, 0, z), "minecraft:redstone_wire");
            }
        }
        let block = p(N / 2, 0, N / 2);
        w.put(block, "minecraft:redstone_block");
        engine.on_placed(&mut w, block);
        let mut sum = 0u32;
        for x in 0..N {
            for z in 0..N {
                sum += u32::from(power(&w, p(x, 0, z)));
            }
        }
        sum
    }

    let full = charged(None);
    let truncated = charged(Some(16));
    assert!(full > 0);
    assert!(
        truncated < full,
        "a tiny limit must truncate the cascade ({truncated} vs {full})"
    );
}

#[test]
fn a_standing_torch_survives_on_a_fence_or_a_hopper() {
    // D3: `BaseTorchBlock.canSurvive` uses `canSupportCenter` (SupportType.CENTER),
    // not the FULL face test. A fence's central post and a hopper's funnel each
    // centre-support a torch, so a DOWN shape update must not remove it.
    let engine = Redstone::new();
    for (support, other) in [
        (
            "minecraft:oak_fence",
            "minecraft:oak_fence[waterlogged=true]",
        ),
        ("minecraft:hopper", "minecraft:hopper[enabled=false]"),
    ] {
        let mut w = MemWorld::new();
        let below = p(0, 0, 0);
        let torch = p(0, 1, 0);
        w.put(below, support);
        w.put(torch, "minecraft:redstone_torch");
        engine.on_placed(&mut w, below);
        engine.on_placed(&mut w, torch);
        assert_eq!(w.block_name(torch), Some("minecraft:redstone_torch"));

        // Re-place the support with a different state: that runs the shape pass
        // and gives the torch a DOWN update_shape.
        let other_state = BlockRegistry::new()
            .get(other)
            .unwrap_or_else(|| panic!("state {other}"));
        engine.set_block(&mut w, below, other_state, SetBlockFlags::PLACE);
        assert_eq!(
            w.block_name(torch),
            Some("minecraft:redstone_torch"),
            "{support}: a centre-supported torch must survive"
        );
    }
}

#[test]
fn a_destroyed_component_drops_its_item() {
    // N8 (partial): a wire that pops off when its support is removed is
    // destroyed — vanilla `Block.updateOrDestroy` calls `destroyBlock(drop=true)`,
    // so the host must see the drop. The engine reports it through
    // `RedstoneView::drop_block`.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let support = p(0, 0, 0);
    let wire = p(0, 1, 0);
    w.put(support, "minecraft:stone");
    w.put(wire, "minecraft:redstone_wire");
    engine.on_placed(&mut w, support);
    engine.on_placed(&mut w, wire);
    let wire_state = w.state(wire);
    assert!(w.dropped().is_empty(), "nothing dropped yet");

    engine.set_block(&mut w, support, BlockStateId::AIR, SetBlockFlags::PLACE);
    assert_eq!(
        w.block_name(wire),
        Some("minecraft:air"),
        "the wire must pop off"
    );
    assert_eq!(
        w.dropped(),
        &[(wire, wire_state)],
        "the wire's item must be dropped at its cell"
    );
}
