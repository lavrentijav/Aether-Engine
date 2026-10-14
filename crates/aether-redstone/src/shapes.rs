//! Per-*state* shape predicates, from the generated collision tables.
//!
//! Two vanilla predicates drive redstone connectivity and they are **not** the
//! same:
//!
//! * `isRedstoneConductor` — the state's collision shape is a whole block. A
//!   conductor passes strong power on as weak power.
//! * `isFaceSturdy(dir, FULL)` — the state's collision shape covers the whole
//!   face in `dir`. This is what redstone wire attaches to and climbs.
//!
//! R1 read both from the per-*block* `solid` flag, which is wrong for a bottom
//! slab, a stair, a hopper or a `type=double` slab: their collision shape is
//! per state, and the tables in [`aether_world::registry::shapes`] already
//! carry it. These functions read that table, so the answer follows the state.

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

/// `SupportType.FULL`: the state's collision shape covers the whole face in
/// `direction` (`Block.isFaceFull`).
///
/// Vanilla projects the shape's **extreme slice** in `direction` onto the plane:
/// a bottom slab's top face at y = 0.5 is a full unit square and counts, even
/// though it is not at the block boundary. The slice is sampled on a 16×16
/// grid; every sample must fall inside a box that reaches the extreme.
pub fn face_full(state: BlockStateId, direction: Direction) -> bool {
    let boxes = shapes::collision_boxes(state.raw());
    if boxes.is_empty() {
        return false;
    }
    let axis = axis(direction);
    let positive = matches!(
        direction,
        Direction::Up | Direction::East | Direction::South
    );
    let coord = |b: &[f32; 6]| if positive { b[axis + 3] } else { b[axis] };
    let extreme =
        boxes
            .iter()
            .map(coord)
            .fold(if positive { f32::MIN } else { f32::MAX }, |a, b| {
                if positive {
                    a.max(b)
                } else {
                    a.min(b)
                }
            });
    let slice: Vec<&[f32; 6]> = boxes
        .iter()
        .filter(|b| (coord(b) - extreme).abs() < EPS)
        .collect();

    let (a1, a2) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    const N: i32 = 16;
    for i in 0..N {
        for j in 0..N {
            let u = (i as f32 + 0.5) / N as f32;
            let v = (j as f32 + 0.5) / N as f32;
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

/// The axis a direction runs along.
fn axis(direction: Direction) -> usize {
    match direction {
        Direction::Up | Direction::Down => 1,
        Direction::West | Direction::East => 0,
        Direction::North | Direction::South => 2,
    }
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
            assert!(face_full(stone, d), "{d:?}");
        }
    }

    #[test]
    fn a_bottom_slab_is_sturdy_on_top_and_bottom_but_not_its_sides() {
        // Finding 2's case: a bottom slab's top face is a full square at
        // y = 0.5, so `isFaceSturdy(UP)` is true and wire climbs it; its west
        // face is half-height, so it is not. It is not a conductor.
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
}
