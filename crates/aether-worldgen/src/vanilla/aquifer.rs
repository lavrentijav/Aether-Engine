//! The aquifer: what fills a gap in the terrain.
//!
//! Cell interpolation says where the world is solid. Everywhere it is *not*,
//! something has to choose between air, water and lava — and below the surface
//! that choice is not "water up to sea level". Vanilla scatters aquifer centres
//! on a coarse 16×12×16 grid, gives each one its own fluid and its own water
//! line, and then blends between the nearest few.
//!
//! Two consequences are easy to miss and both are load-bearing:
//!
//! * **An aquifer can place stone.** Where two neighbouring aquifers sit at
//!   different levels, the blend adds a *pressure* term to the density, and
//!   where that pushes the density above zero the block becomes solid. Those
//!   are the barriers that stop one cave lake draining into another. So the
//!   aquifer is not merely a fluid picker: it moves the solid/not-solid
//!   boundary too.
//! * **One value is per-chunk, not per-position.** `skip_sampling_above_y` is
//!   derived from the highest preliminary surface anywhere in the chunk, and
//!   above it the aquifer is bypassed entirely. Two positions with identical
//!   surroundings can therefore disagree if they sit in different chunks —
//!   which is why [`ChunkAquifer`] is built per chunk and not per column.
//!
//! Written from the game's `Aquifer.NoiseBasedAquifer`; the constants and the
//! order of the tests are transcribed from it, and the whole thing is measured
//! against the game's own output — see [`super::terrain`].

use std::cell::RefCell;
use std::sync::Arc;

use super::density::{Ctx, Node};
use super::random::PositionalFactory;

/// What the noise stage puts at one block position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    /// The dimension's default block; stone in the overworld.
    Solid,
    /// The dimension's default fluid; water in the overworld.
    Water,
    /// Lava.
    Lava,
    /// Nothing.
    Air,
}

/// A water line and what is below it.
///
/// Compared by value, exactly as vanilla's record is: two aquifers with the
/// same level *and* the same fluid are the same aquifer as far as the
/// blending is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FluidStatus {
    level: i32,
    kind: Fill,
}

impl FluidStatus {
    /// What this aquifer puts at height `y`: its fluid below the line, air at
    /// or above it.
    #[inline]
    fn at(&self, y: i32) -> Fill {
        if y < self.level {
            self.kind
        } else {
            Fill::Air
        }
    }
}

// --- the grid --------------------------------------------------------------
//
// Aquifer centres live on a grid 16 blocks apart horizontally and 12 apart
// vertically, each jittered inside its cell.

const Y_SPACING: i32 = 12;

#[inline]
fn grid_x(x: i32) -> i32 {
    x >> 4
}
#[inline]
fn from_grid_x(gx: i32, offset: i32) -> i32 {
    (gx << 4) + offset
}
/// Z uses the same 16-block spacing as X; kept separate because the two are
/// separate functions in the game and a future dimension could change one.
#[inline]
fn grid_z(z: i32) -> i32 {
    grid_x(z)
}

#[inline]
fn from_grid_z(gz: i32, offset: i32) -> i32 {
    from_grid_x(gz, offset)
}

#[inline]
fn grid_y(y: i32) -> i32 {
    y.div_euclid(Y_SPACING)
}
#[inline]
fn from_grid_y(gy: i32, offset: i32) -> i32 {
    gy * Y_SPACING + offset
}

/// `Aquifer.similarity`: 1 at equal distances, falling to 0 as they diverge by
/// 25. Negative means "not close enough to blend at all".
///
/// Note there is no `abs` here — it relies on the caller passing the nearer
/// distance first, which the four-nearest search guarantees.
#[inline]
fn similarity(nearer: i32, farther: i32) -> f64 {
    1.0 - (farther - nearer) as f64 / 25.0
}

/// Vanilla's threshold for treating two aquifers as one; `similarity(10², 12²)`,
/// which works out negative.
///
/// It decides only whether a placed fluid gets a tick scheduled, never which
/// block goes there, so nothing here reads it — it is kept so the omission is
/// visible rather than silent.
#[allow(dead_code)]
const FLOWING_UPDATE_SIMILARITY: f64 = 1.0 - (144 - 100) as f64 / 25.0;

