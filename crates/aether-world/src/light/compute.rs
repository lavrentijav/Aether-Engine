//! Light propagation, as level-by-level mask dilation.
//!
//! # The shape of it
//!
//! Vanilla propagates light with a BFS queue: pop a cell, look at six
//! neighbours, push the ones that brighten. Here the frontier is a [`Mask`],
//! and one step of the flood is one [`Mask::dilate6`]:
//!
//! ```text
//! for level in (1..=15).rev():
//!     frontier = dilate6(frontier) & !opaque | sources_at_or_above(level)
//!     everything newly in the frontier is at this level
//! ```
//!
//! Fifteen iterations of a handful of whole-word operations, no allocation per
//! cell, no queue. Two properties fall out of that shape rather than being
//! arranged:
//!
//! * **It is deterministic.** A queue's result depends on insertion order once
//!   several sources interact. This is a pure function of the input masks,
//!   which is what the engine's Safe-Point merges need.
//! * **Descending levels make the assignment trivial.** A cell's final level is
//!   the first one at which it appears, so "assign if unassigned" is the whole
//!   rule — no comparison, no revisiting.
//!
//! # One class fewer than planned
//!
//! `DESIGN_NOTES` §8.4 expected three opacity classes. Measured against the
//! 1.21.11 block table, opacity takes only three values — 0 (706 blocks), 1
//! (85) and 15 (375) — and vanilla's propagation cost is `max(1, opacity)`.
//! So **opacity 1 costs exactly what air costs**, and for *block* light the
//! middle class does not exist: one `opaque` mask is the whole story.
//!
//! It survives for **sky** light, and only in the vertical pass: sky light
//! falls through opacity-0 cells without dimming at all, which is why the
//! surface is at 15 and the sea floor is not. A leaf or a metre of water costs
//! that free descent, and then the ordinary dilation takes over.

use super::mask::{Mask, DIM};
use super::MAX_LIGHT;

/// A light level per cell, `0..=15`, in the same linear order as [`Mask`].
#[derive(Clone, PartialEq, Eq)]
pub struct Levels {
    data: Vec<u8>,
    height: usize,
}

impl std::fmt::Debug for Levels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Levels({DIM}x{DIM}x{})", self.height)
    }
}

impl Levels {
    /// All dark.
    pub fn zeroed(height: usize) -> Self {
        Self {
            data: vec![0; height * DIM * DIM],
            height,
        }
    }

    /// Height in cells.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The level at `(x, y, z)`.
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        self.data[Mask::index(x, y, z)]
    }

    /// The raw bytes, in linear order — the order a light section is sent in.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Whether every cell holds `level`.
    pub fn is_uniform(&self, level: u8) -> bool {
        self.data.iter().all(|v| *v == level)
    }

    /// The one level every cell holds, if they all hold the same one.
    pub fn uniform(&self) -> Option<u8> {
        let first = *self.data.first()?;
        self.data.iter().all(|v| *v == first).then_some(first)
    }

    /// Assign `level` to every cell of `m` that has none yet.
    fn fill_unassigned(&mut self, m: &Mask, level: u8) {
        for y in 0..self.height {
            for z in 0..DIM {
                for x in 0..DIM {
                    let i = Mask::index(x, y, z);
                    if self.data[i] == 0 && m.get(x, y, z) {
                        self.data[i] = level;
                    }
                }
            }
        }
    }
}

/// A light source: a cell and what it emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    pub x: usize,
    pub y: usize,
    pub z: usize,
    pub level: u8,
}

