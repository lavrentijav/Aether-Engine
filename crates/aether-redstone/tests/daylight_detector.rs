//! Daylight detector circuits (`DaylightDetectorBlock`).

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
fn a_daylight_detector_tracks_the_sun() {
    // Noon: sky light 15 and the sun overhead (angle 0) → power 15.
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let detector = p(0, 0, 0);
    w.put(detector, "minecraft:daylight_detector");
    w.set_daylight(15, 0.0);
    engine.on_placed(&mut w, detector);
    assert_eq!(power(&w, detector), 15);
    assert_eq!(engine.get_signal(&w, detector, Direction::Up), 15);

    // Later in the day the cosine term lowers the signal; below the horizon it
    // is zero.
    w.set_daylight(15, std::f32::consts::PI * 0.5);
    w.run(&engine, 20);
    assert!(power(&w, detector) < 15);

    w.set_daylight(0, std::f32::consts::PI);
    w.run(&engine, 20);
    assert_eq!(power(&w, detector), 0, "night");
}

#[test]
fn an_inverted_daylight_detector_reports_the_complement() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let detector = p(0, 0, 0);
    w.put(detector, "minecraft:daylight_detector[inverted=true]");
    w.set_daylight(15, 0.0);
    engine.on_placed(&mut w, detector);
    // Inverted ignores the sun angle: 15 - 15 = 0.
    assert_eq!(power(&w, detector), 0);

    w.set_daylight(0, 0.0);
    w.run(&engine, 20);
    assert_eq!(power(&w, detector), 15, "15 - 0");
}

#[test]
fn using_a_daylight_detector_toggles_inversion() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let detector = p(0, 0, 0);
    w.put(detector, "minecraft:daylight_detector");
    w.set_daylight(10, 0.0);
    engine.on_placed(&mut w, detector);
    let before = power(&w, detector);
    engine.on_use(&mut w, detector);
    assert_eq!(props::get(w.state(detector), "inverted"), Some("true"));
    assert_eq!(power(&w, detector), 15 - before);
}

#[test]
fn a_daylight_detector_reschedules_itself() {
    let engine = Redstone::new();
    let mut w = MemWorld::new();
    let detector = p(0, 0, 0);
    w.put(detector, "minecraft:daylight_detector");
    w.set_daylight(12, 0.0);
    engine.on_placed(&mut w, detector);
    w.run(&engine, 20);
    assert_eq!(power(&w, detector), 12, "still tracking");
}
