//! # aether-physics
//!
//! Basic voxel physics: **collision** (swept AABB against solid blocks),
//! **movement** resolution per axis, and a simple **gravity + friction** step.
//!
//! The physics layer never touches storage directly — it reads the world
//! through the [`BlockView`] trait, so it can run against the real engine
//! world, a generated chunk, or a hand-built test fixture.
//!
//! The collision algorithm mirrors the classic Minecraft approach: gather the
//! collision boxes overlapping the swept region, then clip the motion one axis
//! at a time (Y, then X, then Z), moving the box after each axis. A block
//! contributes as many boxes as its **state** says: a slab one half-height
//! box, a fence a handful of 1.5-tall ones, an open door or gate none at all.

use aether_core::math::{Aabb, Vec3};

/// Read-only access to block collision for movement.
pub trait BlockView {
    /// Whether the block at integer coordinate `(x, y, z)` is a full collision
    /// cube. Out-of-world coordinates should return `false` (open air).
    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool;

    /// Append the collision boxes of the block at `(x, y, z)`, in **world**
    /// coordinates, to `out`.
    ///
    /// The default is the coarse answer — one unit cube for a solid block,
    /// nothing otherwise — which keeps a plain [`BlockView`] a valid physics
    /// view. A state-aware view overrides it to return the real boxes (slab
    /// halves, fence rails, the empty shape of an open gate). A block with no
    /// boxes is passable, whatever its `is_solid` says.
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.is_solid(x, y, z) {
            out.push(Aabb::block(x, y, z));
        }
    }
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

/// How far a block's collision box may reach outside its own cell, over every
/// shape in the vanilla tables: a fence or wall is 1.5 tall, some shapes dip to
/// −0.25.
const MAX_PROTRUSION: f64 = 0.5;

/// Collect the collision boxes overlapping the region swept by `aabb + motion`.
///
/// The swept hull is grown by [`MAX_PROTRUSION`] before its cells are taken, so
/// a box that sticks out of its cell is still found. Without it a body resting
/// exactly on a non-integer top — a fence at 1.5 — would sweep a region that
/// starts in the cell *above* the block holding it up, and fall straight
/// through it on the next tick.
fn block_boxes<V: BlockView>(view: &V, swept: Aabb) -> Vec<Aabb> {
    let x0 = (swept.min.x - MAX_PROTRUSION).floor() as i32;
    let y0 = (swept.min.y - MAX_PROTRUSION).floor() as i32;
    let z0 = (swept.min.z - MAX_PROTRUSION).floor() as i32;
    let x1 = (swept.max.x + MAX_PROTRUSION - EPS).floor() as i32;
    let y1 = (swept.max.y + MAX_PROTRUSION - EPS).floor() as i32;
    let z1 = (swept.max.z + MAX_PROTRUSION - EPS).floor() as i32;
    let x1 = x1.min(x0.saturating_add(MAX_SWEEP_BLOCKS));
    let y1 = y1.min(y0.saturating_add(MAX_SWEEP_BLOCKS));
    let z1 = z1.min(z0.saturating_add(MAX_SWEEP_BLOCKS));

    let mut boxes = Vec::new();
    for y in y0..=y1 {
        for z in z0..=z1 {
            for x in x0..=x1 {
                view.collision_boxes(x, y, z, &mut boxes);
            }
        }
    }
    boxes
}

/// Move `aabb` by `motion`, resolving collisions against the blocks in `view`.
pub fn collide<V: BlockView>(view: &V, aabb: Aabb, motion: Vec3) -> MoveResult {
    let boxes = block_boxes(view, aabb.expand(motion));
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
    /// Highest obstacle a grounded body auto-steps over when walking into it,
    /// vanilla's `maxUpStep` (a living entity is 0.6, an item is 0).
    pub step_height: f64,
}

