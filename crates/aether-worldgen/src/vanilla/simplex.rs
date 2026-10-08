//! `SimplexNoise` and `PerlinSimplexNoise` — the 2D noises behind biome
//! temperature variation (snow lines, frozen-ocean patches).
//!
//! These are seeded from fixed legacy-LCG seeds in the game (`1234`, `2345`,
//! `3456`), not from the world seed, so they are the same in every world.

use super::rng::{Rng, WorldgenRandom};

const GRADIENT: [[i32; 3]; 16] = [
    [1, 1, 0],
    [-1, 1, 0],
    [1, -1, 0],
    [-1, -1, 0],
    [1, 0, 1],
    [-1, 0, 1],
    [1, 0, -1],
    [-1, 0, -1],
    [0, 1, 1],
    [0, -1, 1],
    [0, 1, -1],
    [0, -1, -1],
    [1, 1, 0],
    [0, -1, 1],
    [-1, 1, 0],
    [0, -1, -1],
];

/// `SimplexNoise`.
#[derive(Debug, Clone)]
pub struct SimplexNoise {
    p: [i32; 512],
    /// Random X offset.
    pub xo: f64,
    /// Random Y offset.
    pub yo: f64,
    /// Random Z offset.
    pub zo: f64,
}

impl SimplexNoise {
    /// Seeded from `r`, consuming draws exactly as the game does.
    pub fn new(r: &mut impl Rng) -> Self {
        let xo = r.next_double() * 256.0;
        let yo = r.next_double() * 256.0;
        let zo = r.next_double() * 256.0;
        let mut p = [0i32; 512];
        for (i, v) in p.iter_mut().enumerate().take(256) {
            *v = i as i32;
        }
        for i in 0..256usize {
            let j = r.next_int_bounded(256 - i as i32) as usize;
            p.swap(i, j + i);
        }
        Self { p, xo, yo, zo }
    }

    #[inline]
    fn perm(&self, i: i32) -> i32 {
        self.p[(i & 0xFF) as usize]
    }

    #[inline]
    fn corner(g: usize, x: f64, y: f64, z: f64, base: f64) -> f64 {
        let mut t = base - x * x - y * y - z * z;
        if t < 0.0 {
            0.0
        } else {
            t *= t;
            let gr = GRADIENT[g];
            t * t * (gr[0] as f64 * x + gr[1] as f64 * y + gr[2] as f64 * z)
        }
    }

    /// 2D `getValue(x, y)`.
    pub fn get_value_2d(&self, x: f64, y: f64) -> f64 {
        let sqrt3 = 3.0f64.sqrt();
        let f2 = 0.5 * (sqrt3 - 1.0);
        let g2 = (3.0 - sqrt3) / 6.0;
        let s = (x + y) * f2;
        let i = (x + s).floor() as i32;
        let j = (y + s).floor() as i32;
        let t = (i + j) as f64 * g2;
        let x0 = x - (i as f64 - t);
        let y0 = y - (j as f64 - t);
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - i1 as f64 + g2;
        let y1 = y0 - j1 as f64 + g2;
        let x2 = x0 - 1.0 + 2.0 * g2;
        let y2 = y0 - 1.0 + 2.0 * g2;
        let ii = i & 0xFF;
        let jj = j & 0xFF;
        let g0 = (self.perm(ii + self.perm(jj)) % 12) as usize;
        let g1 = (self.perm(ii + i1 + self.perm(jj + j1)) % 12) as usize;
        let gg2 = (self.perm(ii + 1 + self.perm(jj + 1)) % 12) as usize;
        let n0 = Self::corner(g0, x0, y0, 0.0, 0.5);
        let n1 = Self::corner(g1, x1, y1, 0.0, 0.5);
        let n2 = Self::corner(gg2, x2, y2, 0.0, 0.5);
        70.0 * (n0 + n1 + n2)
    }
}

