//! Lighting.
//!
//! Two things live here:
//!
//! * [`FullBright`] — the "always lit" fallback (max light everywhere) that
//!   shipped so worlds could render before real lighting existed.
//! * A real **flood-fill light engine** ([`compute_light`]) that computes block
//!   light (from emissive blocks) and sky light (from the top down) with a
//!   breadth-first spread — the Phase 1 roadmap item that replaces `FullBright`.
//!
//! The engine is deliberately data-oriented and self-contained: it reads its
//! world through the [`LightMedium`] trait (opacity + emission over a finite
//! box) and writes a compact [`LightGrid`] of nibble light levels. That keeps
//! the algorithm testable in isolation and lets callers drive it from a
//! sub-chunk column, a stress fixture, or a synthetic scene.
//!
//! ## Model (Phase 1)
//!
//! * **Block light** radiates from emitters, losing one level per block and
//!   stopping at opaque blocks (their interior stays dark; neighbours are lit).
//! * **Sky light** falls straight down at [`MAX_LIGHT`] through transparent
//!   blocks and, once blocked, spreads sideways/downwards losing one level per
//!   block — so caves and overhangs fall into shadow.
//! * Light does **not** enter through the box faces (the volume is treated as
//!   surrounded by darkness on the sides/bottom, open sky on top). Cross-chunk
//!   horizontal bleed is a registered Phase 1 deviation, closed in Phase 2.

use std::collections::VecDeque;

/// Maximum Minecraft light level.
pub const MAX_LIGHT: u8 = 15;

/// Read-only access to per-block light levels (`0..=15`).
pub trait LightView {
    /// Emitted/block light at `(x, y, z)`.
    fn block_light(&self, x: i32, y: i32, z: i32) -> u8;
    /// Sky light at `(x, y, z)`.
    fn sky_light(&self, x: i32, y: i32, z: i32) -> u8;
}

/// A lighting source that always returns [`MAX_LIGHT`] — the "always lit"
/// fallback used before real lighting existed and still handy for previews.
#[derive(Debug, Clone, Copy, Default)]
pub struct FullBright;

impl LightView for FullBright {
    #[inline]
    fn block_light(&self, _x: i32, _y: i32, _z: i32) -> u8 {
        MAX_LIGHT
    }
    #[inline]
    fn sky_light(&self, _x: i32, _y: i32, _z: i32) -> u8 {
        MAX_LIGHT
    }
}

/// The medium light travels through: an opacity + emission oracle over a finite
/// box addressed in local coordinates (`0..w`, `0..h`, `0..d`).
///
/// `y` is the vertical axis; sky light enters from `y = h - 1` downwards.
pub trait LightMedium {
    /// Box extent as `(w, h, d)` — X, Y (vertical), Z.
    fn dims(&self) -> (usize, usize, usize);

    /// Whether the block at `(x, y, z)` is opaque: it blocks direct sky light
    /// and stops block light (light neither fills nor passes through it).
    fn opaque(&self, x: usize, y: usize, z: usize) -> bool;

    /// Self-emitted block light of `(x, y, z)` (`0..=15`; e.g. torch = 14,
    /// glowstone = 15). Emitters light their transparent neighbours even when
    /// the emitter block itself is opaque.
    fn emission(&self, x: usize, y: usize, z: usize) -> u8;
}

/// Computed per-block light over a box: a block-light and a sky-light level
/// (`0..=15`) for every cell, stored one byte each in `x`-fastest order.
#[derive(Clone, PartialEq, Eq)]
pub struct LightGrid {
    w: usize,
    h: usize,
    d: usize,
    block: Vec<u8>,
    sky: Vec<u8>,
}

impl LightGrid {
    fn new(w: usize, h: usize, d: usize) -> Self {
        let n = w * h * d;
        Self {
            w,
            h,
            d,
            block: vec![0; n],
            sky: vec![0; n],
        }
    }

    /// Box extent as `(w, h, d)`.
    #[inline]
    pub fn dims(&self) -> (usize, usize, usize) {
        (self.w, self.h, self.d)
    }

    #[inline]
    fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        debug_assert!(x < self.w && y < self.h && z < self.d);
        (y * self.d + z) * self.w + x
    }

    /// Block (emitted) light at local `(x, y, z)`.
    #[inline]
    pub fn block_light(&self, x: usize, y: usize, z: usize) -> u8 {
        self.block[self.idx(x, y, z)]
    }

    /// Sky light at local `(x, y, z)`.
    #[inline]
    pub fn sky_light(&self, x: usize, y: usize, z: usize) -> u8 {
        self.sky[self.idx(x, y, z)]
    }

    /// The greater of block and sky light — what a renderer usually samples.
    #[inline]
    pub fn combined_light(&self, x: usize, y: usize, z: usize) -> u8 {
        let i = self.idx(x, y, z);
        self.block[i].max(self.sky[i])
    }
}

