//! A stack of [`ImprovedNoise`] octaves drawn from one seed stream, summed
//! with doubling frequency and halving amplitude — vanilla's
//! `NoiseGeneratorOctaves`, minus the coarse-grid-fill API we don't need
//! since we interpolate at a higher level (see `beta::terrain`).

use super::perlin::ImprovedNoise;
use super::random::JavaRandom;

pub struct OctaveNoise {
    layers: Vec<ImprovedNoise>,
}

impl OctaveNoise {
    /// Draw `count` octaves in sequence from `rng` — order matters, since
    /// every octave consumes part of the same stream (this is what lets two
    /// noise fields built from the same seed but at different stream
    /// positions stay statistically independent).
    pub fn new(rng: &mut JavaRandom, count: usize) -> Self {
        let layers = (0..count).map(|_| ImprovedNoise::new(rng)).collect();
        Self { layers }
    }

    /// 3D fbm sample. Not normalized — matches a single octave's own range,
    /// scaled by the geometric falloff across octaves.
    pub fn sample(&self, x: f64, y: f64, z: f64) -> f64 {
        let mut sum = 0.0;
        let mut freq = 1.0;
        for layer in &self.layers {
            sum += layer.sample(x * freq, y * freq, z * freq) / freq;
            freq *= 2.0;
        }
        sum
    }

    /// 2D fbm sample (`y` pinned to `0`) — used for the per-column control
    /// fields (`scale`, `depth`).
    pub fn sample2d(&self, x: f64, z: f64) -> f64 {
        self.sample(x, 0.0, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_seed_sensitive() {
        let a = OctaveNoise::new(&mut JavaRandom::new(11), 4);
        let b = OctaveNoise::new(&mut JavaRandom::new(11), 4);
        let c = OctaveNoise::new(&mut JavaRandom::new(12), 4);
        assert_eq!(a.sample(1.0, 2.0, 3.0), b.sample(1.0, 2.0, 3.0));
        assert_ne!(a.sample(1.0, 2.0, 3.0), c.sample(1.0, 2.0, 3.0));
    }
}