/// `PerlinSimplexNoise`: octaves of [`SimplexNoise`].
#[derive(Debug, Clone)]
pub struct PerlinSimplexNoise {
    levels: Vec<Option<SimplexNoise>>,
    input_factor: f64,
    value_factor: f64,
}

impl PerlinSimplexNoise {
    /// Over `octaves` (any order; deduplicated and sorted as the game does).
    pub fn new(r: &mut impl Rng, octaves: &[i32]) -> Self {
        let mut set: Vec<i32> = octaves.to_vec();
        set.sort_unstable();
        set.dedup();
        let first = -set[0];
        let last = *set.last().unwrap();
        let total = first + last + 1;
        let contains = |v: i32| set.binary_search(&v).is_ok();
        let base = SimplexNoise::new(r);
        let mut levels: Vec<Option<SimplexNoise>> = vec![None; total as usize];
        let base_xyz = (base.xo, base.yo, base.zo);
        let base_copy = base.clone();
        if last >= 0 && last < total && contains(0) {
            levels[last as usize] = Some(base);
        }
        for k in (last + 1)..total {
            if k >= 0 && contains(last - k) {
                levels[k as usize] = Some(SimplexNoise::new(r));
            } else {
                r.consume(262);
            }
        }
        if last > 0 {
            let seed = (base_copy.get_value_3d_unused(base_xyz) * 9.223_372E18_f32 as f64) as i64;
            let mut r2 = WorldgenRandom::legacy(seed);
            for k in (0..last).rev() {
                if k < total && contains(last - k) {
                    levels[k as usize] = Some(SimplexNoise::new(&mut r2));
                } else {
                    r2.consume(262);
                }
            }
        }
        Self {
            levels,
            input_factor: 2f64.powi(last),
            value_factor: 1.0 / (2f64.powi(total) - 1.0),
        }
    }

    /// `getValue(x, y, useOffsets)`.
    pub fn get_value(&self, x: f64, y: f64, offsets: bool) -> f64 {
        let mut v = 0.0;
        let mut inf = self.input_factor;
        let mut vf = self.value_factor;
        for n in &self.levels {
            if let Some(n) = n {
                let ox = if offsets { n.xo } else { 0.0 };
                let oy = if offsets { n.yo } else { 0.0 };
                v += n.get_value_2d(x * inf + ox, y * inf + oy) * vf;
            }
            inf /= 2.0;
            vf *= 2.0;
        }
        v
    }
}

impl SimplexNoise {
    /// Only the overworld's positive-octave noises would need the 3D sample
    /// used to seed the second half of the stack; none of the biome noises
    /// have one. Kept so a stack with positive octaves still builds, but it
    /// panics rather than silently seeding wrongly.
    fn get_value_3d_unused(&self, _xyz: (f64, f64, f64)) -> f64 {
        unimplemented!("PerlinSimplexNoise with positive octaves is not used by the overworld")
    }
}

/// The three fixed-seed noises `Biome` reads.
#[derive(Debug, Clone)]
pub struct BiomeNoises {
    /// `TEMPERATURE_NOISE`: seed 1234, octave 0.
    pub temperature: PerlinSimplexNoise,
    /// `FROZEN_TEMPERATURE_NOISE`: seed 3456, octaves -2..0.
    pub frozen_temperature: PerlinSimplexNoise,
    /// `BIOME_INFO_NOISE`: seed 2345, octave 0.
    pub biome_info: PerlinSimplexNoise,
}

impl BiomeNoises {
    /// Build all three.
    pub fn new() -> Self {
        Self {
            temperature: PerlinSimplexNoise::new(&mut WorldgenRandom::legacy(1234), &[0]),
            frozen_temperature: PerlinSimplexNoise::new(
                &mut WorldgenRandom::legacy(3456),
                &[-2, -1, 0],
            ),
            biome_info: PerlinSimplexNoise::new(&mut WorldgenRandom::legacy(2345), &[0]),
        }
    }
}

impl Default for BiomeNoises {
    fn default() -> Self {
        Self::new()
    }
}
