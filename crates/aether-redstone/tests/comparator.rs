//! Comparator circuits (`ComparatorBlock`).
//!
//! The comparator is a diode: input from the block in front (`facing` side),
//! output to the opposite side. Its front input is analog when the block in
//! front (or the block behind a conductor in front) reports a container fill
//! level; its side inputs set compare/subtract.

use aether_redstone::{props, BlockPos, Direction, MemWorld, Redstone, RedstoneView};
use aether_world::registry::BlockRegistry;
use aether_world::BlockStateId;

fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn state(name: &str) -> BlockStateId {
    BlockRegistry::new().get(name).unwrap()
}

/// A comparator facing west: input from the west, output to the east. The two
/// side inputs are north and south.
fn comparator(mode: &str) -> &'static str {
    match mode {
        "subtract" => "minecraft:comparator[facing=west,mode=subtract]",
        _ => "minecraft:comparator[facing=west,mode=compare]",
    }
}

fn output(w: &MemWorld, comparator: BlockPos) -> u8 {
    // `getSignal` returns the output only toward the facing (`west`).
    Redstone::new().get_signal(w, comparator, Direction::West)
}

/// Build the circuit and kick the comparator so it schedules its refresh tick.
fn kick(engine: &Redstone, w: &mut MemWorld, comparator_pos: BlockPos, from: BlockPos) {
    engine.on_neighbour_changed(w, comparator_pos, from);
    w.run(engine, 2);
}

#[test]
fn compare_mode_passes_the_input_through() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(1, 0, 0);
    let input = p(0, 0, 0);
    w.put(input, "minecraft:redstone_block");
    w.put(comparator_pos, comparator("compare"));
    engine.on_placed(&mut w, input);
    engine.on_placed(&mut w, comparator_pos);
    w.run(&engine, 2);

    assert_eq!(props::get(w.state(comparator_pos), "powered"), Some("true"));
    assert_eq!(output(&w, comparator_pos), 15, "compare passes 15 through");
}

#[test]
fn compare_mode_holds_a_ten_against_a_ten() {
    // A comparator with a 10 input and a 10 side signal: compare treats the
    // equality as "on", so the 10 passes through.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(1, 0, 0);
    w.put(p(0, 0, 0), "minecraft:chest");
    w.set_analog_output(p(0, 0, 0), 10);
    w.set_block(p(1, 0, -1), state("minecraft:redstone_wire[power=10]"));
    w.put(comparator_pos, comparator("compare"));
    kick(&engine, &mut w, comparator_pos, p(1, 0, -1));

    assert_eq!(props::get(w.state(comparator_pos), "powered"), Some("true"));
    assert_eq!(output(&w, comparator_pos), 10);
}

#[test]
fn subtract_mode_takes_the_side_signal_off() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(1, 0, 0);
    w.put(p(0, 0, 0), "minecraft:chest");
    w.set_analog_output(p(0, 0, 0), 15);
    w.set_block(p(1, 0, -1), state("minecraft:redstone_wire[power=5]"));
    w.put(comparator_pos, comparator("subtract"));
    kick(&engine, &mut w, comparator_pos, p(1, 0, -1));

    assert_eq!(
        props::get(w.state(comparator_pos), "powered"),
        Some("true"),
        "15 > 5"
    );
    assert_eq!(output(&w, comparator_pos), 10, "15 - 5");
}

#[test]
fn subtract_mode_outputs_nothing_when_the_side_wins() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(1, 0, 0);
    w.put(p(0, 0, 0), "minecraft:chest");
    w.set_analog_output(p(0, 0, 0), 5);
    w.set_block(p(1, 0, -1), state("minecraft:redstone_wire[power=15]"));
    w.put(comparator_pos, comparator("subtract"));
    kick(&engine, &mut w, comparator_pos, p(1, 0, -1));

    assert_eq!(
        props::get(w.state(comparator_pos), "powered"),
        Some("false")
    );
    assert_eq!(output(&w, comparator_pos), 0, "side 15 beats input 5");
}

#[test]
fn a_comparator_reads_a_container_fill_level() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(1, 0, 0);
    let chest = p(0, 0, 0);
    w.put(chest, "minecraft:chest");
    w.put(comparator_pos, comparator("compare"));
    w.set_analog_output(chest, 7);
    kick(&engine, &mut w, comparator_pos, chest);

    assert_eq!(output(&w, comparator_pos), 7, "container fill level");
}

#[test]
fn a_comparator_reads_a_container_through_a_conductor() {
    // Vanilla reads the analog output of the block two cells ahead when the
    // block directly in front is a conductor.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(2, 0, 0);
    let conductor = p(1, 0, 0);
    let chest = p(0, 0, 0);
    w.put(conductor, "minecraft:stone");
    w.put(chest, "minecraft:chest");
    w.put(comparator_pos, comparator("compare"));
    w.set_analog_output(chest, 9);
    kick(&engine, &mut w, comparator_pos, conductor);

    assert_eq!(output(&w, comparator_pos), 9, "behind the conductor");
}

#[test]
fn using_a_comparator_cycles_its_mode() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let comparator_pos = p(1, 0, 0);
    w.put(comparator_pos, comparator("compare"));
    engine.on_placed(&mut w, comparator_pos);
    engine.on_use(&mut w, comparator_pos);
    assert_eq!(
        props::get(w.state(comparator_pos), "mode"),
        Some("subtract")
    );
    engine.on_use(&mut w, comparator_pos);
    assert_eq!(props::get(w.state(comparator_pos), "mode"), Some("compare"));
}

#[test]
fn a_comparator_stands_on_a_hopper() {
    // Same RIGID base as the repeater.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let hopper = p(0, 0, 0);
    let comparator_pos = p(0, 1, 0);
    w.put(hopper, "minecraft:hopper");
    w.put(comparator_pos, comparator("compare"));
    engine.on_placed(&mut w, hopper);
    engine.on_placed(&mut w, comparator_pos);
    engine.set_block(
        &mut w,
        hopper,
        state("minecraft:hopper[enabled=false]"),
        aether_redstone::SetBlockFlags::PLACE,
    );
    assert_eq!(w.block_name(comparator_pos), Some("minecraft:comparator"));
}