impl std::fmt::Debug for LightGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LightGrid({}x{}x{}, block_sum={}, sky_sum={})",
            self.w,
            self.h,
            self.d,
            self.block.iter().map(|&v| v as u32).sum::<u32>(),
            self.sky.iter().map(|&v| v as u32).sum::<u32>(),
        )
    }
}

// The six axis-aligned neighbour offsets.
const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// Compute block + sky light for a [`LightMedium`], returning a [`LightGrid`].
///
/// This is the reference (single-threaded) flood-fill. It runs two independent
/// BFS passes — one seeded from emitters, one from the open sky column — each
/// spreading light outward and losing a level per block until it reaches an
/// opaque block or the box edge.
pub fn compute_light<M: LightMedium>(medium: &M) -> LightGrid {
    let (w, h, d) = medium.dims();
    let mut grid = LightGrid::new(w, h, d);
    if w == 0 || h == 0 || d == 0 {
        return grid;
    }

    compute_block_light(medium, &mut grid);
    compute_sky_light(medium, &mut grid);
    grid
}

fn compute_block_light<M: LightMedium>(medium: &M, grid: &mut LightGrid) {
    let (w, h, d) = (grid.w, grid.h, grid.d);
    let mut queue: VecDeque<(usize, usize, usize)> = VecDeque::new();

    // Seed from every emitter. An emitter lights its own cell and, during the
    // spread, its transparent neighbours — even if the emitter is opaque.
    for y in 0..h {
        for z in 0..d {
            for x in 0..w {
                let e = medium.emission(x, y, z).min(MAX_LIGHT);
                if e > 0 {
                    let i = grid.idx(x, y, z);
                    if e > grid.block[i] {
                        grid.block[i] = e;
                        queue.push_back((x, y, z));
                    }
                }
            }
        }
    }

    spread(medium, w, h, d, &mut queue, &mut grid.block);
}

fn compute_sky_light<M: LightMedium>(medium: &M, grid: &mut LightGrid) {
    let (w, h, d) = (grid.w, grid.h, grid.d);
    let mut queue: VecDeque<(usize, usize, usize)> = VecDeque::new();

    // Seed the open sky: every cell reachable straight down from the top before
    // the first opaque block gets full sky light. Seeding the whole transparent
    // column (not just its top cell) is what keeps straight-down sky light at
    // MAX with no attenuation, matching Vanilla.
    for z in 0..d {
        for x in 0..w {
            for y in (0..h).rev() {
                if medium.opaque(x, y, z) {
                    break;
                }
                let i = grid.idx(x, y, z);
                grid.sky[i] = MAX_LIGHT;
                queue.push_back((x, y, z));
            }
        }
    }

    spread(medium, w, h, d, &mut queue, &mut grid.sky);
}

