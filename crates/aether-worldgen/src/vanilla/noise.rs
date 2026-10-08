//! Vanilla's noise primitives: improved (Perlin) noise, the octave stack built
//! on it, and the "normal" noise that the density-function graph actually
//! samples.
//!
//! These sit directly on [`super::random`]. Three things have to line up for a
//! sample to match vanilla, and all three are easy to get subtly wrong:
//!
//! * the permutation table and gradient set inside one octave,
//! * how many draws each octave takes from the random source, and in what
//!   order — a single extra `nextDouble` shifts every octave after it,
//! * the amplitude/frequency bookkeeping that folds the octaves together.
//!
//! # Provenance
//!
//! The gradient table, the octave factors and the `1.0181268882175227` input
//! factor are algorithm, not data, so they do not come out of the game's files.
//! They were checked the only way that means anything: the unit tests below
//! compare against values printed by the game's own `NormalNoise` and
//! `BlendedNoise`, driven from a copy of the server jar. See [`super`] for the
//! full set of measurements.

use super::random::XoroshiroRandom;

/// The 3D gradient set shared by vanilla's simplex and improved noise: the 12
/// edge-midpoints of a cube, padded to 16 with four repeats so the index can
/// be a cheap `hash & 15`.
const GRADIENT: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

/// Ken Perlin's improved fade curve, `6t⁵ − 15t⁴ + 10t³`.
#[inline]
fn smoothstep(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// One octave of improved noise: a shuffled permutation table plus a random
/// offset of the lattice.
#[derive(Debug, Clone)]
pub struct ImprovedNoise {
    /// Lattice offset, in `[0, 256)` per axis.
    xo: f64,
    yo: f64,
    zo: f64,
    p: [u8; 256],
}

impl ImprovedNoise {
    /// Draw one octave from `random`, consuming exactly what vanilla consumes:
    /// three doubles for the offset, then 256 bounded ints for the shuffle.
    pub fn new(random: &mut XoroshiroRandom) -> Self {
        let xo = random.next_f64() * 256.0;
        let yo = random.next_f64() * 256.0;
        let zo = random.next_f64() * 256.0;
        let mut p = [0u8; 256];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for i in 0..256usize {
            // A partial Fisher-Yates that swaps forward only: the bound
            // shrinks as `i` advances, so the draw count is fixed at 256.
            let j = random.next_i32_bounded(256 - i as i32) as usize;
            p.swap(i, i + j);
        }
        Self { xo, yo, zo, p }
    }

    /// The lattice offset on the Y axis — needed by the octave stack's
    /// "fixed Y" mode, which pins Y to the octave's own origin.
    #[inline]
    pub fn y_origin(&self) -> f64 {
        self.yo
    }

    #[inline]
    fn perm(&self, i: i32) -> i32 {
        self.p[(i & 255) as usize] as i32
    }

    /// Plain 3D noise.
    #[inline]
    pub fn noise(&self, x: f64, y: f64, z: f64) -> f64 {
        self.noise_with_smear(x, y, z, 0.0, 0.0)
    }

    /// 3D noise with vanilla's optional Y "smearing": when `y_scale` is
    /// non-zero the Y fraction used for the *lattice lookup* is snapped down
    /// to a multiple of `y_scale`, while the fraction used for the *fade
    /// curve* stays continuous. That mismatch is deliberate — it is what gives
    /// the old terrain noise its layered look.
    ///
    /// With the overworld's parameters the snap always works out to zero — the
    /// step is far larger than the fraction it is quantizing — so the whole
    /// mechanism is a no-op there. That is not a reading error: the game's own
    /// bound for it is named `maxBrokenValue`. The path is still exercised, by
    /// [`BlendedNoise`], which passes a non-zero `y_scale`.
    pub fn noise_with_smear(&self, x: f64, y: f64, z: f64, y_scale: f64, y_max: f64) -> f64 {
        let dx = x + self.xo;
        let dy = y + self.yo;
        let dz = z + self.zo;
        let ix = dx.floor();
        let iy = dy.floor();
        let iz = dz.floor();
        let fx = dx - ix;
        let fy = dy - iy;
        let fz = dz - iz;

        let shift = if y_scale != 0.0 {
            let cap = if y_max >= 0.0 && y_max < fy { y_max } else { fy };
            (cap / y_scale + 1.0E-7_f32 as f64).floor() * y_scale
        } else {
            0.0
        };

        self.sample_and_lerp(ix as i32, iy as i32, iz as i32, fx, fy - shift, fz, fy)
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(
        &self,
        gx: i32,
        gy: i32,
        gz: i32,
        dx: f64,
        dy: f64,
        dz: f64,
        fade_y: f64,
    ) -> f64 {
        let a = self.perm(gx);
        let b = self.perm(gx + 1);
        let aa = self.perm(a + gy);
        let ab = self.perm(a + gy + 1);
        let ba = self.perm(b + gy);
        let bb = self.perm(b + gy + 1);

        let g = |h: i32, x: f64, y: f64, z: f64| {
            let v = GRADIENT[(h & 15) as usize];
            v[0] * x + v[1] * y + v[2] * z
        };

        let v000 = g(self.perm(aa + gz), dx, dy, dz);
        let v100 = g(self.perm(ba + gz), dx - 1.0, dy, dz);
        let v010 = g(self.perm(ab + gz), dx, dy - 1.0, dz);
        let v110 = g(self.perm(bb + gz), dx - 1.0, dy - 1.0, dz);
        let v001 = g(self.perm(aa + gz + 1), dx, dy, dz - 1.0);
        let v101 = g(self.perm(ba + gz + 1), dx - 1.0, dy, dz - 1.0);
        let v011 = g(self.perm(ab + gz + 1), dx, dy - 1.0, dz - 1.0);
        let v111 = g(self.perm(bb + gz + 1), dx - 1.0, dy - 1.0, dz - 1.0);

        let u = smoothstep(dx);
        // The fade uses the *unsmeared* Y fraction; `dy` above may have been
        // snapped. Collapsing the two is the classic way to get this wrong.
        let v = smoothstep(fade_y);
        let w = smoothstep(dz);

        let x00 = lerp(u, v000, v100);
        let x10 = lerp(u, v010, v110);
        let x01 = lerp(u, v001, v101);
        let x11 = lerp(u, v011, v111);
        lerp(w, lerp(v, x00, x10), lerp(v, x01, x11))
    }
}

/// Vanilla's coordinate wrap, keeping lattice indices inside the range where
/// `f64 → i32` still has full precision.
#[inline]
pub fn wrap(v: f64) -> f64 {
    const P: f64 = 3.354_432_0E7;
    v - ((v / P + 0.5).floor()) * P
}

/// A stack of [`ImprovedNoise`] octaves with per-octave amplitudes — vanilla's
/// `PerlinNoise`.
#[derive(Debug, Clone)]
pub struct PerlinNoise {
    /// One entry per amplitude; `None` where the amplitude is zero, because
    /// vanilla skips creating (and seeding) those octaves entirely.
    levels: Vec<Option<ImprovedNoise>>,
    amplitudes: Vec<f64>,
    lowest_freq_input_factor: f64,
    lowest_freq_value_factor: f64,
    max_value: f64,
}

impl PerlinNoise {
    /// Build the stack the way the modern (non-legacy) path does: fork a
    /// positional factory off `random`, then seed each non-zero octave from
    /// the name `octave_<n>`.
    ///
    /// Naming each octave rather than drawing them in sequence is why a zero
    /// amplitude in the middle of the list does not shift the octaves after
    /// it.
    pub fn create(random: &mut XoroshiroRandom, first_octave: i32, amplitudes: &[f64]) -> Self {
        let factory = random.fork_positional();
        let mut levels = Vec::with_capacity(amplitudes.len());
        for (k, &amp) in amplitudes.iter().enumerate() {
            if amp != 0.0 {
                let octave = first_octave + k as i32;
                let mut r = factory.from_hash_of(&format!("octave_{octave}"));
                levels.push(Some(ImprovedNoise::new(&mut r)));
            } else {
                levels.push(None);
            }
        }
        let n = amplitudes.len() as i32;
        let lowest_freq_input_factor = 2f64.powi(first_octave);
        let lowest_freq_value_factor = 2f64.powi(n - 1) / (2f64.powi(n) - 1.0);
        let mut me = Self {
            levels,
            amplitudes: amplitudes.to_vec(),
            lowest_freq_input_factor,
            lowest_freq_value_factor,
            max_value: 0.0,
        };
        me.max_value = me.edge_value(2.0);
        me
    }

    /// Vanilla's `edgeValue`: the stack's output when every octave saturates.
    fn edge_value(&self, x: f64) -> f64 {
        let mut d = 0.0;
        let mut f = self.lowest_freq_value_factor;
        for (i, level) in self.levels.iter().enumerate() {
            if level.is_some() {
                d += self.amplitudes[i] * x * f;
            }
            f /= 2.0;
        }
        d
    }

    /// The largest magnitude this stack can produce.
    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    /// Sample the stack.
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        self.get_value_full(x, y, z, 0.0, 0.0, false)
    }

    /// Sample with Y smearing and the optional "fixed Y" mode, in which each
    /// octave is sampled at its own lattice origin instead of at `y`.
    pub fn get_value_full(
        &self,
        x: f64,
        y: f64,
        z: f64,
        y_scale: f64,
        y_max: f64,
        fixed_y: bool,
    ) -> f64 {
        let mut total = 0.0;
        let mut input = self.lowest_freq_input_factor;
        let mut value = self.lowest_freq_value_factor;
        for (i, level) in self.levels.iter().enumerate() {
            if let Some(n) = level {
                let yy = if fixed_y { -n.y_origin() } else { wrap(y * input) };
                let g = n.noise_with_smear(
                    wrap(x * input),
                    yy,
                    wrap(z * input),
                    y_scale * input,
                    y_max * input,
                );
                total += self.amplitudes[i] * g * value;
            }
            input *= 2.0;
            value /= 2.0;
        }
        total
    }
}

/// Two [`PerlinNoise`] stacks summed at slightly offset frequencies and
/// rescaled — vanilla's `NormalNoise`, and the only noise the density-function
/// graph ever asks for by name.
///
/// The second stack reads the *same* coordinates scaled by an irrational-ish
/// factor, which breaks up the lattice alignment the single stack would show.
#[derive(Debug, Clone)]
pub struct NormalNoise {
    first: PerlinNoise,
    second: PerlinNoise,
    value_factor: f64,
    max_value: f64,
}

/// The frequency offset between the two stacks.
const INPUT_FACTOR: f64 = 1.018_126_888_217_522_7;

impl NormalNoise {
    /// Build from a `NoiseParameters` pair, consuming `random` exactly as
    /// vanilla does: the two stacks are created back to back from the same
    /// source, so their streams are adjacent and not independent.
    pub fn create(random: &mut XoroshiroRandom, first_octave: i32, amplitudes: &[f64]) -> Self {
        let first = PerlinNoise::create(random, first_octave, amplitudes);
        let second = PerlinNoise::create(random, first_octave, amplitudes);

        // The spread of non-zero amplitudes — not their count — sets the
        // expected deviation, so a stack with holes in it still normalizes to
        // roughly unit range.
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        let mut any = false;
        for (k, &a) in amplitudes.iter().enumerate() {
            if a != 0.0 {
                lo = lo.min(k);
                hi = hi.max(k);
                any = true;
            }
        }
        let span = if any { hi as i32 - lo as i32 } else { 0 };
        let value_factor = 0.166_666_666_666_666_66 / expected_deviation(span);
        let max_value = (first.max_value() + second.max_value()) * value_factor;
        Self {
            first,
            second,
            value_factor,
            max_value,
        }
    }

    /// The largest magnitude this noise can produce.
    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    /// Sample.
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        let x2 = x * INPUT_FACTOR;
        let y2 = y * INPUT_FACTOR;
        let z2 = z * INPUT_FACTOR;
        (self.first.get_value(x, y, z) + self.second.get_value(x2, y2, z2)) * self.value_factor
    }
}

