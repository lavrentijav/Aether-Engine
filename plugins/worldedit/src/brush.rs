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
    /// A cheap lower bound: the largest diamond-inscribed box in the shape.
    fn inscribed_volume_lower_bound(&self) -> u128 {
        match self.kind {
            BrushKind::Sphere | BrushKind::Smooth => {
                let half = (f64::from(self.radius) / 3f64.sqrt()).floor() as u128;
                let side = half * 2 + 1;
                side * side * side
            }
            BrushKind::Cylinder => {
                let half = (f64::from(self.radius) / 2f64.sqrt()).floor() as u128;
                let side = half * 2 + 1;
                side * side * u128::from(self.height)
            }
        }
    }

    /// The exact number of cells the brush covers, counted up to `cap`.
    ///
    /// A sphere of radius 6 covers 925 cells, not the 2,197 of its bounding
    /// cube, so the limit must use the lattice count. Counting stops as soon as
    /// it exceeds `cap`, so an absurd radius cannot stall the tick thread.
    pub fn volume_capped(&self, cap: u128) -> u128 {
        // A cheap inscribed-shape lower bound rejects an absurd radius before
        // the lattice scan. If it fits the cap, the exact scan below is
        // bounded by a small multiple of `cap` (the bounding cube is at most
        // ~5.2x the ball's lattice count).
        let lower = self.inscribed_volume_lower_bound();
        if lower > cap {
            return lower;
        }
        let radius = i64::from(self.radius);
        let mut count = 0u128;
        match self.kind {
            BrushKind::Sphere | BrushKind::Smooth => {
                for dy in -radius..=radius {
                    for dz in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx * dx + dy * dy + dz * dz <= radius * radius {
                                count += 1;
                                if count > cap {
                                    return count;
                                }
                            }
                        }
                    }
                }
            }
            BrushKind::Cylinder => {
                let height = i64::from(self.height);
                let low = -(height / 2);
                let high = low + height - 1;
                for _dy in low..=high {
                    for dz in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx * dx + dz * dz <= radius * radius {
                                count += 1;
                                if count > cap {
                                    return count;
                                }
                            }
                        }
                    }
                }
            }
        }
        count
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

#[cfg(test)]
mod tests {
    use super::*;

    fn brush(kind: BrushKind, radius: u32, height: u32) -> Brush {
        Brush {
            kind,
            pattern: None,
            radius,
            height,
            item: "minecraft:blaze_rod".to_string(),
        }
    }

    #[test]
    fn a_sphere_volume_is_its_exact_lattice_count() {
        assert_eq!(brush(BrushKind::Sphere, 1, 1).volume_capped(10_000), 7);
        assert_eq!(brush(BrushKind::Sphere, 6, 1).volume_capped(10_000), 925);
        assert_eq!(brush(BrushKind::Smooth, 2, 1).volume_capped(10_000), 33);
    }

    #[test]
    fn a_cylinder_volume_is_its_exact_lattice_count() {
        // A radius-1 disc has five lattice cells, times a height of three.
        assert_eq!(brush(BrushKind::Cylinder, 1, 3).volume_capped(10_000), 15);
    }

    #[test]
    fn an_absurd_radius_is_rejected_without_scanning_its_bounding_cube() {
        let huge = brush(BrushKind::Sphere, 1_000_000, 1);
        assert!(huge.volume_capped(1_000) > 1_000);
    }
}