/// Shared BFS spread: pop lit cells, push `level - 1` into transparent
/// neighbours that are currently darker.
fn spread<M: LightMedium>(
    medium: &M,
    w: usize,
    h: usize,
    d: usize,
    queue: &mut VecDeque<(usize, usize, usize)>,
    light: &mut [u8],
) {
    while let Some((x, y, z)) = queue.pop_front() {
        let level = light[(y * d + z) * w + x];
        if level <= 1 {
            continue;
        }
        let next = level - 1;
        for (dx, dy, dz) in NEIGHBOURS {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            let nz = z as i32 + dz;
            if nx < 0 || ny < 0 || nz < 0 {
                continue;
            }
            let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
            if nx >= w || ny >= h || nz >= d {
                continue;
            }
            if medium.opaque(nx, ny, nz) {
                continue;
            }
            let ni = (ny * d + nz) * w + nx;
            if light[ni] < next {
                light[ni] = next;
                queue.push_back((nx, ny, nz));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built scene: opaque + emission grids, x-fastest like [`LightGrid`].
    struct Scene {
        w: usize,
        h: usize,
        d: usize,
        opaque: Vec<bool>,
        emission: Vec<u8>,
    }

    impl Scene {
        fn new(w: usize, h: usize, d: usize) -> Self {
            Self {
                w,
                h,
                d,
                opaque: vec![false; w * h * d],
                emission: vec![0; w * h * d],
            }
        }
        fn idx(&self, x: usize, y: usize, z: usize) -> usize {
            (y * self.d + z) * self.w + x
        }
        fn set_opaque(&mut self, x: usize, y: usize, z: usize) {
            let i = self.idx(x, y, z);
            self.opaque[i] = true;
        }
        fn set_emission(&mut self, x: usize, y: usize, z: usize, e: u8) {
            let i = self.idx(x, y, z);
            self.emission[i] = e;
        }
    }

    impl LightMedium for Scene {
        fn dims(&self) -> (usize, usize, usize) {
            (self.w, self.h, self.d)
        }
        fn opaque(&self, x: usize, y: usize, z: usize) -> bool {
            self.opaque[self.idx(x, y, z)]
        }
        fn emission(&self, x: usize, y: usize, z: usize) -> u8 {
            self.emission[self.idx(x, y, z)]
        }
    }

    #[test]
    fn full_bright_is_always_max() {
        let l = FullBright;
        assert_eq!(l.block_light(0, 0, 0), 15);
        assert_eq!(l.sky_light(-100, 200, 40), 15);
    }

    #[test]
    fn torch_radiates_and_attenuates() {
        // A single torch (emission 14) in the middle of open air.
        let mut s = Scene::new(9, 1, 9);
        s.set_emission(4, 0, 4, 14);
        let g = compute_light(&s);

        assert_eq!(g.block_light(4, 0, 4), 14, "torch cell");
        // Manhattan distance 1 -> 13, distance 2 -> 12, ...
        assert_eq!(g.block_light(5, 0, 4), 13);
        assert_eq!(g.block_light(4, 0, 6), 12);
        assert_eq!(g.block_light(0, 0, 4), 10); // distance 4
        assert_eq!(g.block_light(0, 0, 0), 6); // distance 8
    }

    #[test]
    fn opaque_wall_casts_a_block_light_shadow() {
        // Torch at x=0, a full opaque wall at x=1 (except one gap) — light must
        // go around, not through.
        let mut s = Scene::new(5, 1, 3);
        s.set_emission(0, 0, 1, 14);
        for z in 0..3 {
            s.set_opaque(1, 0, z);
        }
        let g = compute_light(&s);
        // Directly behind the wall on the torch's row is dark (blocked + far).
        assert_eq!(g.block_light(1, 0, 1), 0, "wall interior stays dark");
        // Two cells behind, only reachable by going around, is dimmer than the
        // straight-line distance would give without the wall.
        assert!(g.block_light(2, 0, 1) < 12);
    }

    #[test]
    fn sky_light_falls_straight_down_without_attenuation() {
        // Open column, 8 tall: every cell should be full sky light.
        let s = Scene::new(1, 8, 1);
        let g = compute_light(&s);
        for y in 0..8 {
            assert_eq!(g.sky_light(0, y, 0), 15, "y={y}");
        }
    }

    #[test]
    fn sky_light_is_blocked_below_a_ceiling() {
        // A solid ceiling at the top: everything under it starts dark and is
        // only reached by spreading in from... nowhere (single column) -> dark.
        let mut s = Scene::new(1, 4, 1);
        s.set_opaque(0, 3, 0); // ceiling at the very top
        let g = compute_light(&s);
        assert_eq!(g.sky_light(0, 3, 0), 0, "opaque ceiling itself unlit");
        for y in 0..3 {
            assert_eq!(g.sky_light(0, y, 0), 0, "shadowed under ceiling y={y}");
        }
    }

    #[test]
    fn sky_light_spreads_under_an_overhang() {
        // An overhang covers x=0..2 at the top; x=2 is open to the sky. Sky
        // light should pour down the open column and creep sideways under the
        // overhang, dimming by one per block.
        let w = 4;
        let h = 3;
        let mut s = Scene::new(w, h, 1);
        // Ceiling over x=0 and x=1 at the top row.
        s.set_opaque(0, h - 1, 0);
        s.set_opaque(1, h - 1, 0);
        let g = compute_light(&s);
        // Open column stays full.
        assert_eq!(g.sky_light(2, 0, 0), 15);
        // Just under the lip of the overhang: one step in from the open column.
        assert_eq!(g.sky_light(1, 0, 0), 14);
        assert_eq!(g.sky_light(0, 0, 0), 13);
    }

    #[test]
    fn opaque_emitter_still_lights_neighbours() {
        // Glowstone: opaque *and* emits 15.
        let mut s = Scene::new(3, 1, 1);
        s.set_opaque(1, 0, 0);
        s.set_emission(1, 0, 0, 15);
        let g = compute_light(&s);
        assert_eq!(g.block_light(1, 0, 0), 15, "emitter cell");
        assert_eq!(g.block_light(0, 0, 0), 14, "transparent neighbour lit");
        assert_eq!(g.block_light(2, 0, 0), 14);
    }

    #[test]
    fn brightest_of_two_torches_wins() {
        let mut s = Scene::new(11, 1, 1);
        s.set_emission(0, 0, 0, 14);
        s.set_emission(10, 0, 0, 14);
        let g = compute_light(&s);
        // The midpoint is equidistant; both contribute 14 - 5 = 9.
        assert_eq!(g.block_light(5, 0, 0), 9);
        // Cell next to the left torch takes the left torch's 13, not the far 5.
        assert_eq!(g.block_light(1, 0, 0), 13);
    }

    #[test]
    fn degenerate_dims_do_not_panic() {
        let s = Scene::new(0, 4, 4);
        let g = compute_light(&s);
        assert_eq!(g.dims(), (0, 4, 4));
    }

    #[test]
    fn empty_scene_has_full_sky_and_no_block_light() {
        let s = Scene::new(4, 4, 4);
        let g = compute_light(&s);
        for y in 0..4 {
            for z in 0..4 {
                for x in 0..4 {
                    assert_eq!(g.sky_light(x, y, z), 15);
                    assert_eq!(g.block_light(x, y, z), 0);
                    assert_eq!(g.combined_light(x, y, z), 15);
                }
            }
        }
    }
}
