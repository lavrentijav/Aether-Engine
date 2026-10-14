//! A* pathfinding over the block grid for ground mobs.
//!
//! A simplified port of vanilla's `WalkNodeEvaluator`: a node is a block cell
//! the mob's feet occupy, and a cell is **standable** when the block below has
//! a collision surface a mob can stand on (a full block or a slab, but not a
//! 1.5-tall fence) and the cell plus the one above are clear of blocking
//! collision. A neighbour is the same-level cell ahead, one block up, or a
//! drop of up to [`MAX_DROP`]; lava, fire and cacti are refused. Doors are
//! walls for now — the per-state collision shape decides, and a closed door
//! has one.
//!
//! The evaluator asks the world only three things, through [`Nav`], so it can
//! be unit-tested against a plain in-memory grid and driven from the server by
//! a thin adapter over its block source.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::registry::shapes::Box6;

/// Tallest collision a walking mob steps onto without jumping.
pub const STEP_HEIGHT: f32 = 0.6;
/// Deepest drop a mob will walk off.
pub const MAX_DROP: i32 = 3;

/// What the pathfinder may ask about the world.
pub trait Nav {
    /// The collision boxes of the block at a cell, in block-local coordinates
    /// (`[min_x, min_y, min_z, max_x, max_y, max_z]`). Empty for air.
    fn boxes(&self, x: i32, y: i32, z: i32) -> &'static [Box6];
    /// Whether standing in the cell hurts (lava, fire, a cactus).
    fn hazard(&self, x: i32, y: i32, z: i32) -> bool;
}

/// Whether a collision shape blocks a walking mob rather than being a step it
/// can climb over: a slab's top is below the step height, a fence, wall or
/// full block is not.
#[inline]
fn blocking(boxes: &[Box6]) -> bool {
    boxes.iter().any(|b| b[4] > STEP_HEIGHT)
}

/// Whether a block can carry a mob: it has a collision top no higher than its
/// own cell, so a full block or a slab qualifies but a fence does not.
#[inline]
fn carries(boxes: &[Box6]) -> bool {
    !boxes.is_empty() && boxes.iter().all(|b| b[4] <= 1.0 + 1e-4)
}

/// Whether a cell is empty of collision. The feet cell may hold a step
/// ([`blocking`] allows a shape up to [`STEP_HEIGHT`]); the head cell may not,
/// because a walking mob's body fills it — a bottom slab or closed trapdoor
/// belongs to the feet cell as a step, but at head height it is a wall.
#[inline]
fn free(boxes: &[Box6]) -> bool {
    boxes.is_empty()
}

/// Whether a mob can stand in this cell: the cell and the one above are clear,
/// nothing lethal is there, and the block below can carry it.
///
/// The hazard test covers the support cell as well as the body's two cells: a
/// cactus or magma block is only ever the *floor* under the mob, and picking a
/// standable floor without it would path a mob straight onto the cactus —
/// vanilla gives the node above a `PathType.DAMAGE_OTHER` (malus −1).
pub fn standable<N: Nav + ?Sized>(nav: &N, x: i32, y: i32, z: i32) -> bool {
    if nav.hazard(x, y, z) || nav.hazard(x, y + 1, z) || nav.hazard(x, y - 1, z) {
        return false;
    }
    if blocking(nav.boxes(x, y, z)) || !free(nav.boxes(x, y + 1, z)) {
        return false;
    }
    carries(nav.boxes(x, y - 1, z))
}

/// The nearest standable cell to `from`: the cell itself if it works, else one
/// of its vertical or horizontal neighbours. `None` when the mob is not on
/// standable ground at all.
pub fn resolve_start<N: Nav + ?Sized>(nav: &N, from: [i32; 3]) -> Option<[i32; 3]> {
    if standable(nav, from[0], from[1], from[2]) {
        return Some(from);
    }
    for dy in [1, -1] {
        let c = [from[0], from[1] + dy, from[2]];
        if standable(nav, c[0], c[1], c[2]) {
            return Some(c);
        }
    }
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        for dy in [0, 1, -1] {
            let c = [from[0] + dx, from[1] + dy, from[2] + dz];
            if standable(nav, c[0], c[1], c[2]) {
                return Some(c);
            }
        }
    }
    None
}

/// What a search produced.
pub struct Search {
    /// The waypoints from just after the start through to the node that
    /// reached the goal. Empty when the goal was not reached.
    pub path: Vec<[i32; 3]>,
    /// Nodes pulled off the frontier before the search stopped.
    pub expanded: usize,
    /// Whether a goal node was found.
    pub reached: bool,
}

