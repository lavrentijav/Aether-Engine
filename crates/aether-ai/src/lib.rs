//! # aether-ai
//!
//! Crowd navigation for the Data-Oriented entity engine (spec §9.3).
//!
//! Instead of running A\* per mob, a whole crowd heading for the same goal
//! shares **one** precomputed vector field. The goal is spread outward once with
//! a Dijkstra **integration field** (distance-to-goal over passable cells), then
//! frozen into a **flow field**: every cell stores the direction of its
//! cheapest neighbour. Each mob then reads its cell's direction in **O(1)** —
//! no per-mob search, no world queries.
//!
//! ```text
//!   [ target ] --Dijkstra--> [ integration field: cost-to-goal per cell ]
//!                                        │
//!                                        ▼
//!                            [ flow field: step direction per cell ]
//!                                        │
//!             mob at (x,z) ──O(1) lookup──┘──► move along dir(x,z)
//! ```
//!
//! Navigation is on the horizontal `(x, z)` grid — the plane mobs walk. Movement
//! is 8-directional; diagonals never cut a blocked corner.
//!
//! ```
//! use aether_ai::{NavGrid, FlowField};
//!
//! let mut grid = NavGrid::new(5, 1);
//! let field = FlowField::toward(&grid, (4, 0));
//! // A mob at the far end steps east, toward the target.
//! assert_eq!(field.dir(0, 0), Some((1, 0)));
//! assert_eq!(field.dir(4, 0), Some((0, 0))); // already on the goal
//! ```

use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Orthogonal step cost (integer to keep the field exact and float-free).
const ORTHO: u32 = 10;
/// Diagonal step cost (≈ `ORTHO * √2`).
const DIAG: u32 = 14;
/// Cost sentinel for "unreachable".
const UNREACHABLE: u32 = u32::MAX;

/// The eight neighbour offsets and their step cost; orthogonals first so ties
/// prefer straight moves over diagonals.
const NEIGHBOURS: [(i32, i32, u32); 8] = [
    (1, 0, ORTHO),
    (-1, 0, ORTHO),
    (0, 1, ORTHO),
    (0, -1, ORTHO),
    (1, 1, DIAG),
    (1, -1, DIAG),
    (-1, 1, DIAG),
    (-1, -1, DIAG),
];

/// A passability grid over the horizontal `(x, z)` plane.
#[derive(Debug, Clone)]
pub struct NavGrid {
    w: usize,
    d: usize,
    blocked: Vec<bool>,
}

impl NavGrid {
    /// A fully-open `w × d` grid.
    pub fn new(w: usize, d: usize) -> Self {
        Self {
            w,
            d,
            blocked: vec![false; w * d],
        }
    }

    /// Grid extent as `(w, d)`.
    #[inline]
    pub fn dims(&self) -> (usize, usize) {
        (self.w, self.d)
    }

    #[inline]
    fn idx(&self, x: usize, z: usize) -> usize {
        z * self.w + x
    }

    #[inline]
    fn in_bounds(&self, x: i32, z: i32) -> bool {
        x >= 0 && z >= 0 && (x as usize) < self.w && (z as usize) < self.d
    }

    /// Mark `(x, z)` blocked (impassable).
    pub fn block(&mut self, x: usize, z: usize) {
        let i = self.idx(x, z);
        self.blocked[i] = true;
    }

    /// Set the passability of `(x, z)`.
    pub fn set_blocked(&mut self, x: usize, z: usize, blocked: bool) {
        let i = self.idx(x, z);
        self.blocked[i] = blocked;
    }

    /// Whether `(x, z)` is blocked. Out-of-bounds counts as blocked.
    #[inline]
    pub fn is_blocked(&self, x: i32, z: i32) -> bool {
        if !self.in_bounds(x, z) {
            return true;
        }
        self.blocked[self.idx(x as usize, z as usize)]
    }

    /// Whether a diagonal move from `(x, z)` by `(dx, dz)` is allowed: the two
    /// orthogonally-adjacent cells it squeezes past must both be open, so a mob
    /// can't clip through a blocked corner.
    #[inline]
    fn diagonal_ok(&self, x: i32, z: i32, dx: i32, dz: i32) -> bool {
        !self.is_blocked(x + dx, z) && !self.is_blocked(x, z + dz)
    }
}

/// A precomputed navigation field toward a fixed target on a [`NavGrid`].
///
/// Holds the per-cell cost-to-goal (the integration field) and the derived step
/// direction (the flow field).
#[derive(Debug, Clone)]
pub struct FlowField {
    w: usize,
    d: usize,
    target: (usize, usize),
    cost: Vec<u32>,
    dir: Vec<(i8, i8)>,
}

