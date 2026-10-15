//! Pressure plate circuits (`BasePressurePlateBlock`).

use aether_redstone::{props, BlockPos, Direction, MemWorld, Redstone, SetBlockFlags};

fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn power(w: &MemWorld, pos: BlockPos) -> u8 {
    props::get(w.state(pos), "power")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn signal(w: &MemWorld, pos: BlockPos) -> u8 {
    Redstone::new().get_signal(w, pos, Direction::Up)
}

#[test]
fn a_stone_pressure_plate_powers_when_stepped_on() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let plate = p(0, 0, 0);
    w.put(p(0, -1, 0), "minecraft:stone");
    w.put(plate, "minecraft:stone_pressure_plate");
    engine.on_placed(&mut w, p(0, -1, 0));
    engine.on_placed(&mut w, plate);
    assert_eq!(signal(&w, plate), 0);

    w.set_entities(plate, 1);
    engine.on_entity_inside(&mut w, plate);
    assert_eq!(props::get(w.state(plate), "powered"), Some("true"));
    assert_eq!(signal(&w, plate), 15);

    // Stepping off: the scheduled re-check sees zero entities.
    w.set_entities(plate, 0);
    w.run(&engine, 20);
    assert_eq!(props::get(w.state(plate), "powered"), Some("false"));
    assert_eq!(signal(&w, plate), 0);
}

#[test]
fn a_light_weighted_plate_scales_with_the_count() {
    // maxWeight 15: count 1 -> ceil(1/15*15)=1, count 7 -> 7, count 15+ -> 15.
    let engine = Redstone::new();
    for (count, expected) in [(0usize, 0u8), (1, 1), (7, 7), (15, 15), (40, 15)] {
        let mut w = MemWorld::new();
        let plate = p(0, 0, 0);
        w.put(p(0, -1, 0), "minecraft:stone");
        w.put(plate, "minecraft:light_weighted_pressure_plate");
        engine.on_placed(&mut w, p(0, -1, 0));
        engine.on_placed(&mut w, plate);
        w.set_entities(plate, count);
        engine.on_entity_inside(&mut w, plate);
        assert_eq!(power(&w, plate), expected, "count {count}");
    }
}

#[test]
fn a_heavy_weighted_plate_scales_to_150() {
    // maxWeight 150: count 15 -> ceil(15/150*15)=2.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let plate = p(0, 0, 0);
    w.put(p(0, -1, 0), "minecraft:stone");
    w.put(plate, "minecraft:heavy_weighted_pressure_plate");
    engine.on_placed(&mut w, p(0, -1, 0));
    engine.on_placed(&mut w, plate);
    w.set_entities(plate, 15);
    engine.on_entity_inside(&mut w, plate);
    assert_eq!(power(&w, plate), 2);
    // While pressed, a change in count is caught by the 10-tick re-check.
    w.set_entities(plate, 150);
    w.run(&engine, 10);
    assert_eq!(power(&w, plate), 15);
}

#[test]
fn a_pressure_plate_needs_a_support() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let support = p(0, -1, 0);
    let plate = p(0, 0, 0);
    w.put(support, "minecraft:stone");
    w.put(plate, "minecraft:stone_pressure_plate");
    engine.on_placed(&mut w, support);
    engine.on_placed(&mut w, plate);
    assert_eq!(w.block_name(plate), Some("minecraft:stone_pressure_plate"));

    engine.set_block(
        &mut w,
        support,
        aether_world::BlockStateId::AIR,
        SetBlockFlags::PLACE,
    );
    assert_eq!(
        w.block_name(plate),
        Some("minecraft:air"),
        "the plate pops off"
    );
}

#[test]
fn a_pressure_plate_powers_a_lamp() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let plate = p(0, 0, 0);
    let lamp = p(0, 1, 0);
    w.put(p(0, -1, 0), "minecraft:stone");
    w.put(plate, "minecraft:stone_pressure_plate");
    w.put(lamp, "minecraft:redstone_lamp");
    for pos in [p(0, -1, 0), plate, lamp] {
        engine.on_placed(&mut w, pos);
    }
    w.set_entities(plate, 1);
    engine.on_entity_inside(&mut w, plate);
    assert_eq!(props::get(w.state(lamp), "lit"), Some("true"));
}
