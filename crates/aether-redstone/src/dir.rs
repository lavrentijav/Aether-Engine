//! The six cardinal directions, with vanilla's iteration and rotation rules.
//!
//! These carry the same numeric meaning as `net.minecraft.core.Direction`:
//! `Down` is `-Y`, `Up` is `+Y`, `North` is `-Z`, `South` is `+Z`, `West` is
//! `-X`, `East` is `+X`. The iteration orders matter for redstone because the
//! engine visits neighbours in vanilla's fixed order — see
//! [`Direction::UPDATE_ORDER`].

/// A cardinal direction, matching `net.minecraft.core.Direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    /// `-Y`.
    Down,
    /// `+Y`.
    Up,
    /// `-Z`.
    North,
    /// `+Z`.
    South,
    /// `-X`.
    West,
    /// `+X`.
    East,
}

impl Direction {
    /// Every direction, in vanilla's `Direction.values()` order.
    pub const ALL: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    /// The four horizontal directions.
    pub const HORIZONTAL: [Direction; 4] = [
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    /// The order a neighbour update visits the six neighbours.
    ///
    /// This is `NeighborUpdater.UPDATE_ORDER` in vanilla. It is not the same as
    /// [`Direction::ALL`], and the difference is observable: it decides which
    /// of several pending neighbour updates is processed first.
    pub const UPDATE_ORDER: [Direction; 6] = [
        Direction::West,
        Direction::East,
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
    ];

    /// The order the six neighbours are visited for a shape update.
    ///
    /// This is `BlockBehaviour.UPDATE_SHAPE_ORDER` in vanilla.
    pub const UPDATE_SHAPE_ORDER: [Direction; 6] = [
        Direction::West,
        Direction::East,
        Direction::North,
        Direction::South,
        Direction::Down,
        Direction::Up,
    ];

    /// The direction pointing the other way.
    pub const fn opposite(self) -> Direction {
        match self {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
            Direction::East => Direction::West,
        }
    }

    /// The unit offset from a cell to its neighbour in this direction.
    pub const fn offset(self) -> (i32, i32, i32) {
        match self {
            Direction::Down => (0, -1, 0),
            Direction::Up => (0, 1, 0),
            Direction::North => (0, 0, -1),
            Direction::South => (0, 0, 1),
            Direction::West => (-1, 0, 0),
            Direction::East => (1, 0, 0),
        }
    }

    /// Whether the direction is horizontal (`North`/`South`/`West`/`East`).
    pub const fn is_horizontal(self) -> bool {
        !matches!(self, Direction::Up | Direction::Down)
    }

    /// The direction 90° clockwise around the vertical axis, as
    /// `Direction.getClockWise()`.
    ///
    /// Returns `self` for the vertical directions, which vanilla's axis-aware
    /// overload does as well.
    pub const fn clockwise(self) -> Direction {
        match self {
            Direction::North => Direction::East,
            Direction::East => Direction::South,
            Direction::South => Direction::West,
            Direction::West => Direction::North,
            other => other,
        }
    }

    /// The direction 90° counter-clockwise around the vertical axis, as
    /// `Direction.getCounterClockWise()`.
    pub const fn counter_clockwise(self) -> Direction {
        self.clockwise().clockwise().clockwise()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_is_an_involution() {
        for d in Direction::ALL {
            assert_eq!(d.opposite().opposite(), d);
        }
    }

    #[test]
    fn rotations_are_inverses() {
        for d in Direction::HORIZONTAL {
            assert_eq!(d.clockwise().counter_clockwise(), d);
            assert_eq!(d.counter_clockwise().clockwise(), d);
        }
    }

    #[test]
    fn offsets_match_the_axes() {
        assert_eq!(Direction::East.offset(), (1, 0, 0));
        assert_eq!(Direction::West.offset(), (-1, 0, 0));
        assert_eq!(Direction::Up.offset(), (0, 1, 0));
        assert_eq!(Direction::Down.offset(), (0, -1, 0));
        assert_eq!(Direction::South.offset(), (0, 0, 1));
        assert_eq!(Direction::North.offset(), (0, 0, -1));
    }
}