impl FlowField {
    /// Compute the flow field steering every reachable cell toward `target`.
    ///
    /// If the target is out of bounds or blocked, the field is entirely
    /// unreachable.
    pub fn toward(grid: &NavGrid, target: (usize, usize)) -> Self {
        let (w, d) = grid.dims();
        let mut cost = vec![UNREACHABLE; w * d];
        let mut dir = vec![(0i8, 0i8); w * d];

        let (tx, tz) = target;
        let valid_target = tx < w && tz < d && !grid.is_blocked(tx as i32, tz as i32);

        if valid_target {
            Self::integrate(grid, target, &mut cost);
            Self::derive_directions(grid, &cost, &mut dir);
        }

        Self {
            w,
            d,
            target,
            cost,
            dir,
        }
    }

    /// Dijkstra outward from the target, filling the cost-to-goal field.
    fn integrate(grid: &NavGrid, target: (usize, usize), cost: &mut [u32]) {
        let (w, _d) = grid.dims();
        let ti = target.1 * w + target.0;
        cost[ti] = 0;
        let mut heap: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
        heap.push(Reverse((0, ti)));

        while let Some(Reverse((c, i))) = heap.pop() {
            if c > cost[i] {
                continue; // stale heap entry
            }
            let x = (i % w) as i32;
            let z = (i / w) as i32;
            for (dx, dz, step) in NEIGHBOURS {
                let (nx, nz) = (x + dx, z + dz);
                if grid.is_blocked(nx, nz) {
                    continue;
                }
                if dx != 0 && dz != 0 && !grid.diagonal_ok(x, z, dx, dz) {
                    continue; // no corner-cutting
                }
                let ni = nz as usize * w + nx as usize;
                let nc = c + step;
                if nc < cost[ni] {
                    cost[ni] = nc;
                    heap.push(Reverse((nc, ni)));
                }
            }
        }
    }

    /// For every reachable cell, pick the neighbour with the lowest cost and
    /// store the step toward it.
    fn derive_directions(grid: &NavGrid, cost: &[u32], dir: &mut [(i8, i8)]) {
        let w = grid.dims().0;
        for i in 0..cost.len() {
            if cost[i] == UNREACHABLE || cost[i] == 0 {
                continue; // unreachable or the target itself -> (0, 0)
            }
            let x = (i % w) as i32;
            let z = (i / w) as i32;
            let mut best = cost[i];
            let mut best_dir = (0i8, 0i8);
            for (dx, dz, _step) in NEIGHBOURS {
                let (nx, nz) = (x + dx, z + dz);
                if grid.is_blocked(nx, nz) {
                    continue;
                }
                if dx != 0 && dz != 0 && !grid.diagonal_ok(x, z, dx, dz) {
                    continue;
                }
                let nc = cost[nz as usize * w + nx as usize];
                if nc < best {
                    best = nc;
                    best_dir = (dx as i8, dz as i8);
                }
            }
            dir[i] = best_dir;
        }
    }

    /// The target this field steers toward.
    #[inline]
    pub fn target(&self) -> (usize, usize) {
        self.target
    }

    #[inline]
    fn idx(&self, x: usize, z: usize) -> Option<usize> {
        (x < self.w && z < self.d).then(|| z * self.w + x)
    }

    /// Whether `(x, z)` can reach the target.
    pub fn reachable(&self, x: usize, z: usize) -> bool {
        self.idx(x, z).is_some_and(|i| self.cost[i] != UNREACHABLE)
    }

    /// Cost-to-goal at `(x, z)`, or `None` if unreachable / out of bounds.
    pub fn cost(&self, x: usize, z: usize) -> Option<u32> {
        let i = self.idx(x, z)?;
        (self.cost[i] != UNREACHABLE).then_some(self.cost[i])
    }

    /// The step direction at `(x, z)` as `(dx, dz)` in `{-1, 0, 1}`:
    /// `Some((0, 0))` on the target, `None` if the cell is blocked, unreachable
    /// or out of bounds.
    pub fn dir(&self, x: usize, z: usize) -> Option<(i8, i8)> {
        let i = self.idx(x, z)?;
        (self.cost[i] != UNREACHABLE).then_some(self.dir[i])
    }

