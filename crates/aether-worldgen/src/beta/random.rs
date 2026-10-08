//! A Java-compatible 48-bit linear congruential generator.
//!
//! Vanilla Minecraft (Alpha through the "old" noise generator that survived,
//! renamed, all the way to 1.18's `minecraft:old_blended_noise`) seeds every
//! noise octave from `java.util.Random`. We reproduce that exact LCG — not
//! because bit-parity with a live Java server matters here, but because it's
//! the cheapest way to get statistically independent, well-distributed
//! permutation tables per octave from a single `u64` world seed, the same way
//! the original does.

/// Java's `java.util.Random`, minus the parts we don't need (no Gaussian
/// cache, no thread safety).
pub struct JavaRandom {
    seed: u64,
}

const MULTIPLIER: u64 = 0x5_DEEC_E66D;
const ADDEND: u64 = 0xB;
const MASK: u64 = (1 << 48) - 1;

impl JavaRandom {
    pub fn new(seed: u64) -> Self {
        Self {
            seed: (seed ^ MULTIPLIER) & MASK,
        }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = (self.seed.wrapping_mul(MULTIPLIER).wrapping_add(ADDEND)) & MASK;
        (self.seed >> (48 - bits)) as i32
    }

    /// `java.util.Random#nextInt(int)`: uniform in `0..bound`.
    pub fn next_int(&mut self, bound: i32) -> i32 {
        debug_assert!(bound > 0);
        if bound & (-bound) == bound {
            // Power of two: a single scaled draw, no rejection needed.
            return (((bound as i64) * (self.next(31) as i64)) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits - val + (bound - 1) >= 0 {
                return val;
            }
        }
    }

    /// `java.util.Random#nextDouble()`: uniform in `[0, 1)`.
    pub fn next_double(&mut self) -> f64 {
        let hi = (self.next(26) as i64) << 27;
        let lo = self.next(27) as i64;
        ((hi + lo) as f64) * (1.0 / (1i64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_java_sequence() {
        // `new Random(42).nextInt(100)` three times — cross-checked against an
        // independent implementation of the same 48-bit LCG.
        let mut r = JavaRandom::new(42);
        assert_eq!(r.next_int(100), 30);
        assert_eq!(r.next_int(100), 63);
        assert_eq!(r.next_int(100), 48);
    }

    #[test]
    fn next_double_is_unit_range() {
        let mut r = JavaRandom::new(1234);
        for _ in 0..1000 {
            let v = r.next_double();
            assert!((0.0..1.0).contains(&v), "out of range: {v}");
        }
    }
}
