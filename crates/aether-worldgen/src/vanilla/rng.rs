//! Vanilla's `RandomSource` surface, over both of the game's generators.
//!
//! Carvers and features do not draw from the bare xoroshiro stream that the
//! noise stack uses. They go through `WorldgenRandom`, a wrapper that
//! re-seeds its inner source per chunk / per feature and funnels every draw
//! through a single `next(bits)` — so `nextInt(bound)`, `nextDouble()` and
//! friends follow the *legacy* `java.util.Random` formulas even when the
//! inner source is xoroshiro. Getting that wrong produces values in the right
//! range from the wrong sequence, which no range check would ever catch.
//!
//! * [`Rng`] is `RandomSource`: the draw helpers every caller uses.
//! * [`LegacyRandom`] is `LegacyRandomSource`, the 48-bit LCG.
//! * [`WorldgenRandom`] is `WorldgenRandom` over either inner source.
//! * [`super::random::XoroshiroRandom`] implements [`Rng`] with
//!   `XoroshiroRandomSource`'s own (different) helpers.

use super::random::XoroshiroRandom;

/// `RandomSource`.
pub trait Rng {
    /// `nextInt()`.
    fn next_int(&mut self) -> i32;
    /// `nextInt(bound)`, `bound > 0`.
    fn next_int_bounded(&mut self, bound: i32) -> i32;
    /// `nextLong()`.
    fn next_long(&mut self) -> i64;
    /// `nextBoolean()`.
    fn next_bool(&mut self) -> bool;
    /// `nextFloat()`.
    fn next_float(&mut self) -> f32;
    /// `nextDouble()`.
    fn next_double(&mut self) -> f64;
    /// `nextGaussian()`.
    fn next_gaussian(&mut self) -> f64;

    /// `nextInt(origin, bound)`: `origin + nextInt(bound - origin)`.
    fn next_int_range(&mut self, origin: i32, bound: i32) -> i32 {
        if origin >= bound {
            return origin;
        }
        origin + self.next_int_bounded(bound - origin)
    }

    /// `nextIntBetweenInclusive(min, max)`.
    fn next_int_between_inclusive(&mut self, min: i32, max: i32) -> i32 {
        self.next_int_bounded(max - min + 1) + min
    }

    /// `triangle(center, deviation)`.
    fn triangle(&mut self, center: f64, deviation: f64) -> f64 {
        center + deviation * (self.next_double() - self.next_double())
    }

    /// `consumeCount(n)`.
    fn consume(&mut self, n: i32) {
        for _ in 0..n {
            self.next_int();
        }
    }
}

/// `MarsagliaPolarGaussian`'s state.
#[derive(Debug, Clone, Copy, Default)]
struct Gaussian {
    next: f64,
    have_next: bool,
}

impl Gaussian {
    fn draw(&mut self, mut next_double: impl FnMut() -> f64) -> f64 {
        if self.have_next {
            self.have_next = false;
            return self.next;
        }
        loop {
            let a = 2.0 * next_double() - 1.0;
            let b = 2.0 * next_double() - 1.0;
            let s = a * a + b * b;
            if s < 1.0 && s != 0.0 {
                let m = (-2.0 * s.ln() / s).sqrt();
                self.next = b * m;
                self.have_next = true;
                return a * m;
            }
        }
    }
}

const LCG_MUL: i64 = 0x5_DEEC_E66D;
const LCG_MASK: i64 = (1 << 48) - 1;

/// `LegacyRandomSource` — `java.util.Random`'s LCG.
#[derive(Debug, Clone, Copy)]
pub struct LegacyRandom {
    seed: i64,
    gaussian: Gaussian,
}

impl LegacyRandom {
    /// Seeded like `new LegacyRandomSource(seed)`.
    pub fn new(seed: i64) -> Self {
        let mut r = Self {
            seed: 0,
            gaussian: Gaussian::default(),
        };
        r.set_seed(seed);
        r
    }

    /// `setSeed`.
    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ LCG_MUL) & LCG_MASK;
        self.gaussian = Gaussian::default();
    }

    /// `next(bits)`.
    #[inline]
    pub fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(LCG_MUL).wrapping_add(11) & LCG_MASK;
        (self.seed >> (48 - bits)) as i32
    }
}

/// The `BitRandomSource` helpers, over any `next(bits)`.
macro_rules! bit_helpers {
    () => {
        fn next_int(&mut self) -> i32 {
            self.next(32)
        }

        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            debug_assert!(bound > 0, "bound must be positive");
            if bound <= 0 {
                return 0;
            }
            if bound & (bound - 1) == 0 {
                return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
            }
            loop {
                let bits = self.next(31);
                let val = bits % bound;
                if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                    return val;
                }
            }
        }

        fn next_long(&mut self) -> i64 {
            let hi = self.next(32) as i64;
            let lo = self.next(32) as i64;
            (hi << 32).wrapping_add(lo)
        }

        fn next_bool(&mut self) -> bool {
            self.next(1) != 0
        }

        fn next_float(&mut self) -> f32 {
            self.next(24) as f32 * 5.960_464_5E-8_f32
        }

        fn next_double(&mut self) -> f64 {
            let a = self.next(26) as i64;
            let b = self.next(27) as i64;
            // `DOUBLE_MULTIPLIER` is declared as a float literal widened to
            // double: exactly 2^-53.
            ((a << 27) + b) as f64 * (1.110_223E-16_f32 as f64)
        }
    };
}

