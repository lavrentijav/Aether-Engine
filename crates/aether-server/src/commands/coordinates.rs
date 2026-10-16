//! Coordinates and rotations: `~` relative, `^` local, and absolute.
//!
//! Every command that moves, places or aims takes one of these, so the parser
//! and the resolver live in one place. Parsing is *source-independent*: it
//! yields a list of [`WorldCoordinate`]s with no idea where the command was
//! typed. Resolving turns those into a concrete point once the executing
//! source is known, which is the point where `~` needs a position and `^`
//! needs a rotation.
//!
//! The rules are vanilla 1.21.11's:
//!
//! * `~n` is relative to the source position; a bare `~` is `~0`.
//! * `^l ^r` (left, up, forward) are local to the source's yaw/pitch. Local
//!   coordinates may not be mixed with absolute or relative ones.
//! * an absolute component is a double for an exact position; a block position
//!   floors after resolving relatives.
//! * a source with no position (the console) cannot resolve `~`; a source with
//!   no rotation cannot resolve `^`.

use aether_api::Vector3 as Vec3;

use crate::players::PlayerHandle;

/// Why a coordinate string or a resolution failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordError {
    /// A component was not a number.
    NotANumber(String),
    /// A block component's absolute part was not an integer.
    NotAnInteger(String),
    /// A `^` component appeared beside a `~` or absolute one.
    MixedLocal,
    /// A relative or local coordinate had no source position to resolve from.
    NoPosition,
    /// A local coordinate had no source rotation to resolve from.
    NoRotation,
}

impl CoordError {
    /// The chat line this error produces.
    pub fn message(&self) -> String {
        match self {
            CoordError::NotANumber(s) => format!("Invalid coordinate '{s}'"),
            CoordError::NotAnInteger(s) => format!("Invalid integer '{s}'"),
            CoordError::MixedLocal => {
                "Cannot mix world & local coordinates (everything must either use ^ or not)".into()
            }
            CoordError::NoPosition => "Unknown position".into(),
            CoordError::NoRotation => "Unknown rotation".into(),
        }
    }
}

/// One component of a coordinate: absolute, `~`-relative, or `^`-local.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WorldCoordinate {
    /// A plain number.
    Absolute(f64),
    /// `~n`, relative to the source position. `~` alone is `~0`.
    Relative(f64),
    /// `^n`, along the source's local left/up/forward axis.
    Local(f64),
}

impl WorldCoordinate {
    /// Parse one component, allowing fractional absolute values.
    pub fn parse(s: &str) -> Result<Self, CoordError> {
        Self::parse_inner(s, false)
    }

    /// Parse one block-position component: absolute values must be integers.
    pub fn parse_block(s: &str) -> Result<Self, CoordError> {
        Self::parse_inner(s, true)
    }

    fn parse_inner(s: &str, integer_absolute: bool) -> Result<Self, CoordError> {
        if let Some(rest) = s.strip_prefix('~') {
            return Ok(WorldCoordinate::Relative(parse_f64(rest, s)?));
        }
        if let Some(rest) = s.strip_prefix('^') {
            return Ok(WorldCoordinate::Local(parse_f64(rest, s)?));
        }
        let v = parse_f64(s, s)?;
        if integer_absolute && v.fract() != 0.0 {
            return Err(CoordError::NotAnInteger(s.to_string()));
        }
        Ok(WorldCoordinate::Absolute(v))
    }

    /// Whether this component is local (`^`).
    pub fn is_local(self) -> bool {
        matches!(self, WorldCoordinate::Local(_))
    }

    /// Whether this component is relative (`~`).
    pub fn is_relative(self) -> bool {
        matches!(self, WorldCoordinate::Relative(_))
    }

    /// The local offset this component carries, if it is local.
    pub fn local_value(self) -> Option<f64> {
        match self {
            WorldCoordinate::Local(v) => Some(v),
            _ => None,
        }
    }

    /// Resolve against a base value: absolute is itself, relative adds `base`.
    fn resolve_scalar(self, base: Option<f64>) -> Result<f64, CoordError> {
        match self {
            WorldCoordinate::Absolute(v) => Ok(v),
            WorldCoordinate::Relative(v) => match base {
                Some(b) => Ok(b + v),
                None => Err(CoordError::NoPosition),
            },
            WorldCoordinate::Local(_) => Err(CoordError::MixedLocal),
        }
    }
}

