//! # aether-physics
//!
//! Basic voxel physics: **collision** (swept AABB against solid blocks),
//! **movement** resolution per axis, and a simple **gravity + friction** step.
//!
//! The physics layer never touches storage directly — it reads the world
//! through the [`BlockView`] trait (`is_solid(x, y, z)`), so it can run against
//! the real engine world, a generated chunk, or a hand-built test fixture.
//!
//! The collision algorithm mirrors the classic Minecraft approach: gather the
//! solid unit-cubes overlapping the swept region, then clip the motion one axis
//! at a time (Y, then X, then Z), moving the box after each axis.

use aether_core::math::{Aabb, Vec3};

/// Read-only access to block solidity for collision.
pub trait BlockView {
    /// Whether the block at integer coordinate `(x, y, z)` is a full collision
    /// cube. Out-of-world coordinates should return `false` (open air).
    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool;
}

/// Outcome of a [`collide`] call: the resolved box and which axes were blocked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveResult {
    /// The box after collision resolution.
    pub aabb: Aabb,
    /// Motion on X was clipped by a block.
    pub collided_x: bool,
    /// Motion on Y was clipped by a block.
    pub collided_y: bool,
    /// Motion on Z was clipped by a block.
    pub collided_z: bool,
    /// The box came to rest on top of a block this step.
    pub on_ground: bool,
}

// Motion smaller than this is treated as "fully applied" when flagging collisions.
const EPS: f64 = 1.0e-7;

fn clip_y(block: Aabb, e: Aabb, dy: f64) -> f64 {
    if e.max.x <= block.min.x || e.min.x >= block.max.x {
        return dy;
    }
    if e.max.z <= block.min.z || e.min.z >= block.max.z {
        return dy;
    }
    if dy > 0.0 && e.max.y <= block.min.y {
        let d = block.min.y - e.max.y;
        if d < dy {
            return d;
        }
    } else if dy < 0.0 && e.min.y >= block.max.y {
        let d = block.max.y - e.min.y;
        if d > dy {
            return d;
        }
    }
    dy
}

fn clip_x(block: Aabb, e: Aabb, dx: f64) -> f64 {
    if e.max.y <= block.min.y || e.min.y >= block.max.y {
        return dx;
    }
    if e.max.z <= block.min.z || e.min.z >= block.max.z {
        return dx;
    }
    if dx > 0.0 && e.max.x <= block.min.x {
        let d = block.min.x - e.max.x;
        if d < dx {
            return d;
        }
    } else if dx < 0.0 && e.min.x >= block.max.x {
        let d = block.max.x - e.min.x;
        if d > dx {
            return d;
        }
    }
    dx
}

fn clip_z(block: Aabb, e: Aabb, dz: f64) -> f64 {
    if e.max.y <= block.min.y || e.min.y >= block.max.y {
        return dz;
    }
    if e.max.x <= block.min.x || e.min.x >= block.max.x {
        return dz;
    }
    if dz > 0.0 && e.max.z <= block.min.z {
        let d = block.min.z - e.max.z;
        if d < dz {
            return d;
        }
    } else if dz < 0.0 && e.min.z >= block.max.z {
        let d = block.max.z - e.min.z;
        if d > dz {
            return d;
        }
    }
    dz
}

// Defense-in-depth: cap how far a single step may sweep on any axis, so an
// abnormally large `motion` (an upstream bug, or network-driven movement later)
// can't turn the block scan into an effectively unbounded loop that hangs a
// tick thread. A tick never legitimately moves this far.
const MAX_SWEEP_BLOCKS: i32 = 256;

/// Collect the solid block boxes overlapping the region swept by `aabb + motion`.
fn solid_boxes<V: BlockView>(view: &V, swept: Aabb) -> Vec<Aabb> {
    let x0 = swept.min.x.floor() as i32;
    let y0 = swept.min.y.floor() as i32;
    let z0 = swept.min.z.floor() as i32;
    let x1 = ((swept.max.x - EPS).floor() as i32).min(x0.saturating_add(MAX_SWEEP_BLOCKS));
    let y1 = ((swept.max.y - EPS).floor() as i32).min(y0.saturating_add(MAX_SWEEP_BLOCKS));
    let z1 = ((swept.max.z - EPS).floor() as i32).min(z0.saturating_add(MAX_SWEEP_BLOCKS));

    let mut boxes = Vec::new();
    for y in y0..=y1 {
        for z in z0..=z1 {
            for x in x0..=x1 {
                if view.is_solid(x, y, z) {
                    boxes.push(Aabb::block(x, y, z));
                }
            }
        }
    }
    boxes
}