impl Default for PhysicsParams {
    fn default() -> Self {
        Self {
            gravity: 0.08,
            vertical_drag: 0.98,
            ground_friction: 0.546, // ~0.6 slipperiness * 0.91 air term
            air_friction: 0.91,
            step_height: 0.6,
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

    /// A body with an arbitrary box, `width` square and `height` tall, whose
    /// base centre is at `feet`.
    ///
    /// A dropped item is 0.25 on a side, not 0.6 by 1.8, and the difference
    /// shows: a player-sized box around an item cannot fall into a one-block
    /// hole.
    pub fn sized(feet: Vec3, width: f64, height: f64) -> Self {
        Self {
            aabb: Aabb::from_base(feet, width, height),
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

/// The horizontal displacement between two boxes' centres.
fn horizontal_distance(from: Aabb, to: Aabb) -> f64 {
    let dx = (to.min.x + to.max.x) - (from.min.x + from.max.x);
    let dz = (to.min.z + to.max.z) - (from.min.z + from.max.z);
    (dx * dx + dz * dz).sqrt() / 2.0
}

/// Resolve one move, stepping up a low obstacle the way a grounded walking
/// entity does.
///
/// The plain move is tried first. If it was cut short horizontally and the
/// body was on the ground, the same move is retried from `step_height` higher:
/// if that gets the body further horizontally it is adopted, and the body
/// settles back down onto whatever it stepped onto. This is how a slab (0.5),
/// a carpet or a single rail is climbed without jumping, while a full block
/// (1.0) still stops the body — 1.0 is taller than any `step_height` a walking
/// entity has.
fn collide_stepped<V: BlockView>(
    view: &V,
    aabb: Aabb,
    motion: Vec3,
    step_height: f64,
    on_ground: bool,
) -> MoveResult {
    let res = collide(view, aabb, motion);
    let want = Vec3::new(motion.x, 0.0, motion.z);
    if !on_ground || step_height <= 0.0 || want.length_squared() <= EPS {
        return res;
    }
    let base = horizontal_distance(aabb, res.aabb);
    if base >= want.length() - EPS {
        return res; // nothing was blocked horizontally
    }

    // Raise, cross at that height, then drop back down.
    let raised = collide(view, aabb, Vec3::new(0.0, step_height, 0.0));
    let up = raised.aabb.min.y - aabb.min.y;
    if up <= EPS {
        return res; // a ceiling lower than the step
    }
    let across = collide(view, raised.aabb, want);
    if horizontal_distance(aabb, across.aabb) <= base + EPS {
        return res; // crossing at step height bought nothing
    }
    let dropped = collide(view, across.aabb, Vec3::new(0.0, -up, 0.0));
    MoveResult {
        aabb: dropped.aabb,
        collided_x: across.collided_x,
        collided_z: across.collided_z,
        collided_y: dropped.collided_y,
        on_ground: dropped.collided_y || across.on_ground,
    }
}

/// Advance `body` one tick: apply gravity, move-and-collide, then friction.
pub fn step<V: BlockView>(view: &V, body: &mut Body, params: PhysicsParams) {
    body.velocity.y -= params.gravity;

    let res = collide_stepped(
        view,
        body.aabb,
        body.velocity,
        params.step_height,
        body.on_ground,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A flat floor at `y < 64` with a few blocks, each carrying an explicit
    /// list of block-local collision boxes (an empty list is passable).
    struct Field {
        blocks: Vec<((i32, i32, i32), Vec<[f64; 6]>)>,
    }
    impl Field {
        fn with(blocks: Vec<((i32, i32, i32), Vec<[f64; 6]>)>) -> Self {
            Self { blocks }
        }
        fn boxes_at(&self, x: i32, y: i32, z: i32) -> Option<&Vec<[f64; 6]>> {
            self.blocks
                .iter()
                .find(|(p, _)| *p == (x, y, z))
                .map(|(_, b)| b)
        }
    }
    impl BlockView for Field {
        fn is_solid(&self, _x: i32, y: i32, _z: i32) -> bool {
            y < 64
        }
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if y < 64 {
                out.push(Aabb::block(x, y, z));
            }
            if let Some(boxes) = self.boxes_at(x, y, z) {
                let (fx, fy, fz) = (x as f64, y as f64, z as f64);
                for b in boxes {
                    out.push(Aabb::new(
                        Vec3::new(fx + b[0], fy + b[1], fz + b[2]),
                        Vec3::new(fx + b[3], fy + b[4], fz + b[5]),
                    ));
                }
            }
        }
    }

    /// A bottom slab: the lower half of the cell.
    fn slab_box() -> Vec<[f64; 6]> {
        vec![[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]]
    }

    #[test]
    fn an_entity_walks_up_a_slab() {
        // A long run of bottom slabs one block east of the spawn.
        let field = Field::with(
            (1..=4)
                .map(|x| ((x, 64, 0), slab_box()))
                .collect::<Vec<_>>(),
        );
        let mut body = Body::player(Vec3::new(0.5, 64.0, 0.5));
        // Settle onto the floor first.
        for _ in 0..5 {
            step(&field, &mut body, PhysicsParams::default());
        }
        assert!(body.on_ground);
        // Walk east at a steady pace with no jump input.
        for _ in 0..25 {
            body.velocity.x = 0.12;
            step(&field, &mut body, PhysicsParams::default());
        }
        // It climbed the 0.5 slab instead of being stopped by it, and is
        // standing on top of the run.
        assert!(
            (body.feet().y - 64.5).abs() < 1.0e-6,
            "feet at {}, expected the slab top 64.5",
            body.feet().y
        );
        assert!(
            (2.0..4.5).contains(&body.feet().x),
            "did not walk on: x {}",
            body.feet().x
        );
        assert!(body.on_ground);
    }

    #[test]
    fn an_entity_cannot_walk_up_a_full_block() {
        // The same run, but full cubes: 0.6 step height cannot climb 1.0.
        let field = Field::with(
            (1..=4)
                .map(|x| ((x, 64, 0), vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]))
                .collect::<Vec<_>>(),
        );
        let mut body = Body::player(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            step(&field, &mut body, PhysicsParams::default());
        }
        for _ in 0..40 {
            body.velocity.x = 0.12;
            step(&field, &mut body, PhysicsParams::default());
        }
        assert!(
            body.feet().y < 64.1,
            "should still be on the floor, feet at {}",
            body.feet().y
        );
        assert!(body.feet().x < 1.0, "should not have entered the block");
    }

    #[test]
    fn an_entity_stands_on_a_fence_at_1_5() {
        // A fence post: a 1.5-tall shape in the middle of its cell.
        let field = Field::with(vec![(
            (0, 64, 0),
            vec![[0.375, 0.0, 0.375, 0.625, 1.5, 0.625]],
        )]);
        let mut body = Body::player(Vec3::new(0.5, 66.0, 0.5));
        for _ in 0..100 {
            step(&field, &mut body, PhysicsParams::default());
        }
        assert!(body.on_ground);
        assert!(
            (body.feet().y - 65.5).abs() < 1.0e-6,
            "feet at {}, expected the fence top 65.5",
            body.feet().y
        );
    }

    #[test]
    fn an_entity_passes_through_an_open_gate() {
        // The gate's cell is passable: an open gate contributes no boxes.
        let open = Field::with(vec![((0, 64, 0), vec![])]);
        let mut body = Body::player(Vec3::new(0.5, 64.0, -0.5));
        for _ in 0..5 {
            step(&open, &mut body, PhysicsParams::default());
        }
        for _ in 0..40 {
            body.velocity.z = 0.12;
            step(&open, &mut body, PhysicsParams::default());
        }
        assert!(
            body.feet().z > 1.0,
            "an open gate must not stop the body, z {}",
            body.feet().z
        );
    }

    #[test]
    fn a_closed_gate_stops_an_entity() {
        let closed = Field::with(vec![((0, 64, 0), vec![[0.0, 0.0, 0.0, 1.0, 1.5, 1.0]])]);
        let mut body = Body::player(Vec3::new(0.5, 64.0, -0.5));
        for _ in 0..5 {
            step(&closed, &mut body, PhysicsParams::default());
        }
        for _ in 0..40 {
            body.velocity.z = 0.12;
            step(&closed, &mut body, PhysicsParams::default());
        }
        assert!(
            body.feet().z < 0.0,
            "a closed gate must stop the body, z {}",
            body.feet().z
        );
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
}