/// Vanilla's `BlendedNoise` — the 1.12-era terrain noise, still the backbone of
/// the modern overworld's 3D shape under the name `minecraft:old_blended_noise`.
///
/// Three octave stacks: a `min` limit, a `max` limit, and a `main` stack that
/// picks a blend between them. The stacks are built by the *legacy* path, which
/// draws its octaves in sequence from one random source rather than naming each
/// one — so their streams are adjacent, and the draw order below is load
/// bearing.
///
/// Verified against the game's own `BlendedNoise` class; see the unit test.
#[derive(Debug, Clone)]
pub struct BlendedNoise {
    min_limit: Vec<ImprovedNoise>,
    max_limit: Vec<ImprovedNoise>,
    main: Vec<ImprovedNoise>,
    xz_multiplier: f64,
    y_multiplier: f64,
    xz_factor: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
}

/// The frequency the old terrain noise is sampled at, before the settings'
/// own scales are applied.
const BLENDED_BASE_FREQUENCY: f64 = 684.412;

/// Draw `count` octaves in sequence, the way the legacy stack does.
///
/// The legacy constructor fills its array from the highest index downwards, and
/// `getOctaveNoise(i)` reads it back from the end — so octave `i` is simply the
/// `i`-th octave drawn. Keeping them in draw order collapses both reversals.
fn legacy_octaves(random: &mut XoroshiroRandom, count: usize) -> Vec<ImprovedNoise> {
    (0..count).map(|_| ImprovedNoise::new(random)).collect()
}