/// `~` or `^` with no number is zero; otherwise the rest must parse.
fn parse_f64(rest: &str, whole: &str) -> Result<f64, CoordError> {
    if rest.is_empty() {
        return Ok(0.0);
    }
    rest.parse::<f64>()
        .map_err(|_| CoordError::NotANumber(whole.to_string()))
}

/// A source's yaw and pitch, in degrees. Yaw 0 faces south (+Z).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rotation {
    /// Rotation about the vertical axis; 0 = +Z, 90 = −X.
    pub yaw: f32,
    /// Rotation about the left axis; −90 looks straight up.
    pub pitch: f32,
}

impl Rotation {
    /// A rotation from a yaw and pitch pair.
    pub fn new(yaw: f32, pitch: f32) -> Rotation {
        Rotation { yaw, pitch }
    }

    /// Both angles folded into `[-180, 180)`, as vanilla does when a rotation
    /// is read back.
    pub fn wrapped(self) -> Rotation {
        Rotation {
            yaw: wrap_degrees(self.yaw),
            pitch: wrap_degrees(self.pitch),
        }
    }
}

/// Fold an angle in degrees into `[-180, 180)`.
pub fn wrap_degrees(v: f32) -> f32 {
    let mut d = v % 360.0;
    if d >= 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// A block position: an integer cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockPos {
    /// The X cell.
    pub x: i32,
    /// The Y cell.
    pub y: i32,
    /// The Z cell.
    pub z: i32,
}

impl BlockPos {
    /// Floor an exact point into the cell that contains it.
    pub fn from_vec3(v: Vec3) -> BlockPos {
        BlockPos {
            x: v.x.floor() as i32,
            y: v.y.floor() as i32,
            z: v.z.floor() as i32,
        }
    }
}

/// Where a command runs from: the anchor for relative and local coordinates.
///
/// A player has both a position and a rotation; the console has neither, which
/// is why `~` and `^` from the console fail rather than silently meaning zero.
pub trait CommandSource {
    /// The source's position, if it has one.
    fn position(&self) -> Option<Vec3>;
    /// The source's rotation, if it has one.
    fn rotation(&self) -> Option<Rotation>;
}

impl CommandSource for PlayerHandle {
    fn position(&self) -> Option<Vec3> {
        let p = self.pos();
        Some(Vec3::new(p.x, p.y, p.z))
    }

    fn rotation(&self) -> Option<Rotation> {
        let p = self.pos();
        Some(Rotation::new(p.yaw, p.pitch))
    }
}

/// Three coordinate components: a block position or an exact position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinates {
    /// The X component.
    pub x: WorldCoordinate,
    /// The Y component.
    pub y: WorldCoordinate,
    /// The Z component.
    pub z: WorldCoordinate,
}

impl Coordinates {
    /// Three components.
    pub fn new(x: WorldCoordinate, y: WorldCoordinate, z: WorldCoordinate) -> Coordinates {
        Coordinates { x, y, z }
    }

    /// Whether every component is local.
    pub fn is_local(&self) -> bool {
        self.x.is_local() && self.y.is_local() && self.z.is_local()
    }

    /// Whether any component is relative.
    pub fn is_relative(&self) -> bool {
        self.x.is_relative() || self.y.is_relative() || self.z.is_relative()
    }

    /// Resolve to an exact point.
    pub fn resolve(&self, source: &dyn CommandSource) -> Result<Vec3, CoordError> {
        let locals = [self.x, self.y, self.z]
            .iter()
            .filter(|c| c.is_local())
            .count();
        if locals == 3 {
            let rot = source.rotation().ok_or(CoordError::NoRotation)?;
            let base = source.position().ok_or(CoordError::NoPosition)?;
            let (left, up, forward) = local_basis(rot);
            let l = self.x.local_value().unwrap_or(0.0);
            let u = self.y.local_value().unwrap_or(0.0);
            let f = self.z.local_value().unwrap_or(0.0);
            return Ok(base + left.scale(l) + up.scale(u) + forward.scale(f));
        }
        if locals != 0 {
            return Err(CoordError::MixedLocal);
        }
        let base = source.position();
        Ok(Vec3::new(
            self.x.resolve_scalar(base.map(|p| p.x))?,
            self.y.resolve_scalar(base.map(|p| p.y))?,
            self.z.resolve_scalar(base.map(|p| p.z))?,
        ))
    }