/// Move `aabb` by `motion`, resolving collisions against solid blocks in `view`.
pub fn collide<V: BlockView>(view: &V, aabb: Aabb, motion: Vec3) -> MoveResult {
    let boxes = solid_boxes(view, aabb.expand(motion));
    let mut current = aabb;

    // Y first, so landing on a floor is detected, then X, then Z.
    let mut dy = motion.y;
    for b in &boxes {
        dy = clip_y(*b, current, dy);
    }
    current = current.offset(Vec3::new(0.0, dy, 0.0));

    let mut dx = motion.x;
    for b in &boxes {
        dx = clip_x(*b, current, dx);
    }
    current = current.offset(Vec3::new(dx, 0.0, 0.0));

    let mut dz = motion.z;
    for b in &boxes {
        dz = clip_z(*b, current, dz);
    }
    current = current.offset(Vec3::new(0.0, 0.0, dz));

    let collided_x = (dx - motion.x).abs() > EPS;
    let collided_y = (dy - motion.y).abs() > EPS;
    let collided_z = (dz - motion.z).abs() > EPS;
    MoveResult {
        aabb: current,
        collided_x,
        collided_y,
        collided_z,
        on_ground: motion.y < 0.0 && collided_y,
    }
}

/// Tunable constants for [`step`]. Defaults are Minecraft-ish, not exact.
#[derive(Debug, Clone, Copy)]
pub struct PhysicsParams {
    /// Downward acceleration applied each tick (blocks/tick²).
    pub gravity: f64,
    /// Multiplicative vertical drag applied each tick.
    pub vertical_drag: f64,
    /// Horizontal multiplier while standing on ground (lower = more friction).
    pub ground_friction: f64,
    /// Horizontal multiplier while airborne.
    pub air_friction: f64,
}

impl Default for PhysicsParams {
    fn default() -> Self {
        Self {
            gravity: 0.08,
            vertical_drag: 0.98,
            ground_friction: 0.546, // ~0.6 slipperiness * 0.91 air term
            air_friction: 0.91,
        }
    }
}

/// A physical body: its bounding box plus velocity and ground state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    /// Collision box in world space.
    pub aabb: Aabb,
    /// Velocity in blocks/tick.
    pub velocity: Vec3,
    /// Whether the body is resting on a block.
    pub on_ground: bool,
}

impl Body {
    /// A body with a player-sized box (0.6 × 1.8) whose feet are at `feet`.
    pub fn player(feet: Vec3) -> Self {
        Self {
            aabb: Aabb::from_base(feet, 0.6, 1.8),
            velocity: Vec3::ZERO,
            on_ground: false,
        }
    }

    /// The world position of the box's base centre (the entity's "feet").
    pub fn feet(&self) -> Vec3 {
        Vec3::new(
            (self.aabb.min.x + self.aabb.max.x) / 2.0,
            self.aabb.min.y,
            (self.aabb.min.z + self.aabb.max.z) / 2.0,
        )
    }
}

/// Advance `body` one tick: apply gravity, move-and-collide, then friction.
pub fn step<V: BlockView>(view: &V, body: &mut Body, params: PhysicsParams) {
    body.velocity.y -= params.gravity;

    let res = collide(view, body.aabb, body.velocity);
    body.aabb = res.aabb;
    body.on_ground = res.on_ground;

    if res.collided_x {
        body.velocity.x = 0.0;
    }
    if res.collided_y {
        body.velocity.y = 0.0;
    }
    if res.collided_z {
        body.velocity.z = 0.0;
    }

    let horizontal = if body.on_ground {
        params.ground_friction
    } else {
        params.air_friction
    };
    body.velocity.x *= horizontal;
    body.velocity.z *= horizontal;
    body.velocity.y *= params.vertical_drag;
}

/// An event-driven cache of an entity's support state (spec §7.3).
///
/// Recomputing "what am I standing on?" every tick is wasted work for the many
/// entities that are just standing still. [`CachedEnvironment`] remembers the
/// feet cell and floor height an entity last rested on; while the entity stays
/// in that cell and nothing nearby changes, [`step_cached`] advances it in
/// **O(1)** with **zero** block queries.
///
/// The cache is invalidated automatically when the entity leaves its cell, and
/// must be invalidated explicitly with [`CachedEnvironment::invalidate`] when a
/// nearby block changes (a `BlockChangeEvent`) — otherwise a removed floor would
/// go unnoticed.
#[derive(Debug, Clone, Copy, Default)]
pub struct CachedEnvironment {
    /// The feet block `(fx, fy, fz)` the cache is valid for, if any.
    cell: Option<(i32, i32, i32)>,
    /// Cached feet height while resting.
    ground_y: f64,
}