/// The columns whose preliminary surface an aquifer consults, in chunk
/// offsets. The first entry is the aquifer's own column and is treated
/// specially, so the order matters.
const SURFACE_SAMPLING_OFFSETS_IN_CHUNKS: [[i32; 2]; 13] = [
    [0, 0],
    [-2, -1],
    [-1, -1],
    [0, -1],
    [1, -1],
    [-3, 0],
    [-2, 0],
    [-1, 0],
    [1, 0],
    [-2, 1],
    [-1, 1],
    [0, 1],
    [1, 1],
];

/// Vanilla's `DimensionType.WAY_BELOW_MIN_Y`: a water line far enough below
/// the world that nothing is ever under it.
const WAY_BELOW_MIN_Y: i32 = -32_512;

/// The density functions and constants an aquifer reads. Shared by every
/// chunk of one world.
#[derive(Clone)]
pub struct AquiferConfig {
    /// `barrier` from the noise router.
    pub barrier: Arc<Node>,
    /// `fluid_level_floodedness` from the noise router.
    pub floodedness: Arc<Node>,
    /// `fluid_level_spread` from the noise router.
    pub spread: Arc<Node>,
    /// `lava` from the noise router.
    pub lava: Arc<Node>,
    /// `erosion` from the noise router.
    pub erosion: Arc<Node>,
    /// `depth` from the noise router.
    pub depth: Arc<Node>,
    /// `preliminary_surface_level` from the noise router.
    pub preliminary_surface_level: Arc<Node>,
    /// Per-position random streams, forked from `minecraft:aquifer`.
    pub random: PositionalFactory,
    /// The dimension's sea level.
    pub sea_level: i32,
}

impl AquiferConfig {
    /// The fluid that would be there with no aquifer at all: lava in the very
    /// deep, the dimension's fluid up to sea level.
    fn global_fluid(&self, y: i32) -> FluidStatus {
        if y < (-54).min(self.sea_level) {
            FluidStatus {
                level: -54,
                kind: Fill::Lava,
            }
        } else {
            FluidStatus {
                level: self.sea_level,
                kind: Fill::Water,
            }
        }
    }

    /// `preliminarySurfaceLevel`, quantized to the quart column the way the
    /// chunk's cache quantizes it.
    pub fn preliminary_surface_level(&self, x: i32, z: i32) -> i32 {
        let qx = (x >> 2) << 2;
        let qz = (z >> 2) << 2;
        self.preliminary_surface_level
            .compute(Ctx::new(qx, 0, qz))
            .floor() as i32
    }

    /// The deep dark's aquifers are suppressed entirely; this is the same test
    /// the biome builder uses to find that region.
    fn is_deep_dark(&self, ctx: Ctx) -> bool {
        // Both thresholds are floats in the game, widened here exactly as the
        // JVM widens them.
        self.erosion.compute(ctx) < -0.225f32 as f64 && self.depth.compute(ctx) > 0.9f32 as f64
    }
}

/// `adjustSurfaceLevel`: the aquifer works from a little above the estimated
/// surface, not from the surface itself.
#[inline]
fn adjust_surface_level(level: i32) -> i32 {
    level + 8
}

/// An aquifer bound to one chunk.
pub struct ChunkAquifer<'a> {
    cfg: &'a AquiferConfig,
    /// Above this Y the aquifer is skipped and the global fluid applies.
    /// Derived from the whole chunk, which is why this type is per-chunk.
    skip_sampling_above_y: i32,
    /// Jittered centre of each grid cell, and the aquifer it defines.
    ///
    /// Vanilla sizes a flat array to the grid cells one chunk can reach and
    /// indexes into it; a map keyed by the cell itself is the same thing
    /// without the bookkeeping, because the value is a pure function of the
    /// cell and the search provably never leaves the range vanilla sized for.
    centres: RefCell<super::FxHashMap<(i32, i32, i32), (i32, i32, i32)>>,
    statuses: RefCell<super::FxHashMap<(i32, i32, i32), FluidStatus>>,
    surface: RefCell<super::FxHashMap<(i32, i32), i32>>,
}

