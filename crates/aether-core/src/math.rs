//! Small `f64` vector/box math shared by physics and the engine API.
//!
//! Deliberately minimal: just what collision, movement and bounding-box queries
//! need. No third-party linear-algebra dependency.

/// A 3D vector / point in world space (blocks are 1.0 units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    /// East(+) / west(−).
    pub x: f64,
    /// Up(+) / down(−).
    pub y: f64,
    /// South(+) / north(−).
    pub z: f64,
}

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Construct a vector.
    #[inline]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// Scale by a scalar.
    #[inline]
    pub fn scale(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }

    /// Squared length (avoids a `sqrt`).
    #[inline]
    pub fn length_squared(self) -> f64 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// Euclidean length.
    #[inline]
    pub fn length(self) -> f64 {
        self.length_squared().sqrt()
    }
}

impl std::ops::Add for Vec3 {
    type Output = Vec3;
    #[inline]
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Vec3;
    #[inline]
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

/// An axis-aligned bounding box in world space.
///
/// Invariant: `min.{x,y,z} <= max.{x,y,z}` for every constructor here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Aabb {
    /// Construct from two corners, normalising so `min <= max` per axis.
    #[inline]
    pub fn new(a: Vec3, b: Vec3) -> Self {
        Self {
            min: Vec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: Vec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    /// A box of the given `size` (width, height, depth) with its **base centre**
    /// (feet) at `pos` — the usual way to place an entity.
    #[inline]
    pub fn from_base(pos: Vec3, width: f64, height: f64) -> Self {
        let hw = width / 2.0;
        Self {
            min: Vec3::new(pos.x - hw, pos.y, pos.z - hw),
            max: Vec3::new(pos.x + hw, pos.y + height, pos.z + hw),
        }
    }

    /// The unit cube occupying block coordinate `(x, y, z)`.
    #[inline]
    pub fn block(x: i32, y: i32, z: i32) -> Self {
        let min = Vec3::new(x as f64, y as f64, z as f64);
        Self {
            min,
            max: Vec3::new(min.x + 1.0, min.y + 1.0, min.z + 1.0),
        }
    }

    /// Translate by `d`.
    #[inline]
    pub fn offset(self, d: Vec3) -> Aabb {
        Aabb {
            min: self.min + d,
            max: self.max + d,
        }
    }

    /// Grow the box to also contain the box translated by `d` (its swept hull).
    #[inline]
    pub fn expand(self, d: Vec3) -> Aabb {
        Aabb {
            min: Vec3::new(
                self.min.x + d.x.min(0.0),
                self.min.y + d.y.min(0.0),
                self.min.z + d.z.min(0.0),
            ),
            max: Vec3::new(
                self.max.x + d.x.max(0.0),
                self.max.y + d.y.max(0.0),
                self.max.z + d.z.max(0.0),
            ),
        }
    }

    /// Whether two boxes overlap with positive volume on all three axes.
    #[inline]
    pub fn intersects(self, o: Aabb) -> bool {
        self.min.x < o.max.x
            && self.max.x > o.min.x
            && self.min.y < o.max.y
            && self.max.y > o.min.y
            && self.min.z < o.max.z
            && self.max.z > o.min.z
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_base_centres_on_feet() {
        let b = Aabb::from_base(Vec3::new(0.0, 64.0, 0.0), 0.6, 1.8);
        assert_eq!(b.min, Vec3::new(-0.3, 64.0, -0.3));
        assert_eq!(b.max, Vec3::new(0.3, 65.8, 0.3));
    }

    #[test]
    fn block_is_unit_cube() {
        let b = Aabb::block(-1, 5, 2);
        assert_eq!(b.min, Vec3::new(-1.0, 5.0, 2.0));
        assert_eq!(b.max, Vec3::new(0.0, 6.0, 3.0));
    }

    #[test]
    fn intersection_is_open() {
        let a = Aabb::block(0, 0, 0);
        // Sharing only a face does not count as intersecting.
        assert!(!a.intersects(Aabb::block(1, 0, 0)));
        assert!(a.intersects(Aabb::new(
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(1.5, 1.5, 1.5)
        )));
    }

    #[test]
    fn expand_covers_swept_region() {
        let a = Aabb::block(0, 0, 0);
        let e = a.expand(Vec3::new(-2.0, 3.0, 0.0));
        assert_eq!(e.min, Vec3::new(-2.0, 0.0, 0.0));
        assert_eq!(e.max, Vec3::new(1.0, 4.0, 1.0));
    }
}