/// Block light from a set of emitters.
///
/// `opaque` marks cells that block light entirely. An emitter that is *itself*
/// opaque still shines — glowstone is a full opaque cube with emission 15 — so
/// the sources are re-applied at every level rather than filtered out once.
pub fn block_light(opaque: &Mask, sources: &[Source]) -> Levels {
    let h = opaque.height();
    let mut levels = Levels::zeroed(h);
    if sources.is_empty() {
        return levels;
    }

    // One mask per level of "sources at least this bright", built once.
    let mut seeds: Vec<Mask> = Vec::with_capacity(MAX_LIGHT as usize + 1);
    for level in 0..=MAX_LIGHT {
        let mut m = Mask::zeroed(h);
        for s in sources.iter().filter(|s| s.level >= level) {
            m.set(s.x, s.y, s.z, true);
        }
        seeds.push(m);
    }

    let mut frontier = seeds[MAX_LIGHT as usize].clone();
    levels.fill_unassigned(&frontier, MAX_LIGHT);
    for level in (1..MAX_LIGHT).rev() {
        frontier = frontier.dilate6();
        frontier.and_not(opaque);
        frontier.or(&seeds[level as usize]);
        levels.fill_unassigned(&frontier, level);
    }
    levels
}

/// Sky light for a column whose top is open to the sky.
///
/// `opaque` blocks light entirely; `dim` is everything that attenuates without
/// blocking — opacity 1, which is leaves and fluids. The distinction only
/// matters here: a `dim` cell costs the free vertical descent, so daylight
/// reaches the sea floor dimmer than the surface even in open water.
pub fn sky_light(opaque: &Mask, dim: &Mask) -> Levels {
    let h = opaque.height();
    let mut levels = Levels::zeroed(h);

    // The vertical sweep. A column sweep rather than a mask operation on
    // purpose: sky light does *not* attenuate going straight down, so it is
    // not a step of the uniform-cost flood at all and cannot be expressed as
    // one. This is the access pattern DESIGN_NOTES §8.9 flags as the one
    // neither Morton nor the AVX-Cell suits.
    let mut open = Mask::zeroed(h);
    for z in 0..DIM {
        for x in 0..DIM {
            for y in (0..h).rev() {
                if opaque.get(x, y, z) || dim.get(x, y, z) {
                    break;
                }
                open.set(x, y, z, true);
            }
        }
    }

    levels.fill_unassigned(&open, MAX_LIGHT);
    let mut frontier = open;
    for level in (1..MAX_LIGHT).rev() {
        frontier = frontier.dilate6();
        frontier.and_not(opaque);
        levels.fill_unassigned(&frontier, level);
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference flood fill, written from the description of how light
    /// spreads — "a neighbour is one level dimmer, opaque cells stop it" —
    /// rather than from the dilation under test. A bug shared by both would
    /// otherwise pass.
    fn reference_block_light(opaque: &Mask, sources: &[Source]) -> Levels {
        let h = opaque.height();
        let mut levels = Levels::zeroed(h);
        let mut queue: Vec<(usize, usize, usize, u8)> = Vec::new();
        for s in sources {
            let i = Mask::index(s.x, s.y, s.z);
            if levels.data[i] < s.level {
                levels.data[i] = s.level;
                queue.push((s.x, s.y, s.z, s.level));
            }
        }
        while let Some((x, y, z, l)) = queue.pop() {
            if l <= 1 {
                continue;
            }
            let (xi, yi, zi) = (x as i32, y as i32, z as i32);
            for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
            {
                let (nx, ny, nz) = (xi + dx, yi + dy, zi + dz);
                if !(0..DIM as i32).contains(&nx)
                    || !(0..h as i32).contains(&ny)
                    || !(0..DIM as i32).contains(&nz)
                {
                    continue;
                }
                let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
                if opaque.get(nx, ny, nz) {
                    continue;
                }
                let i = Mask::index(nx, ny, nz);
                if levels.data[i] < l - 1 {
                    levels.data[i] = l - 1;
                    queue.push((nx, ny, nz, l - 1));
                }
            }
        }
        levels
    }

    fn src(x: usize, y: usize, z: usize, level: u8) -> Source {
        Source { x, y, z, level }
    }

    #[test]
    fn one_torch_in_open_air_falls_off_by_one_per_step() {
        let h = 16;
        let opaque = Mask::zeroed(h);
        let l = block_light(&opaque, &[src(8, 8, 8, 14)]);
        assert_eq!(l.get(8, 8, 8), 14);
        assert_eq!(l.get(9, 8, 8), 13);
        assert_eq!(l.get(8, 8, 8 + 5), 9);
        // Manhattan distance, in every direction at once.
        for (x, y, z) in [(3usize, 8usize, 8usize), (8, 3, 8), (8, 8, 3), (10, 10, 10)] {
            let d = x.abs_diff(8) + y.abs_diff(8) + z.abs_diff(8);
            assert_eq!(l.get(x, y, z), 14u8.saturating_sub(d as u8), "{x},{y},{z}");
        }
    }

    #[test]
    fn light_runs_out_rather_than_wrapping_round() {
        let l = block_light(&Mask::zeroed(16), &[src(0, 0, 0, 3)]);
        assert_eq!(l.get(3, 0, 0), 0, "level 3 reaches two cells, not three");
        assert_eq!(l.get(2, 0, 0), 1);
        assert_eq!(l.get(15, 15, 15), 0);
    }

    #[test]
    fn an_opaque_emitter_still_shines() {
        // Glowstone is a full opaque cube emitting 15. Filtering emitters
        // through `!opaque` once would put it out.
        let h = 8;
        let mut opaque = Mask::zeroed(h);
        opaque.set(4, 4, 4, true);
        let l = block_light(&opaque, &[src(4, 4, 4, 15)]);
        assert_eq!(l.get(4, 4, 4), 15);
        assert_eq!(l.get(5, 4, 4), 14);
    }

    #[test]
    fn a_wall_casts_a_shadow_and_light_goes_round_it() {
        // The case a step-count model gets wrong and a flood fill gets right.
        let h = 8;
        let mut opaque = Mask::zeroed(h);
        for y in 0..h {
            for z in 0..DIM {
                opaque.set(8, y, z, true); // a wall across the whole column
            }
        }
        let l = block_light(&opaque, &[src(4, 4, 4, 15)]);
        assert_eq!(l.get(8, 4, 4), 0, "the wall itself is dark");
        assert_eq!(l.get(9, 4, 4), 0, "and so is everything behind it");
        assert!(l.get(7, 4, 4) > 0, "the lit side is lit");
    }

    #[test]
    fn light_reaches_round_a_corner_at_the_cost_of_the_detour() {
        let h = 8;
        let mut opaque = Mask::zeroed(h);
        // A wall with a one-cell gap in it.
        for y in 0..h {
            for z in 0..DIM {
                if !(y == 4 && z == 4) {
                    opaque.set(8, y, z, true);
                }
            }
        }
        let l = block_light(&opaque, &[src(4, 4, 4, 15)]);
        assert_eq!(l.get(8, 4, 4), 15 - 4, "through the gap");
        assert_eq!(l.get(9, 4, 4), 15 - 5, "and out the other side");
        assert_eq!(l.get(9, 4, 6), 15 - 7, "round the corner, paying for it");
    }

    #[test]
    fn dilation_agrees_with_a_reference_flood_fill() {
        // The test that matters: the same answer as an implementation written
        // from the description, over a world with structure in it.
        let h = 24;
        let mut opaque = Mask::zeroed(h);
        // A deterministic, lumpy obstacle field.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for y in 0..h {
            for z in 0..DIM {
                for x in 0..DIM {
                    if next() % 3 == 0 {
                        opaque.set(x, y, z, true);
                    }
                }
            }
        }
        let sources = vec![
            src(2, 2, 2, 15),
            src(13, 20, 9, 12),
            src(7, 11, 7, 8),
            src(0, 0, 15, 15),
        ];
        let got = block_light(&opaque, &sources);
        let want = reference_block_light(&opaque, &sources);
        for y in 0..h {
            for z in 0..DIM {
                for x in 0..DIM {
                    assert_eq!(
                        got.get(x, y, z),
                        want.get(x, y, z),
                        "at {x},{y},{z}"
                    );
                }
            }
        }
    }

    #[test]
    fn several_sources_take_the_brightest_not_the_sum() {
        let h = 8;
        let l = block_light(&Mask::zeroed(h), &[src(2, 4, 4, 10), src(6, 4, 4, 10)]);
        // Midway between two lamps of 10, two cells from each.
        assert_eq!(l.get(4, 4, 4), 8, "brightest wins; light does not add up");
    }

    #[test]
    fn no_sources_is_no_light_and_no_work() {
        assert!(block_light(&Mask::zeroed(16), &[]).is_uniform(0));
    }

    #[test]
    fn sky_light_falls_to_the_floor_undimmed() {
        // The property that separates sky light from block light: no
        // attenuation going straight down.
        let h = 16;
        let mut opaque = Mask::zeroed(h);
        for z in 0..DIM {
            for x in 0..DIM {
                opaque.set(x, 0, z, true); // a floor
            }
        }
        let l = sky_light(&opaque, &Mask::zeroed(h));
        assert_eq!(l.get(8, h - 1, 8), 15, "at the top");
        assert_eq!(l.get(8, 1, 8), 15, "and still 15 just above the floor");
        assert_eq!(l.get(8, 0, 8), 0, "the floor itself is dark");
    }

    #[test]
    fn a_roof_over_the_whole_column_makes_everything_below_it_dark() {
        let h = 16;
        let mut opaque = Mask::zeroed(h);
        for z in 0..DIM {
            for x in 0..DIM {
                opaque.set(x, 8, z, true);
            }
        }
        let l = sky_light(&opaque, &Mask::zeroed(h));
        assert_eq!(l.get(8, 9, 8), 15, "above the roof");
        assert_eq!(l.get(8, 8, 8), 0, "the roof itself");
        for y in 0..8 {
            assert_eq!(l.get(8, y, 8), 0, "nothing reaches y={y}");
        }
    }

    #[test]
    fn a_partial_roof_fades_inwards_from_its_edge() {
        // A roof narrower than the light's reach does not make the middle
        // dark — it makes a gradient, and getting that backwards is the
        // difference between a shadow and a step function.
        let h = 16;
        let mut opaque = Mask::zeroed(h);
        for z in 0..DIM {
            for x in 4..DIM {
                opaque.set(x, 8, z, true);
            }
        }
        let l = sky_light(&opaque, &Mask::zeroed(h));
        assert_eq!(l.get(3, 7, 8), 15, "outside the roof, daylight falls freely");
        let under: Vec<u8> = (4..12).map(|x| l.get(x, 7, 8)).collect();
        assert_eq!(under[0], 14, "one step in from the edge");
        for w in under.windows(2) {
            assert!(w[1] < w[0] || w[1] == 0, "not monotonic: {under:?}");
        }
        assert_eq!(*under.last().unwrap(), 7, "eight cells in");
    }

    #[test]
    fn a_dim_block_costs_the_free_descent() {
        // Water and leaves. Above them daylight is 15; below, the free fall is
        // over and every further cell costs a level.
        let h = 16;
        let mut dim = Mask::zeroed(h);
        for z in 0..DIM {
            for x in 0..DIM {
                dim.set(x, 10, z, true);
            }
        }
        let l = sky_light(&Mask::zeroed(h), &dim);
        assert_eq!(l.get(8, 11, 8), 15, "above the surface");
        assert_eq!(l.get(8, 10, 8), 14, "the dim cell itself");
        assert_eq!(l.get(8, 9, 8), 13, "and it keeps costing below");
        assert_eq!(l.get(8, 0, 8), 4, "ten cells down");
    }

    #[test]
    fn an_open_column_is_uniformly_lit() {
        let l = sky_light(&Mask::zeroed(8), &Mask::zeroed(8));
        assert_eq!(l.uniform(), Some(15), "nothing in the way means all 15");
    }
}
