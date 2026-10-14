//! The bits of `net.minecraft.util.Mth` whose exact values matter: the
//! table-driven `sin`/`cos` every carver and many features steer by.

use std::sync::OnceLock;

fn table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        (0..65536)
            .map(|i| (i as f64 / 10_430.378_350_470_453).sin() as f32)
            .collect()
    })
}

/// `Mth.sin`: a 65536-entry lookup, not `f64::sin`.
#[inline]
pub fn sin(v: f64) -> f32 {
    table()[((v * 10_430.378_350_470_453) as i64 & 65535) as usize]
}

/// `Mth.cos`.
#[inline]
pub fn cos(v: f64) -> f32 {
    table()[((v * 10_430.378_350_470_453 + 16_384.0) as i64 & 65535) as usize]
}

/// `Mth.floor`.
#[inline]
pub fn floor(v: f64) -> i32 {
    v.floor() as i32
}

/// `Mth.lerp`.
#[inline]
pub fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// `Mth.getSeed(x, y, z)` — one of the few integer hashes the game uses both
/// to seed a positional random and to key structure placement.
#[inline]
pub fn get_seed(x: i32, y: i32, z: i32) -> i64 {
    let mut l =
        (x as i64).wrapping_mul(3_129_871) ^ (z as i64).wrapping_mul(116_129_781) ^ y as i64;
    l = l
        .wrapping_mul(l)
        .wrapping_mul(42_317_861)
        .wrapping_add(l.wrapping_mul(11));
    l >> 16
}

/// `Mth.clampedMap`.
pub fn clamped_map(v: f64, from_a: f64, from_b: f64, to_a: f64, to_b: f64) -> f64 {
    let t = (v - from_a) / (from_b - from_a);
    if t < 0.0 {
        to_a
    } else if t > 1.0 {
        to_b
    } else {
        lerp(t, to_a, to_b)
    }
}