    /// Resolve to a block cell, flooring every component after resolving.
    pub fn resolve_block(&self, source: &dyn CommandSource) -> Result<BlockPos, CoordError> {
        self.resolve(source).map(BlockPos::from_vec3)
    }
}

/// Two coordinate components: a 2D position or a yaw/pitch rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinates2 {
    /// The first component (X, or yaw for a rotation).
    pub x: WorldCoordinate,
    /// The second component (Z, or pitch for a rotation).
    pub y: WorldCoordinate,
}

impl Coordinates2 {
    /// Two components.
    pub fn new(x: WorldCoordinate, y: WorldCoordinate) -> Coordinates2 {
        Coordinates2 { x, y }
    }

    /// Resolve to a pair of exact values.
    pub fn resolve(&self, source: &dyn CommandSource) -> Result<(f64, f64), CoordError> {
        let base = source.position();
        Ok((
            self.x.resolve_scalar(base.map(|p| p.x))?,
            self.y.resolve_scalar(base.map(|p| p.z))?,
        ))
    }

    /// Resolve to a yaw/pitch pair, relative to the source's own rotation.
    pub fn resolve_rotation(&self, source: &dyn CommandSource) -> Result<Rotation, CoordError> {
        let rot = source.rotation();
        let yaw = self.x.resolve_scalar(rot.map(|r| r.yaw as f64))? as f32;
        let pitch = self.y.resolve_scalar(rot.map(|r| r.pitch as f64))? as f32;
        Ok(Rotation::new(yaw, pitch).wrapped())
    }
}

/// Resolve a single `angle` argument: absolute, or relative to source yaw.
pub fn resolve_angle(
    coord: WorldCoordinate,
    source: &dyn CommandSource,
) -> Result<f32, CoordError> {
    let rot = source.rotation();
    let v = coord.resolve_scalar(rot.map(|r| r.yaw as f64))? as f32;
    Ok(wrap_degrees(v))
}

/// The local (left, up, forward) basis for a yaw/pitch, in world space.
///
/// Forward is the look direction; up is perpendicular to it; left is the
/// horizontal left. All three are unit vectors.
pub fn local_basis(rot: Rotation) -> (Vec3, Vec3, Vec3) {
    let yaw = (rot.yaw as f64).to_radians();
    let pitch = (rot.pitch as f64).to_radians();
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let left = Vec3::new(cy, 0.0, sy);
    let up = Vec3::new(-sy * sp, cp, cy * sp);
    let forward = Vec3::new(-sy * cp, -sp, cy * cp);
    (left, up, forward)
}

/// The axes a `swizzle` argument names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swizzle {
    /// `x`.
    X,
    /// `y`.
    Y,
    /// `z`.
    Z,
    /// `xy`.
    XY,
    /// `xz`.
    XZ,
    /// `yz`.
    YZ,
    /// `xyz`.
    XYZ,
}

impl Swizzle {
    /// Parse the exact spelling, case-insensitively as vanilla does.
    pub fn parse(s: &str) -> Option<Swizzle> {
        match s.to_ascii_lowercase().as_str() {
            "x" => Some(Swizzle::X),
            "y" => Some(Swizzle::Y),
            "z" => Some(Swizzle::Z),
            "xy" => Some(Swizzle::XY),
            "xz" => Some(Swizzle::XZ),
            "yz" => Some(Swizzle::YZ),
            "xyz" => Some(Swizzle::XYZ),
            _ => None,
        }
    }

    /// The canonical lowercase spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Swizzle::X => "x",
            Swizzle::Y => "y",
            Swizzle::Z => "z",
            Swizzle::XY => "xy",
            Swizzle::XZ => "xz",
            Swizzle::YZ => "yz",
            Swizzle::XYZ => "xyz",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source {
        pos: Option<Vec3>,
        rot: Option<Rotation>,
    }

    impl CommandSource for Source {
        fn position(&self) -> Option<Vec3> {
            self.pos
        }
        fn rotation(&self) -> Option<Rotation> {
            self.rot
        }
    }