impl<'a> ChunkAquifer<'a> {
    /// Build the aquifer for the chunk at `(chunk_x, chunk_z)`.
    pub fn new(cfg: &'a AquiferConfig, chunk_x: i32, chunk_z: i32) -> Self {
        let min_block_x = chunk_x * 16;
        let min_block_z = chunk_z * 16;
        let max_block_x = min_block_x + 15;
        let max_block_z = min_block_z + 15;

        let min_grid_x = grid_x(min_block_x - 5);
        let max_grid_x = grid_x(max_block_x - 5) + 1;
        let min_grid_z = grid_z(min_block_z - 5);
        let max_grid_z = grid_z(max_block_z - 5) + 1;

        // Built once with a placeholder cut-off, because computing the real one
        // needs the surface cache this very value will later be used with. The
        // warmed cache is carried over rather than thrown away.
        let me = Self {
            cfg,
            skip_sampling_above_y: 0,
            centres: RefCell::new(super::FxHashMap::default()),
            statuses: RefCell::new(super::FxHashMap::default()),
            surface: RefCell::new(super::FxHashMap::default()),
        };

        // The highest surface anywhere the chunk's aquifer grid reaches, which
        // is what decides where sampling can stop.
        let top = adjust_surface_level(me.max_preliminary_surface_level(
            from_grid_x(min_grid_x, 0),
            from_grid_z(min_grid_z, 0),
            from_grid_x(max_grid_x, 9),
            from_grid_z(max_grid_z, 9),
        ));
        let skip = from_grid_y(grid_y(top + 12) + 1, 11) - 1;

        Self {
            skip_sampling_above_y: skip,
            ..me
        }
    }

    fn preliminary_surface_level(&self, x: i32, z: i32) -> i32 {
        let key = ((x >> 2) << 2, (z >> 2) << 2);
        if let Some(v) = self.surface.borrow().get(&key) {
            return *v;
        }
        let v = self.cfg.preliminary_surface_level(x, z);
        self.surface.borrow_mut().insert(key, v);
        v
    }

    /// The highest preliminary surface over a quart-spaced grid of columns.
    fn max_preliminary_surface_level(&self, x0: i32, z0: i32, x1: i32, z1: i32) -> i32 {
        let mut best = i32::MIN;
        let mut z = z0;
        while z <= z1 {
            let mut x = x0;
            while x <= x1 {
                best = best.max(self.preliminary_surface_level(x, z));
                x += 4;
            }
            z += 4;
        }
        best
    }

    /// The jittered centre of one grid cell.
    fn centre(&self, gx: i32, gy: i32, gz: i32) -> (i32, i32, i32) {
        if let Some(c) = self.centres.borrow().get(&(gx, gy, gz)) {
            return *c;
        }
        let mut r = self.cfg.random.at(gx, gy, gz);
        // Three draws, in this order; the cell is 16×12×16 but the jitter
        // deliberately covers only part of it.
        let c = (
            from_grid_x(gx, r.next_i32_bounded(10)),
            from_grid_y(gy, r.next_i32_bounded(9)),
            from_grid_z(gz, r.next_i32_bounded(10)),
        );
        self.centres.borrow_mut().insert((gx, gy, gz), c);
        c
    }

    fn status(&self, gx: i32, gy: i32, gz: i32) -> FluidStatus {
        if let Some(s) = self.statuses.borrow().get(&(gx, gy, gz)) {
            return *s;
        }
        let (cx, cy, cz) = self.centre(gx, gy, gz);
        let s = self.compute_fluid(cx, cy, cz);
        self.statuses.borrow_mut().insert((gx, gy, gz), s);
        s
    }

