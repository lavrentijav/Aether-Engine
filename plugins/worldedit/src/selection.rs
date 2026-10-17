#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub world: Option<String>,
    pub p1: Option<[i32; 3]>,
    pub p2: Option<[i32; 3]>,
}

impl Selection {
    pub fn set_point(&mut self, first: bool, world: &str, pos: [i32; 3]) {
        if self.world.as_deref() != Some(world) {
            self.p1 = None;
            self.p2 = None;
            self.world = Some(world.to_string());
        }
        if first {
            self.p1 = Some(pos);
        } else {
            self.p2 = Some(pos);
        }
    }

    pub fn bounds(&self) -> Option<([i32; 3], [i32; 3])> {
        let a = self.p1?;
        let b = self.p2?;
        Some((
            [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        ))
    }
}

pub fn volume(min: [i32; 3], max: [i32; 3]) -> u128 {
    let dx = (max[0] as i64 - min[0] as i64 + 1) as u128;
    let dy = (max[1] as i64 - min[1] as i64 + 1) as u128;
    let dz = (max[2] as i64 - min[2] as i64 + 1) as u128;
    dx * dy * dz
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    North,
    South,
    East,
    West,
    Up,
    Down,
}

impl Direction {
    pub fn parse(word: &str) -> Option<Direction> {
        match word.to_ascii_lowercase().as_str() {
            "n" | "north" => Some(Direction::North),
            "s" | "south" => Some(Direction::South),
            "e" | "east" => Some(Direction::East),
            "w" | "west" => Some(Direction::West),
            "u" | "up" => Some(Direction::Up),
            "d" | "down" => Some(Direction::Down),
            _ => None,
        }
    }

    pub fn from_yaw(yaw: f32) -> Direction {
        let yaw = ((yaw % 360.0) + 360.0) % 360.0;
        if yaw < 45.0 || yaw >= 315.0 {
            Direction::South
        } else if yaw < 135.0 {
            Direction::West
        } else if yaw < 225.0 {
            Direction::North
        } else {
            Direction::East
        }
    }
}

fn shifted(value: i32, delta: i64) -> Result<i32, String> {
    i64::from(value)
        .checked_add(delta)
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| "the selection would leave the world coordinate range".to_string())
}

pub fn expand(
    min: [i32; 3],
    max: [i32; 3],
    amount: u32,
    direction: Direction,
) -> Result<([i32; 3], [i32; 3]), String> {
    let n = i64::from(amount);
    let (mut min, mut max) = (min, max);
    match direction {
        Direction::North => min[2] = shifted(min[2], -n)?,
        Direction::South => max[2] = shifted(max[2], n)?,
        Direction::East => max[0] = shifted(max[0], n)?,
        Direction::West => min[0] = shifted(min[0], -n)?,
        Direction::Up => max[1] = shifted(max[1], n)?,
        Direction::Down => min[1] = shifted(min[1], -n)?,
    }
    Ok((min, max))
}

