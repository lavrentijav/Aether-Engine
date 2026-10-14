//! Per-*state* shape predicates, from the generated collision tables.
//!
//! Three vanilla predicates matter to redstone, and they are **not** the same:
//!
//! * `isRedstoneConductor` — the state's collision shape is a whole block. A
//!   conductor passes strong power on as weak power.
//! * `isFaceSturdy(dir, FULL)` — the state's shape covers the whole face in
//!   `dir`. Redstone wire attaches to and climbs a full face.
//! * `isFaceSturdy(dir, CENTER)` / `isFaceSturdy(dir, RIGID)` — weaker support
//!   tests. A standing torch uses CENTER (`BaseTorchBlock.canSurvive`); a
//!   repeater or comparator uses RIGID (`DiodeBlock.canSurviveOn`).
//!
//! # How vanilla computes a support face
//!
//! `VoxelShape.getFaceShape(direction)` takes the shape's **extreme slice** in
//! the direction's axis (the topmost 1/16 for UP), flattens the axis to `0..1`,
//! and keeps the footprint on the other two axes. `SupportType` then asks
//! whether that footprint **contains** a fixed region:
//!
//! * FULL — the whole unit square.
//! * CENTER — `Block.column(2, 0, 10)`, the central 2/16 × 2/16 column.
//! * RIGID — `Shapes.block() − Block.column(12, 0, 16)`, the outer 2/16 frame.
//!
//! So the extent along the direction's axis is irrelevant: a hopper's hollow top
//! rim is RIGID (the rim covers the frame) but not CENTER (the hollow centre is
//! not covered), and a fence's post is CENTER (it covers the central column) but
//! not FULL. The functions below read the per-state collision boxes and sample
//! the footprint on a 16×16 grid; all box coordinates are multiples of 1/16, so
//! the samples are exact.

use std::sync::OnceLock;

use aether_world::registry::{blocks, shapes};
use aether_world::BlockStateId;

use crate::dir::Direction;

/// A small tolerance for the `f32` shape coordinates.
const EPS: f32 = 1.0e-4;

/// `isCollisionShapeFullBlock`: the state's collision shape is exactly one
/// whole block.
pub fn full_collision(state: BlockStateId) -> bool {
    let boxes = shapes::collision_boxes(state.raw());
    boxes.len() == 1 && boxes[0] == [0.0, 0.0, 0.0, 1.0, 1.0, 1.0]
}

/// The two axes perpendicular to `direction`, as indices into a box.
fn perpendicular(direction: Direction) -> (usize, usize) {
    match direction {
        Direction::Up | Direction::Down => (0, 2),
        Direction::West | Direction::East => (1, 2),
        Direction::North | Direction::South => (0, 1),
    }
}

/// The axis a direction runs along.
fn axis(direction: Direction) -> usize {
    match direction {
        Direction::Up | Direction::Down => 1,
        Direction::West | Direction::East => 0,
        Direction::North | Direction::South => 2,
    }
}

/// The boxes making up the shape's extreme slice in `direction`
/// (`VoxelShape.getFaceShape` before the footprint test).
fn face_slice(state: BlockStateId, direction: Direction) -> Vec<&'static [f32; 6]> {
    let boxes = shapes::collision_boxes(state.raw());
    let axis = axis(direction);
    let positive = matches!(
        direction,
        Direction::Up | Direction::East | Direction::South
    );
    let coord = move |b: &[f32; 6]| if positive { b[axis + 3] } else { b[axis] };
    let Some(extreme) = boxes
        .iter()
        .map(coord)
        .reduce(|a, b| if positive { a.max(b) } else { a.min(b) })
    else {
        return Vec::new();
    };
    boxes
        .iter()
        .filter(|b| (coord(b) - extreme).abs() < EPS)
        .collect()
}

/// Whether the face slice covers every sample point `required` selects.
fn slice_covers(
    slice: &[&[f32; 6]],
    direction: Direction,
    required: impl Fn(f32, f32) -> bool,
) -> bool {
    if slice.is_empty() {
        return false;
    }
    let (a1, a2) = perpendicular(direction);
    const N: i32 = 16;
    for i in 0..N {
        for j in 0..N {
            let u = (i as f32 + 0.5) / N as f32;
            let v = (j as f32 + 0.5) / N as f32;
            if !required(u, v) {
                continue;
            }
            let covered = slice.iter().any(|b| {
                u >= b[a1] - EPS && u <= b[a1 + 3] + EPS && v >= b[a2] - EPS && v <= b[a2 + 3] + EPS
            });
            if !covered {
                return false;
            }
        }
    }
    true
}

