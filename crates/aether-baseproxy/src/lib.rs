//! # aether-baseproxy
//!
//! **Aether BaseProxy** — the live translation layer that converts *vanilla
//! Minecraft* data into the engine core format, in memory, as it arrives (as
//! opposed to `aether-convert`, which migrates worlds offline from `.mca`).
//!
//! * [`chunk`] decodes a vanilla **network chunk section** (paletted container)
//!   into an engine [`aether_world::SubChunk`].
//! * [`vanilla_registry`] maps vanilla protocol block-state ids to engine ids.
//! * [`player_box`] turns a vanilla player position into a core bounding box the
//!   physics layer understands.
//!
//! Together these let a gateway feed vanilla client/server data straight into
//! the engine's world and physics representation.

pub mod chunk;
pub mod vanilla_registry;

use aether_core::math::{Aabb, Vec3};

pub use chunk::{decode_section, ProxyError, SectionReader};
pub use vanilla_registry::VanillaRegistry;

/// Standard vanilla player collision box: 0.6 wide, 1.8 tall.
pub const PLAYER_WIDTH: f64 = 0.6;
/// Standard vanilla player height.
pub const PLAYER_HEIGHT: f64 = 1.8;

/// Convert a vanilla entity position (feet `x, y, z`, as sent on the wire) into
/// a core [`Aabb`] using the standard player dimensions.
pub fn player_box(x: f64, y: f64, z: f64) -> Aabb {
    Aabb::from_base(Vec3::new(x, y, z), PLAYER_WIDTH, PLAYER_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_box_matches_vanilla_dimensions() {
        let b = player_box(10.0, 64.0, -5.0);
        assert!((b.max.x - b.min.x - PLAYER_WIDTH).abs() < 1.0e-9);
        assert!((b.max.y - b.min.y - PLAYER_HEIGHT).abs() < 1.0e-9);
        assert_eq!(b.min.y, 64.0);
    }
}