pub fn contract(
    min: [i32; 3],
    max: [i32; 3],
    amount: u32,
    direction: Direction,
) -> Result<([i32; 3], [i32; 3]), String> {
    let n = i64::from(amount);
    let (mut min, mut max) = (min, max);
    match direction {
        Direction::North => min[2] = shifted(min[2], n)?,
        Direction::South => max[2] = shifted(max[2], -n)?,
        Direction::East => max[0] = shifted(max[0], -n)?,
        Direction::West => min[0] = shifted(min[0], n)?,
        Direction::Up => max[1] = shifted(max[1], -n)?,
        Direction::Down => min[1] = shifted(min[1], n)?,
    }
    if (0..3).any(|axis| min[axis] > max[axis]) {
        return Err("cannot contract the selection that far".to_string());
    }
    Ok((min, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: [i32; 3] = [10, 20, 30];
    const MAX: [i32; 3] = [12, 22, 32];

    #[test]
    fn expand_grows_the_named_side_by_exactly_n() {
        assert_eq!(
            expand(MIN, MAX, 3, Direction::North),
            Ok(([10, 20, 27], MAX))
        );
        assert_eq!(
            expand(MIN, MAX, 3, Direction::South),
            Ok((MIN, [12, 22, 35]))
        );
        assert_eq!(
            expand(MIN, MAX, 3, Direction::East),
            Ok((MIN, [15, 22, 32]))
        );
        assert_eq!(expand(MIN, MAX, 3, Direction::West), Ok(([7, 20, 30], MAX)));
        assert_eq!(expand(MIN, MAX, 3, Direction::Up), Ok((MIN, [12, 25, 32])));
        assert_eq!(
            expand(MIN, MAX, 3, Direction::Down),
            Ok(([10, 17, 30], MAX))
        );
    }

    #[test]
    fn contract_shrinks_the_named_side_by_exactly_n() {
        assert_eq!(
            contract(MIN, MAX, 1, Direction::North),
            Ok(([10, 20, 31], MAX))
        );
        assert_eq!(
            contract(MIN, MAX, 1, Direction::South),
            Ok((MIN, [12, 22, 31]))
        );
        assert_eq!(
            contract(MIN, MAX, 1, Direction::East),
            Ok((MIN, [11, 22, 32]))
        );
        assert_eq!(
            contract(MIN, MAX, 1, Direction::West),
            Ok(([11, 20, 30], MAX))
        );
        assert_eq!(
            contract(MIN, MAX, 1, Direction::Up),
            Ok((MIN, [12, 21, 32]))
        );
        assert_eq!(
            contract(MIN, MAX, 1, Direction::Down),
            Ok(([10, 21, 30], MAX))
        );
    }

    #[test]
    fn contracting_past_a_side_is_refused() {
        assert!(contract([0, 0, 0], [0, 0, 0], 1, Direction::North).is_err());
    }

    #[test]
    fn expansion_refuses_an_i32_overflow() {
        assert!(expand([i32::MAX, 0, 0], [i32::MAX, 0, 0], 1, Direction::East).is_err());
    }

    #[test]
    fn facing_maps_yaw_quadrants() {
        assert_eq!(Direction::from_yaw(0.0), Direction::South);
        assert_eq!(Direction::from_yaw(90.0), Direction::West);
        assert_eq!(Direction::from_yaw(180.0), Direction::North);
        assert_eq!(Direction::from_yaw(270.0), Direction::East);
        // Wrapping and negatives land in the same quadrants.
        assert_eq!(Direction::from_yaw(-90.0), Direction::East);
        assert_eq!(Direction::from_yaw(720.0), Direction::South);
    }

    #[test]
    fn a_selection_normalises_corner_order_and_resets_on_a_world_change() {
        let mut selection = Selection::default();
        selection.set_point(true, "overworld", [5, 4, 3]);
        selection.set_point(false, "overworld", [1, 2, 9]);
        assert_eq!(
            selection.bounds(),
            Some(([1, 2, 3], [5, 4, 9])),
            "corners are normalised on every axis"
        );
        selection.set_point(true, "the_nether", [0, 0, 0]);
        assert_eq!(
            selection.bounds(),
            None,
            "a point in another world clears the old corners"
        );
        assert_eq!(selection.world.as_deref(), Some("the_nether"));
    }

    #[test]
    fn volume_is_inclusive_and_uses_u128() {
        assert_eq!(volume([0, 0, 0], [0, 0, 0]), 1);
        assert_eq!(volume([-1, -1, -1], [1, 1, 1]), 27);
        // A full-width box overflows `u64` but not `u128`.
        assert!(
            volume(
                [i32::MIN, i32::MIN, i32::MIN],
                [i32::MAX, i32::MAX, i32::MAX]
            ) > u128::from(u64::MAX)
        );
    }
}
