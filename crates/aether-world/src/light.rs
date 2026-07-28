//! Lighting.
//!
//! The real engine will compute block/sky light with the async cell flood-fill
//! (Phase 1 roadmap item). Until that lands, [`FullBright`] is a fallback that
//! reports **maximum light everywhere** — the world is always fully lit — so the
//! rest of the stack (and a connecting client) has a valid light source to read.

/// Maximum Minecraft light level.
pub const MAX_LIGHT: u8 = 15;

/// Read-only access to per-block light levels (`0..=15`).
pub trait LightView {
    /// Emitted/block light at `(x, y, z)`.
    fn block_light(&self, x: i32, y: i32, z: i32) -> u8;
    /// Sky light at `(x, y, z)`.
    fn sky_light(&self, x: i32, y: i32, z: i32) -> u8;
}

/// A lighting source that always returns [`MAX_LIGHT`] — the "always lit"
/// fallback used until real lighting is implemented.
#[derive(Debug, Clone, Copy, Default)]
pub struct FullBright;

impl LightView for FullBright {
    #[inline]
    fn block_light(&self, _x: i32, _y: i32, _z: i32) -> u8 {
        MAX_LIGHT
    }
    #[inline]
    fn sky_light(&self, _x: i32, _y: i32, _z: i32) -> u8 {
        MAX_LIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_bright_is_always_max() {
        let l = FullBright;
        assert_eq!(l.block_light(0, 0, 0), 15);
        assert_eq!(l.sky_light(-100, 200, 40), 15);
    }
}