impl BlendedNoise {
    /// Build the three stacks from `random`, in vanilla's order: min limit
    /// (16 octaves), max limit (16), then main (8).
    pub fn new(
        random: &mut XoroshiroRandom,
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    ) -> Self {
        let min_limit = legacy_octaves(random, 16);
        let max_limit = legacy_octaves(random, 16);
        let main = legacy_octaves(random, 8);
        Self {
            min_limit,
            max_limit,
            main,
            xz_multiplier: BLENDED_BASE_FREQUENCY * xz_scale,
            y_multiplier: BLENDED_BASE_FREQUENCY * y_scale,
            xz_factor,
            y_factor,
            smear_scale_multiplier,
        }
    }

    /// Sample at a block position.
    pub fn compute(&self, x: i32, y: i32, z: i32) -> f64 {
        let d = x as f64 * self.xz_multiplier;
        let e = y as f64 * self.y_multiplier;
        let f = z as f64 * self.xz_multiplier;
        let g = d / self.xz_factor;
        let h = e / self.y_factor;
        let i = f / self.xz_factor;
        let j = self.y_multiplier * self.smear_scale_multiplier;
        let k = j / self.y_factor;

        // The main stack decides how far to lean toward each limit.
        let mut main = 0.0;
        let mut o = 1.0;
        for n in &self.main {
            main += n.noise_with_smear(wrap(g * o), wrap(h * o), wrap(i * o), k * o, h * o) / o;
            o /= 2.0;
        }
        let q = (main / 10.0 + 1.0) / 2.0;
        // Saturated blends skip the stack they cannot influence — an
        // optimization in vanilla, but it also means those octaves are never
        // evaluated, so reproducing it keeps the arithmetic identical.
        let all_max = q >= 1.0;
        let all_min = q <= 0.0;

        let mut lo = 0.0;
        let mut hi = 0.0;
        let mut o = 1.0;
        for r in 0..16usize {
            let s = wrap(d * o);
            let t = wrap(e * o);
            let u = wrap(f * o);
            let v = j * o;
            if !all_max {
                lo += self.min_limit[r].noise_with_smear(s, t, u, v, e * o) / o;
            }
            if !all_min {
                hi += self.max_limit[r].noise_with_smear(s, t, u, v, e * o) / o;
            }
            o /= 2.0;
        }
        clamped_lerp(q, lo / 512.0, hi / 512.0) / 128.0
    }
}