/// One entry on the frontier, ordered by `f` (smallest first).
struct Frontier {
    f: f64,
    g: f64,
    pos: [i32; 3],
}

impl PartialEq for Frontier {
    fn eq(&self, other: &Self) -> bool {
        self.f == other.f
    }
}
impl Eq for Frontier {}
impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: `BinaryHeap` is a max-heap and A* wants the least `f`.
        other.f.total_cmp(&self.f)
    }
}
impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Run an A* search from `start` (a standable cell) to within `goal_range`
/// blocks of `goal`, expanding at most `max_nodes` nodes.
///
/// `goal_range` is the horizontal distance at which the goal counts as
/// reached; the vertical difference must also be at most one, so a mob can
/// path to a cell beside a target standing on the same or an adjacent level.
pub fn search<N: Nav + ?Sized>(
    nav: &N,
    start: [i32; 3],
    goal: [i32; 3],
    goal_range: f64,
    max_nodes: usize,
) -> Search {
    let range2 = goal_range * goal_range;
    let at_goal = |p: [i32; 3]| {
        let dx = (p[0] - goal[0]) as f64;
        let dz = (p[2] - goal[2]) as f64;
        dx * dx + dz * dz <= range2 && (p[1] - goal[1]).abs() <= 1
    };

    let mut open = BinaryHeap::new();
    let mut g: HashMap<[i32; 3], f64> = HashMap::new();
    let mut came: HashMap<[i32; 3], [i32; 3]> = HashMap::new();
    let mut closed: HashSet<[i32; 3]> = HashSet::new();
    g.insert(start, 0.0);
    open.push(Frontier {
        f: heuristic(start, goal),
        g: 0.0,
        pos: start,
    });

    let mut expanded = 0usize;
    while let Some(entry) = open.pop() {
        let pos = entry.pos;
        if closed.contains(&pos) {
            continue;
        }
        match g.get(&pos) {
            Some(&best) if entry.g > best + 1e-9 => continue,
            None => continue,
            _ => {}
        }
        if expanded >= max_nodes {
            break;
        }
        expanded += 1;
        closed.insert(pos);
        if at_goal(pos) {
            return Search {
                path: reconstruct(&came, start, pos),
                expanded,
                reached: true,
            };
        }
        for (next, step_cost) in neighbors(nav, pos) {
            let ng = entry.g + step_cost;
            if closed.contains(&next) {
                continue;
            }
            if ng + 1e-9 < g.get(&next).copied().unwrap_or(f64::INFINITY) {
                g.insert(next, ng);
                came.insert(next, pos);
                open.push(Frontier {
                    f: ng + heuristic(next, goal),
                    g: ng,
                    pos: next,
                });
            }
        }
    }

    Search {
        path: Vec::new(),
        expanded,
        reached: false,
    }
}

/// The walkable neighbours of a node: four cardinals plus the diagonals that
/// do not cut a corner, each with its euclidean step cost.
fn neighbors<N: Nav + ?Sized>(nav: &N, p: [i32; 3]) -> Vec<([i32; 3], f64)> {
    let mut out = Vec::with_capacity(8);
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        if let Some(n) = step(nav, p, dx, dz) {
            out.push((n, distance(p, n)));
        }
    }
    for (dx, dz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
        // No cutting corners: both shared cardinals must be standable here.
        if !standable(nav, p[0] + dx, p[1], p[2]) || !standable(nav, p[0], p[1], p[2] + dz) {
            continue;
        }
        if let Some(n) = step(nav, p, dx, dz) {
            out.push((n, distance(p, n)));
        }
    }
    out
}

/// The cell a mob reaches stepping `(dx, dz)` from `from`: the same level,
/// else one up, else the first standable drop within [`MAX_DROP`].
fn step<N: Nav + ?Sized>(nav: &N, from: [i32; 3], dx: i32, dz: i32) -> Option<[i32; 3]> {
    let (x, y, z) = (from[0], from[1], from[2]);
    let (nx, nz) = (x + dx, z + dz);
    if standable(nav, nx, y, nz) {
        return Some([nx, y, nz]);
    }
    if standable(nav, nx, y + 1, nz) {
        return Some([nx, y + 1, nz]);
    }
    for d in 1..=MAX_DROP {
        if standable(nav, nx, y - d, nz) {
            return Some([nx, y - d, nz]);
        }
    }
    None
}

