use crate::selection::Direction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clipboard {
    pub world: String,
    /// The copy origin (the player's block position when `//copy` ran).
    ///
    /// Stored so a schematic can persist both the absolute minimum corner and
    /// the origin-relative offset Sponge v2 keeps in `Metadata`.
    pub origin: [i32; 3],
    /// The minimum corner relative to [`Self::origin`].
    pub offset: [i32; 3],
    pub size: [usize; 3],
    pub blocks: Vec<String>,
}

/// One 90° rotation or mirror applied to the clipboard, used for both the
/// position remap and the block-state transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    RotateY,
    RotateX,
    RotateZ,
    MirrorX,
    MirrorY,
    MirrorZ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Face {
    North,
    South,
    East,
    West,
    Up,
    Down,
}

impl Face {
    fn parse(value: &str) -> Option<Face> {
        match value {
            "north" => Some(Face::North),
            "south" => Some(Face::South),
            "east" => Some(Face::East),
            "west" => Some(Face::West),
            "up" => Some(Face::Up),
            "down" => Some(Face::Down),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Face::North => "north",
            Face::South => "south",
            Face::East => "east",
            Face::West => "west",
            Face::Up => "up",
            Face::Down => "down",
        }
    }

    fn map(self, step: Step) -> Face {
        match step {
            Step::RotateY => match self {
                Face::North => Face::East,
                Face::East => Face::South,
                Face::South => Face::West,
                Face::West => Face::North,
                other => other,
            },
            Step::RotateX => match self {
                Face::North => Face::Down,
                Face::Down => Face::South,
                Face::South => Face::Up,
                Face::Up => Face::North,
                other => other,
            },
            Step::RotateZ => match self {
                Face::East => Face::Down,
                Face::Down => Face::West,
                Face::West => Face::Up,
                Face::Up => Face::East,
                other => other,
            },
            Step::MirrorX => match self {
                Face::East => Face::West,
                Face::West => Face::East,
                other => other,
            },
            Step::MirrorY => match self {
                Face::Up => Face::Down,
                Face::Down => Face::Up,
                other => other,
            },
            Step::MirrorZ => match self {
                Face::North => Face::South,
                Face::South => Face::North,
                other => other,
            },
        }
    }

    fn horizontal(self) -> bool {
        matches!(self, Face::North | Face::South | Face::East | Face::West)
    }
}

fn map_axis(value: &str, step: Step) -> String {
    let mapped = match (step, value) {
        (Step::RotateY, "x") => "z",
        (Step::RotateY, "z") => "x",
        (Step::RotateX, "y") => "z",
        (Step::RotateX, "z") => "y",
        (Step::RotateZ, "x") => "y",
        (Step::RotateZ, "y") => "x",
        _ => value,
    };
    mapped.to_string()
}

fn map_half(value: &str, step: Step) -> String {
    match step {
        Step::MirrorY => match value {
            "top" => "bottom",
            "bottom" => "top",
            _ => value,
        },
        _ => value,
    }
    .to_string()
}

fn swap_left_right(value: &str) -> String {
    match value {
        "left" => "right",
        "right" => "left",
        "inner_left" => "inner_right",
        "inner_right" => "inner_left",
        "outer_left" => "outer_right",
        "outer_right" => "outer_left",
        _ => value,
    }
    .to_string()
}

fn transform_shape(value: &str, step: Step) -> String {
    let has_compass = ["north", "south", "east", "west"]
        .iter()
        .any(|word| value.contains(word));
    if !has_compass {
        // Stair shapes are relative to `facing`, so a rotation or a vertical
        // mirror keeps them; a horizontal mirror reverses left and right.
        return match step {
            Step::MirrorX | Step::MirrorZ => swap_left_right(value),
            _ => value.to_string(),
        };
    }
    // Rail and jigsaw shapes name compass directions explicitly.
    let words: Vec<&str> = value.split('_').collect();
    let mut mapped = Vec::with_capacity(words.len());
    for word in &words {
        match Face::parse(word) {
            Some(face) => {
                let face = face.map(step);
                if !face.horizontal() {
                    // A vertical rail/jigsaw shape is not representable.
                    return value.to_string();
                }
                mapped.push(face.name());
            }
            None => mapped.push(word),
        }
    }
    if mapped.len() == 2
        && mapped
            .iter()
            .all(|word| matches!(*word, "north" | "south" | "east" | "west"))
    {
        let first = mapped[0];
        let second = mapped[1];
        let (first, second) = canonical_pair(first, second);
        mapped[0] = first;
        mapped[1] = second;
    }
    mapped.join("_")
}

