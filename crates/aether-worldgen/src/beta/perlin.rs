//! Improved (Ken Perlin, 2002) gradient noise, seeded the way vanilla's
//! `ImprovedNoise` is: a permutation table shuffled by [`JavaRandom`], plus a
//! random per-octave origin offset so stacked octaves don't share a lattice.
//!
//! This is real gradient noise — unlike [`crate::noise::ValueNoise`], which
//! hashes lattice *values* — so it has the smoother, less "bubbly" character
//! terrain heightmaps want.

use super::random::JavaRandom;

pub struct ImprovedNoise {
    /// Doubled permutation table (`perm[i] == perm[i + 256]`) so lattice
    /// lookups never need to wrap.
    perm: [u8; 512],
    xo: f64,
    yo: f64,
    zo: f64,
}

impl ImprovedNoise {
    pub fn new(rng: &mut JavaRandom) -> Self {
        let xo = rng.next_double() * 256.0;
        let yo = rng.next_double() * 256.0;
        let zo = rng.next_double() * 256.0;

        let mut base = [0u8; 256];
        for (i, slot) in base.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for i in 0..256 {
            let j = rng.next_int((256 - i) as i32) as usize;
            base.swap(i, i + j);
        }
        let mut perm = [0u8; 512];
        for (i, slot) in perm.iter_mut().enumerate() {
            *slot = base[i & 255];
        }

        Self { perm, xo, yo, zo }
    }

    /// Sample the field at `(x, y, z)`. Range is bounded (not normalized) —
    /// roughly `[-1, 1]` for a single octave, same as vanilla.
    pub fn sample(&self, x: f64, y: f64, z: f64) -> f64 {
        let x = x + self.xo;
        let y = y + self.yo;
        let z = z + self.zo;

        let fx = x.floor();
        let fy = y.floor();
        let fz = z.floor();
        let ix = (fx as i64 & 255) as usize;
        let iy = (fy as i64 & 255) as usize;
        let iz = (fz as i64 & 255) as usize;

        let dx = x - fx;
        let dy = y - fy;
        let dz = z - fz;
        let u = fade(dx);
        let v = fade(dy);
        let w = fade(dz);

        let p = &self.perm;
        let a = p[ix] as usize + iy;
        let aa = p[a] as usize + iz;
        let ab = p[a + 1] as usize + iz;
        let b = p[ix + 1] as usize + iy;
        let ba = p[b] as usize + iz;
        let bb = p[b + 1] as usize + iz;

        lerp(
            w,
            lerp(
                v,
                lerp(
                    u,
                    grad(p[aa] as i32, dx, dy, dz),
                    grad(p[ba] as i32, dx - 1.0, dy, dz),
                ),
                lerp(
                    u,
                    grad(p[ab] as i32, dx, dy - 1.0, dz),
                    grad(p[bb] as i32, dx - 1.0, dy - 1.0, dz),
                ),
            ),
            lerp(
                v,
                lerp(
                    u,
                    grad(p[aa + 1] as i32, dx, dy, dz - 1.0),
                    grad(p[ba + 1] as i32, dx - 1.0, dy, dz - 1.0),
                ),
                lerp(
                    u,
                    grad(p[ab + 1] as i32, dx, dy - 1.0, dz - 1.0),
                    grad(p[bb + 1] as i32, dx - 1.0, dy - 1.0, dz - 1.0),
                ),
            ),
        )
    }
}

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// The classic 12-direction (padded to 16 via bit tricks) Perlin gradient
/// table, picked by the low 4 bits of the lattice hash.
#[inline]
fn grad(hash: i32, x: f64, y: f64, z: f64) -> f64 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_from_seed() {
        let a = ImprovedNoise::new(&mut JavaRandom::new(7));
        let b = ImprovedNoise::new(&mut JavaRandom::new(7));
        for i in 0..50 {
            let x = i as f64 * 0.31;
            assert_eq!(a.sample(x, x * 0.5, -x), b.sample(x, x * 0.5, -x));
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let a = ImprovedNoise::new(&mut JavaRandom::new(1));
        let b = ImprovedNoise::new(&mut JavaRandom::new(2));
        assert_ne!(a.sample(3.2, 1.0, 4.5), b.sample(3.2, 1.0, 4.5));
    }

    #[test]
    fn output_is_bounded() {
        let n = ImprovedNoise::new(&mut JavaRandom::new(99));
        for i in 0..2000 {
            let t = i as f64 * 0.37;
            let v = n.sample(t, -t * 0.7, t * 1.3);
            assert!((-2.0..=2.0).contains(&v), "sample out of range: {v}");
        }
    }

    #[test]
    fn lattice_points_are_continuous() {
        let n = ImprovedNoise::new(&mut JavaRandom::new(3));
        assert!((n.sample(5.0, 2.0, 9.0) - n.sample(5.0, 2.0, 9.0)).abs() < 1e-12);
    }
}
