//! The Beta/1.8-era terrain shape: a 3D density field, not a 2D heightmap —
//! this is what buys real overhangs and caves instead of one height sample
//! per column.
//!
//! Structurally this mirrors vanilla's classic `ChunkProviderGenerate`
//! (unchanged in spirit from Beta all the way through 1.17, later reframed as
//! the `minecraft:old_blended_noise` density function): two independent
//! octave noise fields (`min_limit` / `max_limit`) are blended by a third
//! ("selector") field, then reshaped by a per-column vertical falloff whose
//! base height and amplitude come from two more 2D noise fields (`depth`,
//! `scale`). Where that falloff-adjusted density is positive, the block is
//! solid.
//!
//! This is **not** a bit-exact port. Vanilla's real formula folds in a
//! biome-driven climate factor we have no biome system to supply, quantizes
//! the y-coordinate per octave ("smeared" noise) for a subtler blend, and
//! draws a few more octave stacks (beach, surface, forest) purely to keep a
//! shared `java.util.Random` stream aligned with decoration steps we don't
//! have either. What's preserved is the actual shape of the algorithm — a
//! real gradient-noise 3D density field, blended and vertically shaped by
//! per-column control noise — which is what actually produces
//! Beta-recognizable terrain (hills, cliffs, overhangs, caves) instead of a
//! smooth heightmap.

use super::octaves::OctaveNoise;
use super::random::JavaRandom;

/// Vanilla's Beta-era build height. Kept even though the engine's chunk
/// format supports 256, so the terrain silhouette (hill scale, cave depth)
/// matches the original rather than being stretched over double the height.
pub const WORLD_HEIGHT: i32 = 128;

/// Horizontal wavelength of the main density field, in blocks.
const XZ_SCALE: f64 = 1.0 / 100.0;
/// Vertical wavelength of the main density field, in blocks.
const Y_SCALE: f64 = 1.0 / 40.0;
/// Wavelength of the 2D control fields (`scale`, `depth`), in blocks —
/// deliberately much larger than the terrain noise so hilly/flat and
/// high/low regions form at a "biome" scale rather than block to block.
const CONTROL_SCALE: f64 = 1.0 / 400.0;
/// How strongly the blended min/max density field competes with the
/// vertical falloff gradient. The two 16-octave fbm stacks settle to a
/// fairly small typical magnitude (their per-octave amplitude is divided by
/// frequency each step); without this the falloff gradient — which spans a
/// fixed range over the world's full height — completely dominates and the
/// "density field" degenerates back into a smooth heightmap; no caves, no
/// overhangs, no per-column variation worth the name.
const BASE_AMPLITUDE: f64 = 6.0;

/// Per-column control values that shape the vertical falloff.
#[derive(Debug, Clone, Copy)]
pub struct ColumnControl {
    /// Shifts the effective sea/base height for this column — negative
    /// pulls it down (deeper terrain), positive pushes it up.
    pub depth: f64,
    /// Amplifies (>1) or dampens (<1) how sharply density falls off with
    /// height — higher means taller mountains and deeper valleys.
    pub scale: f64,
}

pub struct BetaTerrain {
    min_limit: OctaveNoise,
    max_limit: OctaveNoise,
    selector: OctaveNoise,
    scale: OctaveNoise,
    depth: OctaveNoise,
}

impl BetaTerrain {
    /// Seed every octave stack from one world seed, each stack drawing from
    /// the shared stream in a fixed order so the same seed always reproduces
    /// the same world (mirrors vanilla's `min_limit, max_limit, selector,
    /// scale, depth` draw order, dropping the beach/surface/forest octaves
    /// vanilla draws purely to position decoration steps we don't have).
    pub fn new(seed: u64) -> Self {
        let mut rng = JavaRandom::new(seed);
        Self {
            min_limit: OctaveNoise::new(&mut rng, 16),
            max_limit: OctaveNoise::new(&mut rng, 16),
            selector: OctaveNoise::new(&mut rng, 8),
            scale: OctaveNoise::new(&mut rng, 10),
            depth: OctaveNoise::new(&mut rng, 16),
        }
    }

    /// The per-column base-height/amplitude control values at world `(wx,
    /// wz)`. Cheap enough to call once per grid column (see
    /// `crate::NoiseGenerator`), not once per block.
    pub fn column_control(&self, wx: i32, wz: i32) -> ColumnControl {
        let x = wx as f64 * CONTROL_SCALE;
        let z = wz as f64 * CONTROL_SCALE;

        let raw_scale = self.scale.sample2d(x, z);
        let scale = 0.5 + (raw_scale.clamp(-1.0, 1.0) * 0.5 + 0.5);

        // The classic "fold" that turns a smooth noise field into Beta's
        // distinctive mix of gentle plains and sharper cliffs: the negative
        // half is compressed much harder than the positive half.
        let mut depth = self.depth.sample2d(x, z);
        if depth < 0.0 {
            depth = depth.abs() * 0.3;
        }
        depth = depth * 3.0 - 2.0;
        depth = if depth < 0.0 {
            (depth / 2.0).max(-1.0) / 1.4 / 2.0
        } else {
            depth.min(1.0) / 8.0
        };

        ColumnControl { depth, scale }
    }

    /// Density at world `(wx, wy, wz)` for a column whose control values are
    /// `control` (from [`Self::column_control`]) — positive is solid.
    pub fn density(&self, wx: i32, wy: i32, wz: i32, control: ColumnControl) -> f64 {
        let x = wx as f64 * XZ_SCALE;
        let y = wy as f64 * Y_SCALE;
        let z = wz as f64 * XZ_SCALE;

        let low = self.min_limit.sample(x, y, z);
        let high = self.max_limit.sample(x, y, z);
        let alpha = (self.selector.sample(x, y * 2.0, z) / 10.0 + 0.5).clamp(0.0, 1.0);
        let base = (low + (high - low) * alpha) * BASE_AMPLITUDE;

        // Vertical falloff: density drops off above the column's effective
        // base height (`0.5 + depth`, as a fraction of world height) and
        // the rate of drop-off is stretched/compressed by `scale`.
        let height_frac = wy as f64 / WORLD_HEIGHT as f64;
        let gradient = (height_frac - 0.5 - control.depth) * 6.0 / control.scale;
        base - gradient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_from_seed() {
        let a = BetaTerrain::new(42);
        let b = BetaTerrain::new(42);
        let ca = a.column_control(37, -12);
        let cb = b.column_control(37, -12);
        assert_eq!(a.density(37, 60, -12, ca), b.density(37, 60, -12, cb));
    }

    #[test]
    fn different_seeds_diverge() {
        let a = BetaTerrain::new(1);
        let b = BetaTerrain::new(2);
        let ca = a.column_control(0, 0);
        let cb = b.column_control(0, 0);
        assert_ne!(a.density(0, 60, 0, ca), b.density(0, 60, 0, cb));
    }

    #[test]
    fn density_falls_off_with_height() {
        // Deep underground should be solid far more often than the sky.
        let t = BetaTerrain::new(7);
        let mut deep_solid = 0;
        let mut sky_solid = 0;
        for wx in (-64..64).step_by(8) {
            for wz in (-64..64).step_by(8) {
                let c = t.column_control(wx, wz);
                if t.density(wx, 8, wz, c) > 0.0 {
                    deep_solid += 1;
                }
                if t.density(wx, 120, wz, c) > 0.0 {
                    sky_solid += 1;
                }
            }
        }
        assert!(
            deep_solid > sky_solid,
            "deep {deep_solid} vs sky {sky_solid}"
        );
    }
}