    /// The aquifer at one centre: how high its water stands, and what it is.
    fn compute_fluid(&self, x: i32, y: i32, z: i32) -> FluidStatus {
        let cfg = self.cfg;
        let global = cfg.global_fluid(y);
        let mut min_surface = i32::MAX;
        let y_plus = y + 12;
        let y_minus = y - 12;
        let mut own_column_is_flooded = false;

        for off in SURFACE_SAMPLING_OFFSETS_IN_CHUNKS {
            let sx = x + off[0] * 16;
            let sz = z + off[1] * 16;
            let prelim = self.preliminary_surface_level(sx, sz);
            let adj = adjust_surface_level(prelim);
            let is_own_column = off[0] == 0 && off[1] == 0;

            // Deep under its own column: nothing up there can influence it.
            if is_own_column && y_minus > adj {
                return global;
            }
            let near_or_above_surface = y_plus > adj;
            if near_or_above_surface || is_own_column {
                let surface_fluid = cfg.global_fluid(adj);
                if surface_fluid.at(adj) != Fill::Air {
                    if is_own_column {
                        own_column_is_flooded = true;
                    }
                    if near_or_above_surface {
                        return surface_fluid;
                    }
                }
            }
            min_surface = min_surface.min(prelim);
        }

        let level = self.compute_surface_level(x, y, z, global, min_surface, own_column_is_flooded);
        FluidStatus {
            level,
            kind: self.compute_fluid_type(x, y, z, global, level),
        }
    }

