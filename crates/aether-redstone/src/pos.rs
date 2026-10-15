//! A block position, and the neighbour arithmetic the engine uses everywhere.

use crate::dir::Direction;

/// An integer block coordinate.
///
/// Deliberately tiny and `Copy`: the redstone engine passes positions around
/// far more often than it dereferences them, and a tuple would need the same
/// methods bolted on somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    /// World X.
    pub x: i32,
    /// World Y.
    pub y: i32,
    /// World Z.
    pub z: i32,
}

impl BlockPos {
    /// A position at `(x, y, z)`.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// The position one step in `dir`.
    pub const fn offset(self, dir: Direction) -> Self {
        let (dx, dy, dz) = dir.offset();
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_walks_a_direction_then_back() {
        let p = BlockPos::new(3, -4, 5);
        for d in Direction::ALL {
            assert_eq!(p.offset(d).offset(d.opposite()), p);
        }
    }
}