    /// The step direction as a unit-length `(dx, dz)` vector (diagonals
    /// normalised), or `None` if there is nowhere to go. On the target, returns
    /// `Some((0.0, 0.0))`.
    pub fn steer(&self, x: usize, z: usize) -> Option<(f64, f64)> {
        let (dx, dz) = self.dir(x, z)?;
        if dx == 0 && dz == 0 {
            return Some((0.0, 0.0));
        }
        let (fx, fz) = (dx as f64, dz as f64);
        let len = (fx * fx + fz * fz).sqrt();
        Some((fx / len, fz / len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_row_points_at_target() {
        let grid = NavGrid::new(6, 1);
        let f = FlowField::toward(&grid, (5, 0));
        for x in 0..5 {
            assert_eq!(f.dir(x, 0), Some((1, 0)), "x={x}");
        }
        assert_eq!(f.dir(5, 0), Some((0, 0)));
        assert_eq!(f.cost(5, 0), Some(0));
        assert_eq!(f.cost(0, 0), Some(50)); // 5 orthogonal steps
    }

    #[test]
    fn cost_decreases_toward_target_in_open_field() {
        let grid = NavGrid::new(9, 9);
        let f = FlowField::toward(&grid, (4, 4));
        // Every cell's chosen neighbour must be strictly closer to the goal.
        for z in 0..9 {
            for x in 0..9 {
                if (x, z) == (4, 4) {
                    continue;
                }
                let (dx, dz) = f.dir(x, z).unwrap();
                let nx = (x as i32 + dx as i32) as usize;
                let nz = (z as i32 + dz as i32) as usize;
                assert!(
                    f.cost(nx, nz).unwrap() < f.cost(x, z).unwrap(),
                    "at ({x},{z}) dir ({dx},{dz}) did not descend"
                );
            }
        }
    }

    #[test]
    fn diagonal_is_preferred_when_cheaper() {
        let grid = NavGrid::new(5, 5);
        let f = FlowField::toward(&grid, (4, 4));
        // From the opposite corner the cheapest move is the diagonal.
        assert_eq!(f.dir(0, 0), Some((1, 1)));
    }

    #[test]
    fn routes_around_a_wall() {
        // A vertical wall at x=2 spanning z=0..3, with a gap at z=3, between a
        // mob at (0,0) and the target at (4,0).
        let mut grid = NavGrid::new(5, 5);
        for z in 0..3 {
            grid.block(2, z);
        }
        let f = FlowField::toward(&grid, (4, 0));
        // The mob must not be told to walk straight into the wall column.
        let (dx, _dz) = f.dir(1, 0).unwrap();
        // Stepping east from (1,0) would enter the blocked (2,0): forbidden.
        assert!(
            !(dx == 1 && f.dir(1, 0).unwrap().1 == 0),
            "steered into the wall"
        );
        // But the target is still reachable from the mob's cell (around the gap).
        assert!(f.reachable(0, 0));
        assert!(f.cost(0, 0).unwrap() > f.cost(4, 0).unwrap());
    }

    #[test]
    fn no_corner_cutting_through_blocked_diagonal() {
        // Block the two orthogonal cells around the (1,1)->(0,0) diagonal so the
        // squeeze is illegal.
        let mut grid = NavGrid::new(3, 3);
        grid.block(1, 0);
        grid.block(0, 1);
        let f = FlowField::toward(&grid, (0, 0));
        // (1,1) cannot reach (0,0) by cutting the corner; it must be either
        // unreachable or routed the long way — never a direct (-1,-1) squeeze.
        if let Some(dir) = f.dir(1, 1) {
            assert_ne!(dir, (-1, -1), "cut a blocked corner");
        }
    }

    #[test]
    fn enclosed_target_is_unreachable_from_outside() {
        // Wall off the target (2,2) completely.
        let mut grid = NavGrid::new(5, 5);
        for (x, z) in [
            (1, 2),
            (3, 2),
            (2, 1),
            (2, 3),
            (1, 1),
            (1, 3),
            (3, 1),
            (3, 3),
        ] {
            grid.block(x, z);
        }
        let f = FlowField::toward(&grid, (2, 2));
        assert!(f.reachable(2, 2)); // the target itself
        assert!(!f.reachable(0, 0)); // walled out
        assert_eq!(f.dir(0, 0), None);
        assert_eq!(f.cost(0, 0), None);
    }

    #[test]
    fn blocked_target_yields_empty_field() {
        let mut grid = NavGrid::new(4, 4);
        grid.block(2, 2);
        let f = FlowField::toward(&grid, (2, 2));
        assert!(!f.reachable(2, 2));
        assert_eq!(f.dir(0, 0), None);
    }

    #[test]
    fn steer_normalises_diagonals() {
        let grid = NavGrid::new(3, 3);
        let f = FlowField::toward(&grid, (2, 2));
        let (sx, sz) = f.steer(0, 0).unwrap();
        let inv = 1.0 / 2.0_f64.sqrt();
        assert!((sx - inv).abs() < 1e-12 && (sz - inv).abs() < 1e-12);
        // On the target the steer vector is zero.
        assert_eq!(f.steer(2, 2), Some((0.0, 0.0)));
    }

    #[test]
    fn out_of_bounds_queries_are_none() {
        let grid = NavGrid::new(3, 3);
        let f = FlowField::toward(&grid, (0, 0));
        assert_eq!(f.dir(5, 5), None);
        assert_eq!(f.cost(9, 0), None);
        assert!(!f.reachable(3, 0));
    }
}
