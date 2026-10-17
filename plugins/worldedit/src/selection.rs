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