fn distance(a: [i32; 3], b: [i32; 3]) -> f64 {
    let dx = (a[0] - b[0]) as f64;
    let dy = (a[1] - b[1]) as f64;
    let dz = (a[2] - b[2]) as f64;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Octile distance in the horizontal plane; the vertical is left out because
/// it is always an over-estimate to include and would break admissibility.
fn heuristic(a: [i32; 3], b: [i32; 3]) -> f64 {
    let dx = (a[0] - b[0]).abs() as f64;
    let dz = (a[2] - b[2]).abs() as f64;
    let (lo, hi) = if dx < dz { (dx, dz) } else { (dz, dx) };
    hi + (std::f64::consts::SQRT_2 - 1.0) * lo
}

/// Walk the came-from chain back to the start (the start itself is dropped).
fn reconstruct(
    came: &HashMap<[i32; 3], [i32; 3]>,
    start: [i32; 3],
    mut cur: [i32; 3],
) -> Vec<[i32; 3]> {
    let mut path = vec![cur];
    while cur != start {
        match came.get(&cur) {
            Some(&prev) => {
                cur = prev;
                path.push(cur);
            }
            None => break,
        }
    }
    path.pop();
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: [Box6; 1] = [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
    /// A bottom slab: the top half of the cell is open.
    const SLAB: [Box6; 1] = [[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]];
    const NONE: [Box6; 0] = [];

    /// A grid of full cubes over a floor. The mob's feet are at y = 1; the
    /// floor is y = 0 and walls are inserted at y = 1.
    struct Grid {
        solid: HashSet<[i32; 3]>,
        slabs: HashSet<[i32; 3]>,
        hazards: HashSet<[i32; 3]>,
    }

    impl Grid {
        fn floor(w: i32, d: i32) -> Self {
            let mut solid = HashSet::new();
            for x in 0..w {
                for z in 0..d {
                    solid.insert([x, 0, z]);
                }
            }
            Self {
                solid,
                slabs: HashSet::new(),
                hazards: HashSet::new(),
            }
        }

        /// A two-block-tall wall, which neither a step nor a drop climbs.
        fn wall(mut self, x: i32, z: i32) -> Self {
            self.solid.insert([x, 1, z]);
            self.solid.insert([x, 2, z]);
            self
        }

        /// A bottom slab — a step in the feet cell, an obstacle at head
        /// height.
        fn slab(mut self, x: i32, y: i32, z: i32) -> Self {
            self.slabs.insert([x, y, z]);
            self
        }

        /// An otherwise-solid cell that hurts to touch, like a cactus or a
        /// magma block — it still has a collision box, so it can carry.
        fn hazard(mut self, x: i32, y: i32, z: i32) -> Self {
            self.solid.insert([x, y, z]);
            self.hazards.insert([x, y, z]);
            self
        }
    }

    impl Nav for Grid {
        fn boxes(&self, x: i32, y: i32, z: i32) -> &'static [Box6] {
            if self.slabs.contains(&[x, y, z]) {
                &SLAB
            } else if self.solid.contains(&[x, y, z]) {
                &FULL
            } else {
                &NONE
            }
        }
        fn hazard(&self, x: i32, y: i32, z: i32) -> bool {
            self.hazards.contains(&[x, y, z])
        }
    }

    /// Every step of a path is a legal neighbour of the one before it.
    fn assert_contiguous(nav: &Grid, start: [i32; 3], path: &[[i32; 3]]) {
        let mut prev = start;
        for node in path {
            assert!(
                neighbors(nav, prev).iter().any(|(n, _)| *n == *node),
                "step {prev:?} -> {node:?} is not a neighbour"
            );
            assert!(standable(nav, node[0], node[1], node[2]));
            prev = *node;
        }
    }

    #[test]
    fn a_straight_corridor_is_solved() {
        let grid = Grid::floor(1, 5);
        let start = [0, 1, 0];
        let goal = [0, 1, 4];
        let r = search(&grid, start, goal, 0.0, 64);
        assert!(r.reached);
        assert_eq!(r.path.last(), Some(&goal));
        assert_eq!(r.path.len(), 4);
        assert_contiguous(&grid, start, &r.path);
    }

    #[test]
    fn it_routes_around_a_wall() {
        // A wall across x = 2 with a gap at z = 4.
        let mut grid = Grid::floor(5, 5);
        for z in 0..4 {
            grid = grid.wall(2, z);
        }
        let start = [0, 1, 2];
        let goal = [4, 1, 2];
        let r = search(&grid, start, goal, 0.0, 4096);
        assert!(r.reached);
        assert_eq!(r.path.last(), Some(&goal));
        // It has to leave the straight line and come back through the gap.
        assert!(r.path.iter().any(|n| n[2] == 4), "{:?}", r.path);
        assert!(r.path.len() > 4);
        assert_contiguous(&grid, start, &r.path);
    }

    #[test]
    fn an_unreachable_goal_gives_up() {
        // Seal the goal's four cardinals; corner-cutting is refused, so that
        // is enough to make it unreachable.
        let mut grid = Grid::floor(7, 7);
        for (x, z) in [(3, 3), (5, 3), (4, 2), (4, 4)] {
            grid = grid.wall(x, z);
        }
        let r = search(&grid, [0, 1, 0], [4, 1, 3], 0.0, 4096);
        assert!(!r.reached);
        assert!(r.path.is_empty());
    }

    #[test]
    fn a_one_block_step_is_climbed() {
        let mut grid = Grid::floor(6, 1);
        grid.solid.insert([2, 1, 0]);
        let r = search(&grid, [0, 1, 0], [4, 1, 0], 0.0, 4096);
        assert!(r.reached, "{:?}", r.path);
        assert!(r.path.iter().any(|n| n[1] == 2), "climbed the step");
        assert_contiguous(&grid, [0, 1, 0], &r.path);
    }

    #[test]
    fn a_drop_within_three_is_walked_off() {
        let mut grid = Grid::floor(6, 1);
        // A two-block ledge: the floor after x = 2 is two lower.
        grid.solid.remove(&[3, 0, 0]);
        grid.solid.remove(&[4, 0, 0]);
        grid.solid.remove(&[5, 0, 0]);
        grid.solid.insert([3, -2, 0]);
        grid.solid.insert([4, -2, 0]);
        grid.solid.insert([5, -2, 0]);
        let r = search(&grid, [0, 1, 0], [5, -1, 0], 0.0, 4096);
        assert!(r.reached, "{:?}", r.path);
        assert_eq!(r.path.last(), Some(&[5, -1, 0]));
        assert_contiguous(&grid, [0, 1, 0], &r.path);
    }

    #[test]
    fn the_node_budget_is_respected() {
        let grid = Grid::floor(64, 64);
        let r = search(&grid, [0, 1, 0], [63, 1, 63], 0.0, 5);
        assert!(r.expanded <= 5);
        assert!(!r.reached);
    }

    #[test]
    fn resolve_start_drops_onto_the_floor() {
        let grid = Grid::floor(3, 3);
        // Feet one above the floor: settle onto it.
        assert_eq!(resolve_start(&grid, [1, 2, 1]), Some([1, 1, 1]));
    }

    #[test]
    fn a_hazard_in_the_floor_is_not_standable() {
        // A cactus is only ever the support cell, under the mob. It has a
        // collision top (0.9375) so `carries` alone accepts it; the floor
        // hazard must reject the cell above it all the same.
        let grid = Grid::floor(5, 1).hazard(1, 0, 0);
        assert!(
            !standable(&grid, 1, 1, 0),
            "the cell standing on a cactus must not be walkable"
        );
        // The search cannot cross the cactus on a one-wide corridor.
        let r = search(&grid, [0, 1, 0], [4, 1, 0], 0.0, 4096);
        assert!(!r.reached, "a corridor blocked by a cactus is unreachable");
    }

    #[test]
    fn a_slab_at_head_height_blocks_the_cell() {
        // A bottom slab in the feet cell is a step the mob stands on.
        let stepped = Grid::floor(5, 1).slab(1, 1, 0);
        assert!(
            standable(&stepped, 1, 1, 0),
            "a bottom slab in the feet cell is a step"
        );
        // The same slab in the head cell is where the mob's body goes, so it
        // is a wall — its collision top (0.5) is below the step height, which
        // the old `blocking` test wrongly called free.
        let blocked = Grid::floor(5, 1).slab(1, 2, 0);
        assert!(
            !standable(&blocked, 1, 1, 0),
            "a head-height slab must not be walkable"
        );
        let r = search(&blocked, [0, 1, 0], [4, 1, 0], 0.0, 4096);
        assert!(!r.reached, "a head-height slab blocks a one-wide corridor");
    }
}