/// `SupportType.FULL`: the footprint covers the whole face.
pub fn face_full(state: BlockStateId, direction: Direction) -> bool {
    slice_covers(&face_slice(state, direction), direction, |_, _| true)
}

/// `SupportType.CENTER`: the footprint covers the central 2/16 × 2/16 column.
pub fn face_center(state: BlockStateId, direction: Direction) -> bool {
    slice_covers(&face_slice(state, direction), direction, |u, v| {
        (7.0 / 16.0..=9.0 / 16.0).contains(&u) && (7.0 / 16.0..=9.0 / 16.0).contains(&v)
    })
}

/// `SupportType.RIGID`: the footprint covers the outer 2/16 frame.
pub fn face_rigid(state: BlockStateId, direction: Direction) -> bool {
    slice_covers(&face_slice(state, direction), direction, |u, v| {
        !(2.0 / 16.0..=14.0 / 16.0).contains(&u) || !(2.0 / 16.0..=14.0 / 16.0).contains(&v)
    })
}

/// `Block.canSupportCenter(state, direction)`: a standing torch's support.
pub fn can_support_center(state: BlockStateId, direction: Direction) -> bool {
    face_center(state, direction)
}

/// The hopper block id, resolved once. `RedStoneWireBlock.canSurviveOn`
/// special-cases a hopper because its top face is not full.
pub fn is_hopper(state: BlockStateId) -> bool {
    static HOPPER: OnceLock<Option<u16>> = OnceLock::new();
    let hopper = HOPPER.get_or_init(|| blocks::block_id_of("minecraft:hopper"));
    blocks::block_of_state(state).map(|(b, _)| b) == *hopper
}

#[cfg(test)]
mod tests {
    use aether_world::BlockRegistry;

    use super::*;

    fn s(name: &str) -> BlockStateId {
        BlockRegistry::new().get(name).unwrap()
    }

    #[test]
    fn a_cube_is_a_conductor_and_sturdy_on_every_face() {
        let stone = s("minecraft:stone");
        assert!(full_collision(stone));
        for d in Direction::ALL {
            assert!(face_full(stone, d), "FULL {d:?}");
            assert!(face_center(stone, d), "CENTER {d:?}");
            assert!(face_rigid(stone, d), "RIGID {d:?}");
        }
    }

    #[test]
    fn a_bottom_slab_is_sturdy_on_top_and_bottom_but_not_its_sides() {
        // A bottom slab's top face at y = 0.5 is a full unit square, so wire
        // climbs it; its west face is half-height, so it is not FULL. It is not
        // a conductor.
        let slab = s("minecraft:oak_slab");
        assert!(!full_collision(slab));
        assert!(face_full(slab, Direction::Up));
        assert!(face_full(slab, Direction::Down));
        assert!(!face_full(slab, Direction::West));
        assert!(!face_full(slab, Direction::East));
    }

    #[test]
    fn a_double_slab_is_a_conductor() {
        // `aether-world`'s per-block `solid` flag cannot tell this from the
        // single slab; the per-state shape can.
        let double = s("minecraft:oak_slab[type=double]");
        assert!(full_collision(double));
    }

    #[test]
    fn a_hopper_is_not_sturdy_on_top() {
        let hopper = s("minecraft:hopper");
        assert!(is_hopper(hopper));
        assert!(!face_full(hopper, Direction::Up));
    }

    #[test]
    fn a_hopper_is_rigid_but_not_centre_on_top() {
        // D3/N1/N2: the hopper's top slice is the outer rim ring. The rim
        // covers the RIGID frame but leaves the centre hollow, so a repeater
        // (RIGID) stands on it while a torch (CENTER) does not.
        let hopper = s("minecraft:hopper");
        assert!(face_rigid(hopper, Direction::Up), "RIGID");
        assert!(!face_center(hopper, Direction::Up), "CENTER");
    }

    #[test]
    fn a_fence_post_is_centre_but_not_rigid_or_full() {
        // The post covers the central column (a torch stands) but not the outer
        // frame, so it is not RIGID (a repeater pops) and not FULL.
        let fence = s("minecraft:oak_fence");
        assert!(face_center(fence, Direction::Up));
        assert!(!face_rigid(fence, Direction::Up));
        assert!(!face_full(fence, Direction::Up));
    }

    #[test]
    fn air_supports_nothing() {
        let air = s("minecraft:air");
        assert!(!face_full(air, Direction::Up));
        assert!(!face_center(air, Direction::Up));
        assert!(!face_rigid(air, Direction::Up));
    }
}