impl Rng for LegacyRandom {
    bit_helpers!();

    fn next_gaussian(&mut self) -> f64 {
        let mut g = self.gaussian;
        let v = g.draw(|| self.next_double());
        self.gaussian = g;
        v
    }
}

/// The inner source of a [`WorldgenRandom`].
#[derive(Debug, Clone, Copy)]
enum Inner {
    Legacy(LegacyRandom),
    Xoroshiro(XoroshiroRandom),
}

/// `WorldgenRandom`: re-seedable, every draw funnelled through `next(bits)`.
#[derive(Debug, Clone, Copy)]
pub struct WorldgenRandom {
    inner: Inner,
    /// Vanilla never resets this one on `setSeed` (the override skips the
    /// reset its parent class does), so neither do we.
    gaussian: Gaussian,
}

impl WorldgenRandom {
    /// `new WorldgenRandom(new LegacyRandomSource(seed))` — what carvers use.
    pub fn legacy(seed: i64) -> Self {
        Self {
            inner: Inner::Legacy(LegacyRandom::new(seed)),
            gaussian: Gaussian::default(),
        }
    }

    /// `new WorldgenRandom(new XoroshiroRandomSource(seed))` — what feature
    /// decoration uses.
    pub fn xoroshiro(seed: i64) -> Self {
        Self {
            inner: Inner::Xoroshiro(XoroshiroRandom::new(seed as u64)),
            gaussian: Gaussian::default(),
        }
    }

    /// `next(bits)`.
    #[inline]
    pub fn next(&mut self, bits: u32) -> i32 {
        match &mut self.inner {
            Inner::Legacy(l) => l.next(bits),
            Inner::Xoroshiro(x) => (x.next_u64() >> (64 - bits)) as i32,
        }
    }

    /// `setSeed`: re-seeds the inner source.
    pub fn set_seed(&mut self, seed: i64) {
        match &mut self.inner {
            Inner::Legacy(l) => l.set_seed(seed),
            Inner::Xoroshiro(x) => *x = XoroshiroRandom::new(seed as u64),
        }
    }

    /// `setDecorationSeed(levelSeed, minBlockX, minBlockZ)`.
    pub fn set_decoration_seed(&mut self, level_seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(level_seed);
        let a = self.next_long() | 1;
        let b = self.next_long() | 1;
        let s = (x as i64)
            .wrapping_mul(a)
            .wrapping_add((z as i64).wrapping_mul(b))
            ^ level_seed;
        self.set_seed(s);
        s
    }

    /// `setFeatureSeed(decorationSeed, index, step)`.
    pub fn set_feature_seed(&mut self, decoration_seed: i64, index: i32, step: i32) {
        let s = decoration_seed
            .wrapping_add(index as i64)
            .wrapping_add(10_000i64.wrapping_mul(step as i64));
        self.set_seed(s);
    }

    /// `setLargeFeatureSeed(seed, chunkX, chunkZ)`.
    pub fn set_large_feature_seed(&mut self, seed: i64, cx: i32, cz: i32) {
        self.set_seed(seed);
        let a = self.next_long();
        let b = self.next_long();
        let s = (cx as i64).wrapping_mul(a) ^ (cz as i64).wrapping_mul(b) ^ seed;
        self.set_seed(s);
    }

    /// `setLargeFeatureWithSalt(levelSeed, regionX, regionZ, salt)`.
    pub fn set_large_feature_with_salt(&mut self, level_seed: i64, rx: i32, rz: i32, salt: i32) {
        let s = (rx as i64)
            .wrapping_mul(341_873_128_712)
            .wrapping_add((rz as i64).wrapping_mul(132_897_987_541))
            .wrapping_add(level_seed)
            .wrapping_add(salt as i64);
        self.set_seed(s);
    }
}

impl Rng for WorldgenRandom {
    bit_helpers!();

    fn next_gaussian(&mut self) -> f64 {
        let mut g = self.gaussian;
        let v = g.draw(|| self.next_double());
        self.gaussian = g;
        v
    }
}

impl Rng for XoroshiroRandom {
    fn next_int(&mut self) -> i32 {
        self.next_i32()
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        self.next_i32_bounded(bound)
    }

    fn next_long(&mut self) -> i64 {
        self.next_i64()
    }

    fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 != 0
    }

    fn next_float(&mut self) -> f32 {
        self.next_f32()
    }

    fn next_double(&mut self) -> f64 {
        self.next_f64()
    }

    fn next_gaussian(&mut self) -> f64 {
        // Only reached by callers that never draw a second gaussian from the
        // same fresh source, so the cached half is never observed.
        Gaussian::default().draw(|| self.next_f64())
    }

    fn consume(&mut self, n: i32) {
        for _ in 0..n {
            self.next_u64();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_matches_java_util_random() {
        // new java.util.Random(42): nextInt() = -1170105035, nextInt(10) = 0
        // (second draw), nextLong() follows.
        let mut r = LegacyRandom::new(42);
        assert_eq!(r.next_int(), -1_170_105_035);
        assert_eq!(r.next_int_bounded(10), 3);
        let mut r = LegacyRandom::new(0);
        assert_eq!(r.next_long(), -4_962_768_465_676_381_896);
        // Vanilla's multiplier is a float literal, which rounds to exactly
        // 2^-53 — so this is bit-identical to java.util.Random.
        let mut r = LegacyRandom::new(1);
        assert_eq!(r.next_double(), 0.730_878_190_703_290_9);
    }
}
