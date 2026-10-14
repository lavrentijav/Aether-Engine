//! Target block circuits (`TargetBlock`).

use aether_redstone::{props, BlockPos, Direction, MemWorld, Redstone};

fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn power(w: &MemWorld, pos: BlockPos) -> u8 {
    props::get(w.state(pos), "power")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[test]
fn a_centre_hit_gives_fifteen() {
    // Hitting the exact centre of the north face: d=e=0 → 15.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let target = p(0, 0, 0);
    w.put(target, "minecraft:target");
    engine.on_placed(&mut w, target);

    engine.on_projectile_hit(&mut w, target, Direction::North, [0.5, 0.5, 0.5], true);
    assert_eq!(power(&w, target), 15);
    assert_eq!(engine.get_signal(&w, target, Direction::Up), 15);
}

#[test]
fn an_off_centre_hit_gives_less() {
    // North face uses max(d, e) = max(|x-0.5|, |y-0.5|). At x = 0.9 the
    // distance is 0.4, so the signal is ceil(15 * (0.5-0.4)/0.5) = 3.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let target = p(0, 0, 0);
    w.put(target, "minecraft:target");
    engine.on_placed(&mut w, target);
    engine.on_projectile_hit(&mut w, target, Direction::North, [0.1, 0.5, 0.5], true);
    assert_eq!(power(&w, target), 3);
}

#[test]
fn the_target_resets_after_twenty_ticks_for_an_arrow() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let target = p(0, 0, 0);
    w.put(target, "minecraft:target");
    engine.on_placed(&mut w, target);
    engine.on_projectile_hit(&mut w, target, Direction::North, [0.5, 0.5, 0.5], true);

    w.run(&engine, 19);
    assert_eq!(power(&w, target), 15, "still on at 19 ticks");
    w.run(&engine, 1);
    assert_eq!(power(&w, target), 0, "off at 20 ticks");
}

#[test]
fn a_non_arrow_resets_after_eight_ticks() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let target = p(0, 0, 0);
    w.put(target, "minecraft:target");
    engine.on_placed(&mut w, target);
    engine.on_projectile_hit(&mut w, target, Direction::North, [0.5, 0.5, 0.5], false);

    w.run(&engine, 7);
    assert_eq!(power(&w, target), 15);
    w.run(&engine, 1);
    assert_eq!(power(&w, target), 0);
}

#[test]
fn a_target_powers_a_lamp() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let target = p(0, 0, 0);
    let lamp = p(1, 0, 0);
    w.put(target, "minecraft:target");
    w.put(lamp, "minecraft:redstone_lamp");
    engine.on_placed(&mut w, target);
    engine.on_placed(&mut w, lamp);
    engine.on_projectile_hit(&mut w, target, Direction::North, [0.5, 0.5, 0.5], true);
    assert_eq!(props::get(w.state(lamp), "lit"), Some("true"));
}