/// Vanilla's canonical spelling of a two-direction value: a north/south word
/// comes first, otherwise east precedes west. Without this an east/west rail
/// mirrored into `west_east` is not a registered state and the edit fails.
fn canonical_pair(first: &str, second: &str) -> (&'static str, &'static str) {
    let names = |word: &str| -> &'static str {
        match word {
            "north" => "north",
            "south" => "south",
            "east" => "east",
            _ => "west",
        }
    };
    let (first, second) = (names(first), names(second));
    let north = if first == "north" || second == "north" {
        Some("north")
    } else {
        None
    };
    let south = if first == "south" || second == "south" {
        Some("south")
    } else {
        None
    };
    let east = if first == "east" || second == "east" {
        Some("east")
    } else {
        None
    };
    let west = if first == "west" || second == "west" {
        Some("west")
    } else {
        None
    };
    if let (Some(north), Some(other)) = (north, south.or(east).or(west)) {
        (north, other)
    } else if let (Some(south), Some(other)) = (south, east.or(west)) {
        (south, other)
    } else {
        (east.unwrap_or("west"), west.unwrap_or("east"))
    }
}

fn transform_type(value: &str, step: Step) -> String {
    match step {
        Step::MirrorX | Step::MirrorZ => match value {
            "left" => "right".to_string(),
            "right" => "left".to_string(),
            _ => value.to_string(),
        },
        Step::MirrorY => match value {
            "top" => "bottom".to_string(),
            "bottom" => "top".to_string(),
            _ => value.to_string(),
        },
        _ => value.to_string(),
    }
}

fn transform_rotation(value: &str, step: Step) -> String {
    // Sign/banner rotations are 16 steps: 0 = south, 4 = west, 8 = north,
    // 12 = east. All 16 are transformed, not just the quarter turns.
    let Ok(rotation) = value.parse::<i32>() else {
        return value.to_string();
    };
    let rotation = rotation.rem_euclid(16);
    let mapped = match step {
        Step::RotateY => rotation + 4,
        Step::MirrorX => -rotation,
        Step::MirrorZ => 8 - rotation,
        // A rotation about X/Z tips a floor sign or banner out of the plane
        // its `rotation` property describes; vanilla has no state for that,
        // so the yaw is left as it was. A vertical mirror keeps the yaw too.
        Step::RotateX | Step::RotateZ | Step::MirrorY => rotation,
    };
    mapped.rem_euclid(16).to_string()
}

/// Transform a `FrontAndTop` value (`north_up`, `down_east`, …), used by the
/// crafter and jigsaw blocks. A valid value has one vertical and one
/// horizontal face; a rotation preserves that. A vertical mirror can turn
/// `up` into `down`, which has no valid name with a horizontal front, so the
/// result falls back to the nearest representable orientation.
fn transform_orientation(value: &str, step: Step) -> String {
    let mut parts = value.split('_');
    let (Some(front), Some(top), None) = (parts.next(), parts.next(), parts.next()) else {
        return value.to_string();
    };
    let (Some(front), Some(top)) = (Face::parse(front), Face::parse(top)) else {
        return value.to_string();
    };
    let front = front.map(step);
    let top = top.map(step);
    if front.horizontal() {
        // A horizontal front is only valid with `top = up`; a vertical
        // mirror maps `up` to `down`, so normalise back to `up`.
        format!("{}_{}", front.name(), Face::Up.name())
    } else {
        let top = if top.horizontal() { top } else { Face::North };
        format!("{}_{}", front.name(), top.name())
    }
}

fn transform_property(key: &str, value: &str, step: Step) -> String {
    match key {
        "facing" => Face::parse(value)
            .map(|face| face.map(step).name().to_string())
            .unwrap_or_else(|| value.to_string()),
        "axis" => map_axis(value, step),
        "half" => map_half(value, step),
        "shape" => transform_shape(value, step),
        "type" => transform_type(value, step),
        "rotation" => transform_rotation(value, step),
        "orientation" => transform_orientation(value, step),
        _ => value.to_string(),
    }
}

