use crate::pattern::Pattern;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushKind {
    Sphere,
    Cylinder,
    Smooth,
}

#[derive(Clone, Debug)]
pub struct Brush {
    pub kind: BrushKind,
    pub pattern: Option<Pattern>,
    pub radius: u32,
    pub height: u32,
    pub item: String,
}

impl Brush {
    pub fn estimated_volume(&self) -> u128 {
        let diameter = u128::from(self.radius) * 2 + 1;
        match self.kind {
            BrushKind::Sphere | BrushKind::Smooth => diameter * diameter * diameter,
            BrushKind::Cylinder => diameter * diameter * u128::from(self.height),
        }
    }

    pub fn cells(&self, center: [i32; 3]) -> Result<Vec<[i32; 3]>, String> {
        let radius = i64::from(self.radius);
        let [cx, cy, cz] = center;
        let shifted = |axis: i32, delta: i64| {
            i64::from(axis)
                .checked_add(delta)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| "the brush would leave the world coordinate range".to_string())
        };
        let mut cells = Vec::new();
        match self.kind {
            BrushKind::Sphere | BrushKind::Smooth => {
                for dy in -radius..=radius {
                    for dz in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx * dx + dy * dy + dz * dz <= radius * radius {
                                cells.push([shifted(cx, dx)?, shifted(cy, dy)?, shifted(cz, dz)?]);
                            }
                        }
                    }
                }
            }
            BrushKind::Cylinder => {
                let height = i64::from(self.height);
                let low = -(height / 2);
                let high = low + height - 1;
                for dy in low..=high {
                    for dz in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx * dx + dz * dz <= radius * radius {
                                cells.push([shifted(cx, dx)?, shifted(cy, dy)?, shifted(cz, dz)?]);
                            }
                        }
                    }
                }
            }
        }
        Ok(cells)
    }
}