/// `Mth.clampedLerp(delta, start, end)`: the delta comes first, and out-of-range
/// deltas clamp to an endpoint rather than extrapolating.
#[inline]
fn clamped_lerp(t: f64, a: f64, b: f64) -> f64 {
    if t < 0.0 {
        a
    } else if t > 1.0 {
        b
    } else {
        a + t * (b - a)
    }
}

fn expected_deviation(octaves: i32) -> f64 {
    0.1 * (1.0 + 1.0 / (octaves + 1) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_permutation_table_is_a_permutation() {
        // A shuffle bug that duplicates entries would still "look like noise",
        // so check the invariant directly.
        let mut r = XoroshiroRandom::new(42);
        let n = ImprovedNoise::new(&mut r);
        let mut seen = [false; 256];
        for &v in &n.p {
            assert!(!seen[v as usize], "value {v} appears twice");
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn one_octave_is_zero_on_the_lattice() {
        // Perlin noise is exactly zero at integer lattice points: every
        // gradient dot product there has a zero displacement vector. This
        // pins the fade/lerp wiring without needing a reference value.
        let mut r = XoroshiroRandom::new(7);
        let n = ImprovedNoise::new(&mut r);
        for i in -4..4 {
            for j in -4..4 {
                // Undo the random lattice offset so we land on integers.
                let v = n.noise(i as f64 - n.xo, j as f64 - n.yo, -n.zo);
                assert!(v.abs() < 1e-12, "lattice value {v} at ({i},{j})");
            }
        }
    }

    #[test]
    fn octaves_with_zero_amplitude_are_not_seeded() {
        // The property that lets vanilla add a hole to an amplitude list
        // without disturbing its neighbours.
        let mut a = XoroshiroRandom::new(1);
        let p = PerlinNoise::create(&mut a, -7, &[1.0, 0.0, 1.0]);
        assert!(p.levels[1].is_none());
        // The surviving octaves must be identical to the ones a hole-free
        // build of the same octave numbers produces.
        let mut b = XoroshiroRandom::new(1);
        let q = PerlinNoise::create(&mut b, -7, &[1.0, 1.0, 1.0]);
        assert_eq!(
            p.levels[2].as_ref().unwrap().p,
            q.levels[2].as_ref().unwrap().p
        );
    }

    #[test]
    fn normal_noise_stays_in_a_sane_band() {
        let mut r = XoroshiroRandom::new(42);
        let n = NormalNoise::create(&mut r, -9, &[1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0]);
        let mut max = 0.0f64;
        for i in 0..2000 {
            let v = n.get_value(i as f64 * 3.7, 0.0, i as f64 * -1.3);
            max = max.max(v.abs());
        }
        assert!(max > 0.05, "noise is suspiciously flat: {max}");
        assert!(
            max <= n.max_value(),
            "sample {max} exceeded the declared max {}",
            n.max_value()
        );
    }

    #[test]
    fn wrap_is_identity_near_the_origin() {
        for v in [0.0, 1.0, -1.0, 1e6, -1e6] {
            assert_eq!(wrap(v), v);
        }
        // ...and folds far-away coordinates back.
        assert!(wrap(1e9).abs() <= 3.354_432_0E7 / 2.0);
    }

    // --- reference values -------------------------------------------------
    //
    // The constants below are not this crate's own output frozen in place —
    // that would only prove the code still does what it did yesterday. They
    // were printed by the game's own `NormalNoise` and `BlendedNoise` classes,
    // driven from a copy of the server jar:
    //
    // ```java
    // PositionalRandomFactory f = new XoroshiroRandomSource(42L).forkPositional();
    // NormalNoise n = NormalNoise.create(f.fromHashOf("minecraft:continentalness"),
    //     new NormalNoise.NoiseParameters(-9, DoubleArrayList.of(1,1,2,2,2,1,1,1,1)));
    // System.out.printf("%.17e%n", n.getValue(x, y, z));
    // ```
    //
    // Seventeen significant digits round-trip a `double` exactly, so agreement
    // here is bit-for-bit and not merely close.

    /// The parameters of `minecraft:continentalness`, as the data file gives
    /// them. Small enough to inline; the generator proper reads the file.
    const CONTINENTALNESS: (i32, [f64; 9]) = (-9, [1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0]);
    /// The parameters of `minecraft:offset`.
    const OFFSET: (i32, [f64; 4]) = (-3, [1.0, 1.0, 1.0, 0.0]);

    #[test]
    fn normal_noise_matches_the_game() {
        let mut root = XoroshiroRandom::new(42);
        let f = root.fork_positional();

        let mut r = f.from_hash_of("minecraft:continentalness");
        let n = NormalNoise::create(&mut r, CONTINENTALNESS.0, &CONTINENTALNESS.1);
        let want = [
            -7.619_239_037_913_490_0e-2,
            -7.183_318_007_947_485_0e-2,
            -8.532_247_361_321_689_0e-2,
            -1.439_651_199_269_539_3e-1,
            -1.948_269_626_389_930_0e-1,
            -2.065_326_575_535_241_5e-1,
        ];
        for (i, w) in want.iter().enumerate() {
            let got = n.get_value(i as f64 * 13.0, i as f64 * 2.0, i as f64 * -7.0);
            assert_eq!(got, *w, "continentalness sample {i}");
        }

        // Built from the same factory, so this also pins that a named stream
        // depends only on its name and not on what was built before it.
        let mut r = f.from_hash_of("minecraft:offset");
        let o = NormalNoise::create(&mut r, OFFSET.0, &OFFSET.1);
        let want = [
            3.544_825_675_662_071_6e-1,
            -2.541_342_852_542_133_0e-1,
            4.302_605_840_197_844_0e-1,
            3.810_698_105_517_788_0e-1,
            2.199_666_319_018_652_0e-1,
            -3.899_510_323_708_621_5e-1,
        ];
        for (i, w) in want.iter().enumerate() {
            let got = o.get_value(i as f64 * 3.0, 0.0, i as f64 * 5.0);
            assert_eq!(got, *w, "offset sample {i}");
        }
    }

    #[test]
    fn blended_noise_matches_the_game() {
        // The overworld's `old_blended_noise` parameters, from
        // `density_function/overworld/base_3d_noise.json`.
        let mut root = XoroshiroRandom::new(42);
        let f = root.fork_positional();
        let mut r = f.from_hash_of("minecraft:terrain");
        let b = BlendedNoise::new(&mut r, 0.25, 0.125, 80.0, 160.0, 8.0);
        let want = [
            2.675_943_642_204_994_0e-1,
            -2.164_166_642_196_431_2e-1,
            -1.754_151_748_530_703_4e-1,
            -9.572_196_896_687_926_0e-2,
            3.265_703_603_721_411_6e-1,
            2.313_775_187_501_578_6e-1,
        ];
        for (i, w) in want.iter().enumerate() {
            let i = i as i32;
            let got = b.compute(i * 7, 40 - i * 11, i * -5);
            assert_eq!(got, *w, "blended sample {i}");
        }
    }
}
