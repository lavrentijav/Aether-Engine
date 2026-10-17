use crate::selection::Direction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clipboard {
    pub world: String,
    pub offset: [i32; 3],
    pub size: [usize; 3],
    pub blocks: Vec<String>,
}

impl Clipboard {
    pub fn volume(&self) -> u128 {
        self.size
            .iter()
            .fold(1u128, |acc, n| acc.saturating_mul(*n as u128))
    }

    fn index(&self, x: usize, y: usize, z: usize) -> usize {
        x + self.size[0] * (z + self.size[2] * y)
    }

    pub fn get(&self, x: usize, y: usize, z: usize) -> Option<&str> {
        if x >= self.size[0] || y >= self.size[1] || z >= self.size[2] {
            return None;
        }
        self.blocks.get(self.index(x, y, z)).map(String::as_str)
    }

    fn remap(
        &self,
        new_size: [usize; 3],
        map: impl Fn(usize, usize, usize) -> [usize; 3],
    ) -> Clipboard {
        let mut out = Clipboard {
            world: self.world.clone(),
            offset: self.offset,
            size: new_size,
            blocks: vec![String::new(); new_size[0] * new_size[1] * new_size[2]],
        };
        for y in 0..self.size[1] {
            for z in 0..self.size[2] {
                for x in 0..self.size[0] {
                    let [nx, ny, nz] = map(x, y, z);
                    let target = out.index(nx, ny, nz);
                    out.blocks[target] = self.blocks[self.index(x, y, z)].clone();
                }
            }
        }
        out
    }

    fn rotate_y_once(&self) -> Clipboard {
        let [w, h, d] = self.size;
        self.remap([d, h, w], |x, y, z| [d - 1 - z, y, x])
    }

    fn rotate_x_once(&self) -> Clipboard {
        let [w, h, d] = self.size;
        self.remap([w, d, h], |x, y, z| [x, z, h - 1 - y])
    }

    fn rotate_z_once(&self) -> Clipboard {
        let [w, h, d] = self.size;
        self.remap([h, w, d], |x, y, z| [y, w - 1 - x, z])
    }

    pub fn rotate(&self, y: i32, x: i32, z: i32) -> Result<Clipboard, String> {
        let mut out = self.clone();
        let rotations: [(i32, fn(&Clipboard) -> Clipboard); 3] = [
            (y, Clipboard::rotate_y_once),
            (x, Clipboard::rotate_x_once),
            (z, Clipboard::rotate_z_once),
        ];
        for (degrees, rotate) in rotations {
            if degrees % 90 != 0 {
                return Err("rotation must be a multiple of 90 degrees".to_string());
            }
            for _ in 0..(degrees / 90).rem_euclid(4) {
                out = rotate(&out);
            }
        }
        Ok(out)
    }

    pub fn flip(&self, direction: Direction) -> Clipboard {
        let [w, h, d] = self.size;
        match direction {
            Direction::North | Direction::South => {
                self.remap([w, h, d], |x, y, z| [x, y, d - 1 - z])
            }
            Direction::East | Direction::West => self.remap([w, h, d], |x, y, z| [w - 1 - x, y, z]),
            Direction::Up | Direction::Down => self.remap([w, h, d], |x, y, z| [x, h - 1 - y, z]),
        }
    }

    pub fn step(&self, direction: Direction) -> [i64; 3] {
        let axis_size = |axis: usize| self.size[axis] as i64;
        match direction {
            Direction::North => [0, 0, -axis_size(2)],
            Direction::South => [0, 0, axis_size(2)],
            Direction::East => [axis_size(0), 0, 0],
            Direction::West => [-axis_size(0), 0, 0],
            Direction::Up => [0, axis_size(1), 0],
            Direction::Down => [0, -axis_size(1), 0],
        }
    }
}

pub fn is_air(name: &str) -> bool {
    let base = name.split_once('[').map_or(name, |(head, _)| head);
    base == "minecraft:air" || base == "air" || base == "minecraft:cave_air" || base == "cave_air"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clipboard(size: [usize; 3]) -> Clipboard {
        let mut blocks = Vec::new();
        for y in 0..size[1] {
            for z in 0..size[2] {
                for x in 0..size[0] {
                    blocks.push(format!("{x},{y},{z}"));
                }
            }
        }
        Clipboard {
            world: "overworld".to_string(),
            offset: [0, 0, 0],
            size,
            blocks,
        }
    }

    #[test]
    fn y_rotation_turns_the_footprint() {
        let c = clipboard([2, 1, 3]);
        let c90 = c.rotate(90, 0, 0).unwrap();
        assert_eq!(c90.size, [3, 1, 2]);
        assert_eq!(c90.get(2, 0, 0), Some("0,0,0"));
        assert_eq!(c90.get(0, 0, 0), Some("0,0,2"));
        let c180 = c90.rotate(90, 0, 0).unwrap();
        assert_eq!(c180.size, [2, 1, 3]);
        assert_eq!(c180.get(1, 0, 2), Some("0,0,0"));
        let c270 = c180.rotate(90, 0, 0).unwrap();
        assert_eq!(c270.size, [3, 1, 2]);
        assert_eq!(c270.get(0, 0, 1), Some("0,0,0"));
        assert_eq!(c.rotate(360, 0, 0).unwrap(), c);
    }

    #[test]
    fn an_invalid_angle_is_refused() {
        assert!(clipboard([1, 1, 1]).rotate(45, 0, 0).is_err());
        assert!(clipboard([1, 1, 1]).rotate(0, 1, 0).is_err());
    }

    #[test]
    fn flipping_twice_is_the_identity() {
        let c = clipboard([3, 2, 4]);
        for direction in [
            Direction::North,
            Direction::South,
            Direction::East,
            Direction::West,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(c.flip(direction).flip(direction), c, "{direction:?}");
        }
    }

    #[test]
    fn axis_rotations_keep_every_cell() {
        let c = clipboard([2, 3, 4]);
        for angle in [90, 180, 270] {
            let rotated = c.rotate(angle, angle, angle).unwrap();
            assert_eq!(rotated.blocks.len(), c.blocks.len());
            assert!(rotated.blocks.iter().all(|b| !b.is_empty()));
        }
    }
}