    /// Where this aquifer's water line sits.
    fn compute_surface_level(
        &self,
        x: i32,
        y: i32,
        z: i32,
        global: FluidStatus,
        min_surface: i32,
        below_flooded_surface: bool,
    ) -> i32 {
        let cfg = self.cfg;
        let ctx = Ctx::new(x, y, z);
        let (low, high) = if cfg.is_deep_dark(ctx) {
            // The deep dark is dry by construction.
            (-1.0, -1.0)
        } else {
            let depth_below_surface = (min_surface + 8) - y;
            // Close under a flooded surface, aquifers are much likelier to be
            // full; far below, the taper is gone entirely.
            let closeness = if below_flooded_surface {
                clamped_map(depth_below_surface as f64, 0.0, 64.0, 1.0, 0.0)
            } else {
                0.0
            };
            let floodedness = cfg.floodedness.compute(ctx).clamp(-1.0, 1.0);
            let full_threshold = map(closeness, 1.0, 0.0, -0.3, 0.8);
            let any_threshold = map(closeness, 1.0, 0.0, -0.8, 0.4);
            (floodedness - any_threshold, floodedness - full_threshold)
        };

        if high > 0.0 {
            // Full to the global water line.
            global.level
        } else if low > 0.0 {
            self.randomized_fluid_surface_level(x, y, z, min_surface)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    /// A water line snapped to a coarse grid, so neighbouring aquifers share
    /// levels often enough to look like one body of water.
    fn randomized_fluid_surface_level(&self, x: i32, y: i32, z: i32, max_level: i32) -> i32 {
        let gx = x.div_euclid(16);
        let gy = y.div_euclid(40);
        let gz = z.div_euclid(16);
        let base = gy * 40 + 20;
        let spread = self.cfg.spread.compute(Ctx::new(gx, gy, gz)) * 10.0;
        // Quantized to multiples of 3, so the level is stable across a
        // neighbourhood instead of jittering block by block.
        let q = quantize(spread, 3);
        max_level.min(base + q)
    }

    /// Deep, isolated aquifers turn to lava.
    fn compute_fluid_type(&self, x: i32, y: i32, z: i32, global: FluidStatus, level: i32) -> Fill {
        let mut kind = global.kind;
        if level <= -10 && level != WAY_BELOW_MIN_Y && global.kind != Fill::Lava {
            let v = self.cfg.lava.compute(Ctx::new(
                x.div_euclid(64),
                y.div_euclid(40),
                z.div_euclid(64),
            ));
            if v.abs() > 0.3 {
                kind = Fill::Lava;
            }
        }
        kind
    }

    /// The pressure two neighbouring aquifers exert on the density between
    /// them. Positive pushes toward solid — this is what builds the barrier.
    fn pressure(
        &self,
        ctx: Ctx,
        barrier_cache: &mut Option<f64>,
        a: FluidStatus,
        b: FluidStatus,
    ) -> f64 {
        let y = ctx.y;
        let ka = a.at(y);
        let kb = b.at(y);
        // Water meeting lava is always walled off, at full strength.
        if (ka == Fill::Lava && kb == Fill::Water) || (ka == Fill::Water && kb == Fill::Lava) {
            return 2.0;
        }
        let gap = (a.level - b.level).abs();
        if gap == 0 {
            return 0.0;
        }
        let midpoint = 0.5 * (a.level + b.level) as f64;
        let above_midpoint = y as f64 + 0.5 - midpoint;
        let half_gap = gap as f64 / 2.0;
        let slack = half_gap - above_midpoint.abs();
        // Asymmetric on purpose: a barrier is thinner above the midpoint than
        // below it, so aquifer roofs are thin and their floors are thick.
        let shaped = if above_midpoint > 0.0 {
            let t = slack;
            if t > 0.0 {
                t / 1.5
            } else {
                t / 2.5
            }
        } else {
            let t = 3.0 + slack;
            if t > 0.0 {
                t / 3.0
            } else {
                t / 10.0
            }
        };
        // Far from the boundary the noise cannot change the outcome, so
        // vanilla skips sampling it — and the cache means one position pays
        // for the barrier noise at most once.
        let barrier = if !(-2.0..=2.0).contains(&shaped) {
            0.0
        } else {
            match barrier_cache {
                Some(v) => *v,
                None => {
                    let v = self.cfg.barrier.compute(ctx);
                    *barrier_cache = Some(v);
                    v
                }
            }
        };
        2.0 * (barrier + shaped)
    }

    /// What to put at `(x, y, z)` given the interpolated density there.
    pub fn substance(&self, x: i32, y: i32, z: i32, density: f64) -> Fill {
        let cfg = self.cfg;
        if density > 0.0 {
            return Fill::Solid;
        }
        let global = cfg.global_fluid(y);
        if y > self.skip_sampling_above_y {
            return global.at(y);
        }
        if global.at(y) == Fill::Lava {
            return Fill::Lava;
        }

        // The four nearest aquifer centres, by squared distance.
        let gx = grid_x(x - 5);
        let gy = grid_y(y + 1);
        let gz = grid_z(z - 5);
        let mut d = [i32::MAX; 4];
        let mut g = [(0i32, 0i32, 0i32); 4];
        for ox in 0..=1 {
            for oy in -1..=1 {
                for oz in 0..=1 {
                    let cell = (gx + ox, gy + oy, gz + oz);
                    let (cx, cy, cz) = self.centre(cell.0, cell.1, cell.2);
                    let (dx, dy, dz) = (cx - x, cy - y, cz - z);
                    let dist = dx * dx + dy * dy + dz * dz;
                    // A shift-down insertion, keeping the list sorted. The
                    // comparisons are `>=`, so a later cell at an equal
                    // distance displaces an earlier one — matching vanilla.
                    if d[0] >= dist {
                        g[3] = g[2];
                        g[2] = g[1];
                        g[1] = g[0];
                        g[0] = cell;
                        d[3] = d[2];
                        d[2] = d[1];
                        d[1] = d[0];
                        d[0] = dist;
                    } else if d[1] >= dist {
                        g[3] = g[2];
                        g[2] = g[1];
                        g[1] = cell;
                        d[3] = d[2];
                        d[2] = d[1];
                        d[1] = dist;
                    } else if d[2] >= dist {
                        g[3] = g[2];
                        g[2] = cell;
                        d[3] = d[2];
                        d[2] = dist;
                    } else if d[3] >= dist {
                        g[3] = cell;
                        d[3] = dist;
                    }
                }
            }
        }

        let s1 = self.status(g[0].0, g[0].1, g[0].2);
        let sim12 = similarity(d[0], d[1]);
        let here = s1.at(y);
        if sim12 <= 0.0 {
            // The nearest aquifer is alone; no barrier to build.
            return here;
        }
        // Water directly over lava is left alone rather than walled off.
        if here == Fill::Water && cfg.global_fluid(y - 1).at(y - 1) == Fill::Lava {
            return here;
        }

        let ctx = Ctx::new(x, y, z);
        let mut barrier_cache = None;
        let s2 = self.status(g[1].0, g[1].1, g[1].2);
        if density + sim12 * self.pressure(ctx, &mut barrier_cache, s1, s2) > 0.0 {
            return Fill::Solid;
        }
        let s3 = self.status(g[2].0, g[2].1, g[2].2);
        let sim13 = similarity(d[0], d[2]);
        if sim13 > 0.0
            && density + sim12 * sim13 * self.pressure(ctx, &mut barrier_cache, s1, s3) > 0.0
        {
            return Fill::Solid;
        }
        let sim23 = similarity(d[1], d[2]);
        if sim23 > 0.0
            && density + sim12 * sim23 * self.pressure(ctx, &mut barrier_cache, s2, s3) > 0.0
        {
            return Fill::Solid;
        }
        here
    }
}

/// `Mth.map`: rescale from one range to another, extrapolating outside it.
#[inline]
fn map(v: f64, from: f64, to: f64, from_value: f64, to_value: f64) -> f64 {
    let t = (v - from) / (to - from);
    from_value + t * (to_value - from_value)
}

/// `Mth.clampedMap`: as [`map`], but held at the endpoints.
#[inline]
fn clamped_map(v: f64, from: f64, to: f64, from_value: f64, to_value: f64) -> f64 {
    let t = (v - from) / (to - from);
    if t < 0.0 {
        from_value
    } else if t > 1.0 {
        to_value
    } else {
        from_value + t * (to_value - from_value)
    }
}

/// `Mth.quantize`: round down to a multiple of `step`, including below zero.
#[inline]
fn quantize(v: f64, step: i32) -> i32 {
    (v / step as f64).floor() as i32 * step
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fluid_status_is_air_at_and_above_its_line() {
        let s = FluidStatus {
            level: 63,
            kind: Fill::Water,
        };
        assert_eq!(s.at(62), Fill::Water);
        assert_eq!(s.at(63), Fill::Air);
        assert_eq!(s.at(64), Fill::Air);
    }

    #[test]
    fn similarity_falls_off_over_twenty_five() {
        assert_eq!(similarity(100, 100), 1.0);
        assert_eq!(similarity(100, 125), 0.0);
        assert!(similarity(100, 150) < 0.0);
        // `FLOWING_UPDATE_SIMILARITY` is genuinely negative in vanilla.
        assert!(FLOWING_UPDATE_SIMILARITY < 0.0);
    }

    #[test]
    fn the_grid_floors_on_the_negative_side() {
        // A truncating divide here would fold two cells into one around the
        // origin and make aquifers visibly repeat.
        assert_eq!(grid_x(-1), -1);
        assert_eq!(grid_x(-16), -1);
        assert_eq!(grid_x(-17), -2);
        assert_eq!(grid_y(-1), -1);
        assert_eq!(grid_y(-12), -1);
        assert_eq!(grid_y(-13), -2);
        assert_eq!(from_grid_x(grid_x(-17), 0), -32);
        assert_eq!(from_grid_y(grid_y(-13), 0), -24);
    }

    #[test]
    fn quantize_rounds_down_not_toward_zero() {
        assert_eq!(quantize(7.0, 3), 6);
        assert_eq!(quantize(-1.0, 3), -3);
        assert_eq!(quantize(-3.0, 3), -3);
        assert_eq!(quantize(0.0, 3), 0);
    }

    #[test]
    fn map_extrapolates_and_clamped_map_does_not() {
        assert_eq!(map(1.0, 1.0, 0.0, -0.3, 0.8), -0.3);
        assert_eq!(map(0.0, 1.0, 0.0, -0.3, 0.8), 0.8);
        // Outside the range `map` keeps going and `clamped_map` stops.
        assert!(map(2.0, 1.0, 0.0, -0.3, 0.8) < -0.3);
        assert_eq!(clamped_map(2.0, 1.0, 0.0, -0.3, 0.8), -0.3);
        assert_eq!(clamped_map(-1.0, 0.0, 64.0, 1.0, 0.0), 1.0);
        assert_eq!(clamped_map(100.0, 0.0, 64.0, 1.0, 0.0), 0.0);
    }
}