// Below this per-axis horizontal speed an entity counts as "at rest" and is
// eligible for the cached fast path.
const REST_EPS: f64 = 1.0e-4;

impl CachedEnvironment {
    /// A fresh, empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the cached support — call this when a block near the entity changes.
    pub fn invalidate(&mut self) {
        self.cell = None;
    }

    /// Whether the cache currently holds a valid support state.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.cell.is_some()
    }

    /// The feet block an entity currently occupies.
    #[inline]
    fn feet_cell(body: &Body) -> (i32, i32, i32) {
        let f = body.feet();
        (f.x.floor() as i32, f.y.floor() as i32, f.z.floor() as i32)
    }

    #[inline]
    fn at_rest(body: &Body) -> bool {
        body.on_ground && body.velocity.x.abs() < REST_EPS && body.velocity.z.abs() < REST_EPS
    }
}

/// Advance `body` one tick using the [`CachedEnvironment`] fast path when it can.
///
/// Returns `true` if the O(1) cached path was taken (no block queries), or
/// `false` if it fell back to a full [`step`] (which also refreshes the cache).
///
/// The fast path applies only to an entity that is on the ground, essentially
/// not moving horizontally, and still in the cell the cache was built for — the
/// "standing or slowly shuffling" case §7.3 targets. Everything else takes the
/// full swept-collision path.
pub fn step_cached<V: BlockView>(
    view: &V,
    body: &mut Body,
    params: PhysicsParams,
    env: &mut CachedEnvironment,
) -> bool {
    let cell = CachedEnvironment::feet_cell(body);
    if env.cell == Some(cell) && CachedEnvironment::at_rest(body) {
        // Fast path: the entity is resting on known ground. Gravity is fully
        // absorbed by the floor, so it stays put; only friction is applied.
        // No `is_solid` calls at all.
        body.velocity.y = 0.0;
        body.velocity.x *= params.ground_friction;
        body.velocity.z *= params.ground_friction;
        body.on_ground = true;
        // Keep the feet pinned to the cached surface (guards fp drift).
        let drop = body.feet().y - env.ground_y;
        if drop != 0.0 {
            body.aabb = body.aabb.offset(Vec3::new(0.0, -drop, 0.0));
        }
        return true;
    }

    // Slow path: a full swept-collision step, then refresh the cache.
    step(view, body, params);
    if CachedEnvironment::at_rest(body) {
        env.cell = Some(CachedEnvironment::feet_cell(body));
        env.ground_y = body.feet().y;
    } else {
        env.invalidate();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A flat solid floor at y < `floor`, air elsewhere.
    struct Floor {
        floor: i32,
    }
    impl BlockView for Floor {
        fn is_solid(&self, _x: i32, y: i32, _z: i32) -> bool {
            y < self.floor
        }
    }

    /// A single wall block at a given coordinate.
    struct Wall {
        wx: i32,
        wy: i32,
        wz: i32,
    }
    impl BlockView for Wall {
        fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
            x == self.wx && y == self.wy && z == self.wz
        }
    }

    #[test]
    fn falls_and_lands_on_floor() {
        // Floor fills y<64, so the top surface is y=64.
        let view = Floor { floor: 64 };
        let mut body = Body::player(Vec3::new(0.5, 70.0, 0.5));
        for _ in 0..200 {
            step(&view, &mut body, PhysicsParams::default());
        }
        assert!(body.on_ground, "should have landed");
        // Feet should rest on the floor surface at y=64.
        assert!(
            (body.feet().y - 64.0).abs() < 1.0e-6,
            "feet at {}",
            body.feet().y
        );
        assert!(
            body.velocity.y.abs() < 1.0e-6,
            "vertical velocity not cleared"
        );
    }

    #[test]
    fn horizontal_motion_stops_at_wall() {
        // Wall cube occupying x=1,y=64,z=0.
        let view = Wall {
            wx: 1,
            wy: 64,
            wz: 0,
        };
        // Box hugging x in [0.0,0.6] at y=64, moving +x by 2.0.
        let aabb = Aabb::from_base(Vec3::new(0.3, 64.0, 0.3), 0.6, 1.8);
        let res = collide(&view, aabb, Vec3::new(2.0, 0.0, 0.0));
        assert!(res.collided_x);
        // Right face should stop at the wall's min x = 1.0.
        assert!(
            (res.aabb.max.x - 1.0).abs() < 1.0e-9,
            "max.x = {}",
            res.aabb.max.x
        );
    }

    #[test]
    fn no_obstacle_moves_freely() {
        struct Empty;
        impl BlockView for Empty {
            fn is_solid(&self, _: i32, _: i32, _: i32) -> bool {
                false
            }
        }
        let aabb = Aabb::from_base(Vec3::new(0.0, 64.0, 0.0), 0.6, 1.8);
        let res = collide(&Empty, aabb, Vec3::new(1.5, -2.0, 0.25));
        assert!(!res.collided_x && !res.collided_y && !res.collided_z);
        assert_eq!(res.aabb, aabb.offset(Vec3::new(1.5, -2.0, 0.25)));
    }

    #[test]
    fn standing_on_floor_keeps_feet_put() {
        let view = Floor { floor: 64 };
        let mut body = Body::player(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..20 {
            step(&view, &mut body, PhysicsParams::default());
        }
        assert!(body.on_ground);
        assert!((body.feet().y - 64.0).abs() < 1.0e-6);
    }

    /// A `BlockView` wrapper that counts `is_solid` calls.
    struct Counting<'a, V: BlockView> {
        inner: &'a V,
        calls: Cell<u32>,
    }
    impl<'a, V: BlockView> Counting<'a, V> {
        fn new(inner: &'a V) -> Self {
            Self {
                inner,
                calls: Cell::new(0),
            }
        }
    }
    impl<V: BlockView> BlockView for Counting<'_, V> {
        fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
            self.calls.set(self.calls.get() + 1);
            self.inner.is_solid(x, y, z)
        }
    }

    #[test]
    fn cached_environment_skips_block_queries_at_rest() {
        let floor = Floor { floor: 64 };
        let view = Counting::new(&floor);
        let mut body = Body::player(Vec3::new(0.5, 70.0, 0.5));
        let mut env = CachedEnvironment::new();

        // Let it fall and settle; these steps use the slow path (block scans).
        for _ in 0..200 {
            step_cached(&view, &mut body, PhysicsParams::default(), &mut env);
        }
        assert!(body.on_ground);
        assert!(env.is_valid());

        // Now that it's resting, further steps must take the O(1) fast path with
        // zero block queries.
        let before = view.calls.get();
        for _ in 0..100 {
            let fast = step_cached(&view, &mut body, PhysicsParams::default(), &mut env);
            assert!(fast, "expected the cached fast path while resting");
        }
        assert_eq!(view.calls.get(), before, "fast path queried blocks");
        assert!((body.feet().y - 64.0).abs() < 1.0e-6);
        assert!(body.on_ground);
    }

    #[test]
    fn cached_step_matches_plain_step_while_falling() {
        let floor = Floor { floor: 0 };
        let mut a = Body::player(Vec3::new(0.5, 50.0, 0.5));
        let mut b = a;
        let mut env = CachedEnvironment::new();
        // While airborne the cached step must behave exactly like the plain one.
        for _ in 0..30 {
            step(&floor, &mut a, PhysicsParams::default());
            let fast = step_cached(&floor, &mut b, PhysicsParams::default(), &mut env);
            assert!(!fast, "should not fast-path while falling");
            assert_eq!(a.aabb, b.aabb);
            assert_eq!(a.velocity, b.velocity);
        }
    }

    #[test]
    fn invalidate_forces_a_rescan() {
        let floor = Floor { floor: 64 };
        let view = Counting::new(&floor);
        let mut body = Body::player(Vec3::new(0.5, 66.0, 0.5));
        let mut env = CachedEnvironment::new();
        for _ in 0..200 {
            step_cached(&view, &mut body, PhysicsParams::default(), &mut env);
        }
        assert!(env.is_valid());
        // A nearby block changed: the world invalidates the cache.
        env.invalidate();
        assert!(!env.is_valid());
        let before = view.calls.get();
        let fast = step_cached(&view, &mut body, PhysicsParams::default(), &mut env);
        assert!(!fast, "invalidated cache must take the slow path");
        assert!(view.calls.get() > before, "slow path should query blocks");
        // ...and it re-validates once resting again.
        assert!(env.is_valid());
    }
}