fn transform_block(block: &str, step: Step) -> String {
    let Some((base, rest)) = block.split_once('[') else {
        return block.to_string();
    };
    let Some(props) = rest.strip_suffix(']') else {
        return block.to_string();
    };
    let mut out = Vec::new();
    for part in props.split(',') {
        let Some((key, value)) = part.split_once('=') else {
            out.push(part.to_string());
            continue;
        };
        out.push(format!("{key}={}", transform_property(key, value, step)));
    }
    format!("{base}[{}]", out.join(","))
}

fn mirror_offset(value: i32, extent: usize) -> i32 {
    i64::from(value)
        .checked_neg()
        .and_then(|value| value.checked_sub(extent as i64 - 1))
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(value)
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
            origin: self.origin,
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

    fn transform_states(&mut self, step: Step) {
        for block in &mut self.blocks {
            *block = transform_block(block, step);
        }
    }

    fn rotate_y_once(&self) -> Clipboard {
        let [w, h, d] = self.size;
        let mut out = self.remap([d, h, w], |x, y, z| [d - 1 - z, y, x]);
        out.offset = [
            mirror_offset(self.offset[2], d),
            self.offset[1],
            self.offset[0],
        ];
        out.transform_states(Step::RotateY);
        out
    }

    fn rotate_x_once(&self) -> Clipboard {
        let [w, h, d] = self.size;
        let mut out = self.remap([w, d, h], |x, y, z| [x, z, h - 1 - y]);
        out.offset = [
            self.offset[0],
            self.offset[2],
            mirror_offset(self.offset[1], h),
        ];
        out.transform_states(Step::RotateX);
        out
    }

    fn rotate_z_once(&self) -> Clipboard {
        let [w, h, d] = self.size;
        let mut out = self.remap([h, w, d], |x, y, z| [y, w - 1 - x, z]);
        out.offset = [
            self.offset[1],
            mirror_offset(self.offset[0], w),
            self.offset[2],
        ];
        out.transform_states(Step::RotateZ);
        out
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
                let mut out = self.remap([w, h, d], |x, y, z| [x, y, d - 1 - z]);
                out.offset = [
                    self.offset[0],
                    self.offset[1],
                    mirror_offset(self.offset[2], d),
                ];
                out.transform_states(Step::MirrorZ);
                out
            }
            Direction::East | Direction::West => {
                let mut out = self.remap([w, h, d], |x, y, z| [w - 1 - x, y, z]);
                out.offset = [
                    mirror_offset(self.offset[0], w),
                    self.offset[1],
                    self.offset[2],
                ];
                out.transform_states(Step::MirrorX);
                out
            }
            Direction::Up | Direction::Down => {
                let mut out = self.remap([w, h, d], |x, y, z| [x, h - 1 - y, z]);
                out.offset = [
                    self.offset[0],
                    mirror_offset(self.offset[1], h),
                    self.offset[2],
                ];
                out.transform_states(Step::MirrorY);
                out
            }
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
            origin: [0, 0, 0],
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
    fn a_rotation_rotates_the_offset_about_the_origin_and_not_the_corner() {
        // The player copied from the middle of a 3-wide bar: the minimum
        // corner is one block west of the origin.
        let mut c = clipboard([3, 1, 1]);
        c.offset = [-1, 0, 0];
        let rotated = c.rotate(90, 0, 0).unwrap();
        assert_eq!(rotated.size, [1, 1, 3]);
        // The rotated bar still runs through the origin: x=0, z=-1..=1.
        assert_eq!(rotated.offset, [0, 0, -1]);
        let back = rotated.rotate(270, 0, 0).unwrap();
        assert_eq!(back.offset, c.offset);
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
    fn flipping_mirrors_the_offset_about_the_origin() {
        let mut c = clipboard([3, 2, 4]);
        c.offset = [1, -1, 2];
        let flipped = c.flip(Direction::East);
        assert_eq!(flipped.offset, [-3, -1, 2]);
        assert_eq!(flipped.flip(Direction::West).offset, c.offset);
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

    #[test]
    fn rotation_transforms_directional_block_states() {
        let mut c = clipboard([2, 1, 1]);
        c.blocks = vec![
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight]".to_string(),
            "minecraft:oak_log[axis=x]".to_string(),
        ];
        let rotated = c.rotate(90, 0, 0).unwrap();
        assert_eq!(
            rotated.blocks,
            vec![
                "minecraft:oak_stairs[facing=east,half=bottom,shape=straight]".to_string(),
                "minecraft:oak_log[axis=z]".to_string(),
            ]
        );
        let mirrored = c.flip(Direction::East);
        assert_eq!(
            mirrored.blocks,
            vec![
                "minecraft:oak_log[axis=x]".to_string(),
                "minecraft:oak_stairs[facing=north,half=bottom,shape=straight]".to_string(),
            ]
        );
        assert_eq!(
            transform_block(
                "minecraft:oak_stairs[facing=north,shape=inner_left]",
                Step::MirrorX
            ),
            "minecraft:oak_stairs[facing=north,shape=inner_right]"
        );
        assert_eq!(
            transform_block("minecraft:rail[shape=north_east]", Step::RotateY),
            "minecraft:rail[shape=south_east]"
        );
        assert_eq!(
            transform_block("minecraft:oak_log[axis=y]", Step::RotateX),
            "minecraft:oak_log[axis=z]"
        );
    }

    #[test]
    fn two_way_compass_shapes_keep_a_valid_canonical_order() {
        // east_west is the only spelling of an east/west rail; the mirrored
        // `west_east` is not a registered state.
        for direction in [Direction::East, Direction::West] {
            let flipped = transform_block("minecraft:rail[shape=east_west]", step_for(direction));
            assert_eq!(flipped, "minecraft:rail[shape=east_west]", "{direction:?}");
        }
        assert_eq!(
            transform_block("minecraft:rail[shape=north_east]", Step::MirrorX),
            "minecraft:rail[shape=north_west]"
        );
        assert_eq!(
            transform_block("minecraft:rail[shape=south_west]", Step::MirrorZ),
            "minecraft:rail[shape=north_west]"
        );
        assert_eq!(
            transform_block("minecraft:rail[shape=north_south]", Step::MirrorX),
            "minecraft:rail[shape=north_south]"
        );
    }

    #[test]
    fn arbitrary_sign_rotations_are_transformed() {
        assert_eq!(
            transform_block("minecraft:oak_sign[rotation=1]", Step::RotateY),
            "minecraft:oak_sign[rotation=5]"
        );
        assert_eq!(
            transform_block("minecraft:oak_sign[rotation=1]", Step::MirrorX),
            "minecraft:oak_sign[rotation=15]"
        );
        assert_eq!(
            transform_block("minecraft:oak_sign[rotation=1]", Step::MirrorZ),
            "minecraft:oak_sign[rotation=7]"
        );
        assert_eq!(
            transform_block("minecraft:oak_sign[rotation=13]", Step::RotateY),
            "minecraft:oak_sign[rotation=1]"
        );
    }

    #[test]
    fn crafter_orientation_is_rotated_and_mirrored() {
        assert_eq!(
            transform_block("minecraft:crafter[orientation=north_up]", Step::RotateY),
            "minecraft:crafter[orientation=east_up]"
        );
        assert_eq!(
            transform_block("minecraft:crafter[orientation=down_east]", Step::RotateY),
            "minecraft:crafter[orientation=down_south]"
        );
        assert_eq!(
            transform_block("minecraft:crafter[orientation=down_east]", Step::MirrorX),
            "minecraft:crafter[orientation=down_west]"
        );
        // A vertical mirror cannot point `up` at `down` with a horizontal
        // front, so the result is the nearest representable orientation.
        assert_eq!(
            transform_block("minecraft:crafter[orientation=north_up]", Step::MirrorY),
            "minecraft:crafter[orientation=north_up]"
        );
        assert_eq!(
            transform_block("minecraft:jigsaw[orientation=west_up]", Step::RotateZ),
            "minecraft:jigsaw[orientation=up_east]"
        );
    }

    fn step_for(direction: Direction) -> Step {
        match direction {
            Direction::East | Direction::West => Step::MirrorX,
            _ => Step::MirrorZ,
        }
    }
}