    fn at(x: f64, y: f64, z: f64) -> Source {
        Source {
            pos: Some(Vec3::new(x, y, z)),
            rot: Some(Rotation::new(0.0, 0.0)),
        }
    }

    fn looking(x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> Source {
        Source {
            pos: Some(Vec3::new(x, y, z)),
            rot: Some(Rotation::new(yaw, pitch)),
        }
    }

    const CONSOLE: Source = Source {
        pos: None,
        rot: None,
    };

    fn c(t: &str, integer: bool) -> WorldCoordinate {
        if integer {
            WorldCoordinate::parse_block(t).unwrap()
        } else {
            WorldCoordinate::parse(t).unwrap()
        }
    }

    fn coords(x: &str, y: &str, z: &str) -> Coordinates {
        Coordinates::new(c(x, false), c(y, false), c(z, false))
    }

    #[test]
    fn a_bare_tilde_and_caret_are_zero() {
        assert_eq!(
            WorldCoordinate::parse("~").unwrap(),
            WorldCoordinate::Relative(0.0)
        );
        assert_eq!(
            WorldCoordinate::parse("^").unwrap(),
            WorldCoordinate::Local(0.0)
        );
        assert_eq!(
            WorldCoordinate::parse("~5.5").unwrap(),
            WorldCoordinate::Relative(5.5)
        );
        assert_eq!(
            WorldCoordinate::parse("^-2").unwrap(),
            WorldCoordinate::Local(-2.0)
        );
        assert_eq!(
            WorldCoordinate::parse("12").unwrap(),
            WorldCoordinate::Absolute(12.0)
        );
    }

    #[test]
    fn a_garbage_component_is_an_error_not_a_panic() {
        assert!(matches!(
            WorldCoordinate::parse("~abc"),
            Err(CoordError::NotANumber(_))
        ));
        assert!(matches!(
            WorldCoordinate::parse("wat"),
            Err(CoordError::NotANumber(_))
        ));
        assert!(matches!(
            WorldCoordinate::parse_block("1.5"),
            Err(CoordError::NotAnInteger(_))
        ));
    }

    #[test]
    fn relative_coordinates_add_to_the_source_position() {
        let src = at(10.25, 64.0, -3.5);
        let p = coords("~1", "~2", "~-1").resolve(&src).unwrap();
        assert_eq!(p, Vec3::new(11.25, 66.0, -4.5));
    }

    #[test]
    fn a_fractional_relative_block_position_floors_to_the_right_cell() {
        // The brief's case: `~0 ~ ~` from (0.5, 64.0, 0.5) floors to 0 64 0.
        let src = at(0.5, 64.0, 0.5);
        let block = Coordinates::new(c("~0", true), c("~", true), c("~", true));
        assert_eq!(
            block.resolve_block(&src).unwrap(),
            BlockPos { x: 0, y: 64, z: 0 }
        );
        // A fractional relative part still floors: 0.5 + 0.9 = 1.4 → cell 1.
        let frac = Coordinates::new(c("~0.9", true), c("~", true), c("~", true));
        assert_eq!(frac.resolve_block(&src).unwrap().x, 1);
    }

    #[test]
    fn local_coordinates_follow_yaw_and_pitch() {
        // Facing +Z (yaw 0): `^ ^ ^5` is five blocks forward (+Z).
        let src = looking(0.0, 64.0, 0.0, 0.0, 0.0);
        let forward = Coordinates::new(c("^", false), c("^", false), c("^5", false));
        let p = forward.resolve(&src).unwrap();
        assert!(
            (p.x).abs() < 1e-9 && (p.y - 64.0).abs() < 1e-9 && (p.z - 5.0).abs() < 1e-9,
            "{p:?}"
        );

        // `^1 ^ ^` is one block to the left: facing south, left is east (+X).
        let left = Coordinates::new(c("^1", false), c("^", false), c("^", false));
        let p = left.resolve(&src).unwrap();
        assert!((p.x - 1.0).abs() < 1e-9 && (p.z).abs() < 1e-9, "{p:?}");

        // `^ ^1 ^` is always one block up.
        let up = Coordinates::new(c("^", false), c("^1", false), c("^", false));
        let p = up.resolve(&src).unwrap();
        assert!((p.y - 65.0).abs() < 1e-9, "{p:?}");

        // Facing west (yaw 90): forward is −X.
        let src = looking(0.0, 0.0, 0.0, 90.0, 0.0);
        let p = forward.resolve(&src).unwrap();
        assert!((p.x + 5.0).abs() < 1e-9 && p.z.abs() < 1e-9, "{p:?}");

        // Looking straight up (pitch −90): forward is +Y.
        let src = looking(0.0, 0.0, 0.0, 0.0, -90.0);
        let p = forward.resolve(&src).unwrap();
        assert!((p.y - 5.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn mixing_local_with_world_is_rejected() {
        let src = at(0.0, 0.0, 0.0);
        let mixed = Coordinates::new(c("^", false), c("~", false), c("^", false));
        assert_eq!(mixed.resolve(&src), Err(CoordError::MixedLocal));
        // A block position is no different.
        let mixed = Coordinates::new(c("^", true), c("2", true), c("^", true));
        assert_eq!(mixed.resolve_block(&src), Err(CoordError::MixedLocal));
    }

    #[test]
    fn the_console_has_no_position_or_rotation() {
        let relative = Coordinates::new(c("~1", false), c("~", false), c("~", false));
        assert_eq!(relative.resolve(&CONSOLE), Err(CoordError::NoPosition));
        let local = Coordinates::new(c("^", false), c("^", false), c("^1", false));
        assert_eq!(local.resolve(&CONSOLE), Err(CoordError::NoRotation));
        // Absolute coordinates need no source at all.
        let abs = Coordinates::new(c("1", false), c("2", false), c("3", false));
        assert_eq!(abs.resolve(&CONSOLE).unwrap(), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn a_position_without_a_rotation_cannot_do_local() {
        let src = Source {
            pos: Some(Vec3::new(1.0, 2.0, 3.0)),
            rot: None,
        };
        let local = Coordinates::new(c("^", false), c("^", false), c("^1", false));
        assert_eq!(local.resolve(&src), Err(CoordError::NoRotation));
    }

    #[test]
    fn rotations_resolve_against_the_source_and_wrap() {
        let src = looking(0.0, 0.0, 0.0, 90.0, 0.0);
        let absolute = Coordinates2::new(c("45", false), c("10", false));
        assert_eq!(
            absolute.resolve_rotation(&src).unwrap(),
            Rotation::new(45.0, 10.0)
        );
        let relative = Coordinates2::new(c("~90", false), c("~-10", false));
        // 90 + 90 = 180, which vanilla folds to −180.
        assert_eq!(
            relative.resolve_rotation(&src).unwrap(),
            Rotation::new(-180.0, -10.0)
        );
        // 270 wraps to −90.
        let wrap = Coordinates2::new(c("270", false), c("0", false));
        assert_eq!(wrap.resolve_rotation(&src).unwrap().yaw, -90.0);
    }

    #[test]
    fn a_vec2_resolves_x_and_z() {
        let src = at(1.0, 2.0, 3.0);
        let v = Coordinates2::new(c("~1", false), c("~2", false));
        assert_eq!(v.resolve(&src).unwrap(), (2.0, 5.0));
    }

    #[test]
    fn swizzles_round_trip() {
        for s in ["x", "y", "z", "xy", "xz", "yz", "xyz"] {
            let sw = Swizzle::parse(s).unwrap();
            assert_eq!(sw.as_str(), s);
        }
        // Case-insensitive, as vanilla reads swizzles.
        assert_eq!(Swizzle::parse("XYZ").unwrap(), Swizzle::XYZ);
        assert_eq!(Swizzle::parse("xy z"), None);
        assert_eq!(Swizzle::parse(""), None);
    }

    #[test]
    fn angles_resolve_and_wrap() {
        let src = looking(0.0, 0.0, 0.0, 90.0, 0.0);
        assert_eq!(
            resolve_angle(WorldCoordinate::parse("45").unwrap(), &src).unwrap(),
            45.0
        );
        assert_eq!(
            resolve_angle(WorldCoordinate::parse("~10").unwrap(), &src).unwrap(),
            100.0
        );
        assert_eq!(
            resolve_angle(WorldCoordinate::parse("350").unwrap(), &src).unwrap(),
            -10.0
        );
    }
}
