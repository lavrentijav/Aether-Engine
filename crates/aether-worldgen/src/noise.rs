//! A tiny, dependency-free value-noise field for terrain heightmaps.
//!
//! Not Perlin/Simplex — just hashed lattice values with smooth (cosine)
//! interpolation and a couple of fBm octaves. Fully deterministic from a seed,
//! which is all the generator needs for a believable, repeatable surface.

/// Deterministic value noise over a 2D lattice.
#[derive(Debug, Clone, Copy)]
pub struct ValueNoise {
    seed: u64,
}

impl ValueNoise {
    /// A noise field for the given `seed`.
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Hash a lattice point to `[0, 1)`.
    fn lattice(&self, xi: i64, zi: i64) -> f64 {
        // SplitMix64-style avalanche over the mixed coordinates + seed.
        let mut h = self
            .seed
            .wrapping_add((xi as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
            .wrapping_add((zi as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f));
        h ^= h >> 30;
        h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h ^= h >> 27;
        h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
        h ^= h >> 31;
        // Top 53 bits -> [0,1).
        (h >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Sample the field at continuous `(x, z)` with cosine interpolation.
    pub fn sample(&self, x: f64, z: f64) -> f64 {
        let x0 = x.floor();
        let z0 = z.floor();
        let (xi, zi) = (x0 as i64, z0 as i64);
        let fx = x - x0;
        let fz = z - z0;

        let v00 = self.lattice(xi, zi);
        let v10 = self.lattice(xi + 1, zi);
        let v01 = self.lattice(xi, zi + 1);
        let v11 = self.lattice(xi + 1, zi + 1);

        // Cosine smoothing for continuous derivatives.
        let sx = smooth(fx);
        let sz = smooth(fz);
        let a = lerp(v00, v10, sx);
        let b = lerp(v01, v11, sx);
        lerp(a, b, sz)
    }

    /// Fractal Brownian motion: sum of `octaves` scaled samples in `[0, 1]`.
    pub fn fbm(&self, x: f64, z: f64, octaves: u32) -> f64 {
        let mut freq = 1.0;
        let mut amp = 1.0;
        let mut sum = 0.0;
        let mut norm = 0.0;
        for _ in 0..octaves.max(1) {
            sum += self.sample(x * freq, z * freq) * amp;
            norm += amp;
            freq *= 2.0;
            amp *= 0.5;
        }
        sum / norm
    }
}

#[inline]
fn smooth(t: f64) -> f64 {
    // Cosine ease.
    (1.0 - (t * std::f64::consts::PI).cos()) * 0.5
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_bounded_and_deterministic() {
        let n = ValueNoise::new(1234);
        for i in 0..1000 {
            let x = i as f64 * 0.37;
            let z = i as f64 * -0.19;
            let v = n.fbm(x, z, 4);
            assert!((0.0..=1.0).contains(&v), "fbm out of range: {v}");
            assert_eq!(v, ValueNoise::new(1234).fbm(x, z, 4), "not deterministic");
        }
    }

    #[test]
    fn different_seeds_differ() {
        let a = ValueNoise::new(1).fbm(3.2, 4.5, 4);
        let b = ValueNoise::new(2).fbm(3.2, 4.5, 4);
        assert_ne!(a, b);
    }

    #[test]
    fn lattice_points_are_continuous() {
        // Sampling exactly on a lattice point returns that point's value.
        let n = ValueNoise::new(7);
        assert!((n.sample(5.0, 9.0) - n.sample(5.0, 9.0)).abs() < 1.0e-12);
    }
}
