//! Ore veins: the last thing the noise stage does.
//!
//! Long ribbons of copper near the surface and iron down in the deepslate,
//! placed by swapping some of the stone the density already put there. Nothing
//! here can create or remove a block — a vein only ever replaces the
//! dimension's default block — so this pass cannot change the shape of the
//! world, only its contents.
//!
//! Written from the game's `OreVeinifier`.

use std::sync::Arc;

use super::density::{Ctx, Node};
use super::random::PositionalFactory;

/// A block the noise stage can place.
///
/// Named rather than numbered: the caller maps these onto its own registry,
/// and a name survives a change of id space where a number does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseBlock {
    /// Nothing.
    Air,
    /// The dimension's default fluid.
    Water,
    /// Lava.
    Lava,
    /// The dimension's default block.
    Stone,
    /// Copper vein filler.
    Granite,
    /// Iron vein filler.
    Tuff,
    /// Copper vein ore.
    CopperOre,
    /// The rare rich block in a copper vein.
    RawCopperBlock,
    /// Iron vein ore.
    DeepslateIronOre,
    /// The rare rich block in an iron vein.
    RawIronBlock,
}

impl NoiseBlock {
    /// The namespaced block id.
    pub fn name(self) -> &'static str {
        match self {
            NoiseBlock::Air => "minecraft:air",
            NoiseBlock::Water => "minecraft:water",
            NoiseBlock::Lava => "minecraft:lava",
            NoiseBlock::Stone => "minecraft:stone",
            NoiseBlock::Granite => "minecraft:granite",
            NoiseBlock::Tuff => "minecraft:tuff",
            NoiseBlock::CopperOre => "minecraft:copper_ore",
            NoiseBlock::RawCopperBlock => "minecraft:raw_copper_block",
            NoiseBlock::DeepslateIronOre => "minecraft:deepslate_iron_ore",
            NoiseBlock::RawIronBlock => "minecraft:raw_iron_block",
        }
    }
}

/// One of the two vein kinds and the band of the world it lives in.
#[derive(Debug, Clone, Copy)]
struct VeinType {
    ore: NoiseBlock,
    raw_ore_block: NoiseBlock,
    filler: NoiseBlock,
    min_y: i32,
    max_y: i32,
}

const COPPER: VeinType = VeinType {
    ore: NoiseBlock::CopperOre,
    raw_ore_block: NoiseBlock::RawCopperBlock,
    filler: NoiseBlock::Granite,
    min_y: 0,
    max_y: 50,
};

const IRON: VeinType = VeinType {
    ore: NoiseBlock::DeepslateIronOre,
    raw_ore_block: NoiseBlock::RawIronBlock,
    filler: NoiseBlock::Tuff,
    min_y: -60,
    max_y: -8,
};

/// The three density functions and the random stream a vein reads.
#[derive(Clone)]
pub struct OreVeins {
    /// `vein_toggle`: its sign picks the vein kind, its magnitude how strongly
    /// a vein is present.
    pub toggle: Arc<Node>,
    /// `vein_ridged`: carves the ribbons out of the blob.
    pub ridged: Arc<Node>,
    /// `vein_gap`: punches holes so a vein is not solid ore.
    pub gap: Arc<Node>,
    /// Per-position streams, forked from `minecraft:ore`.
    pub random: PositionalFactory,
}

impl OreVeins {
    /// The vein block at this position, or `None` to leave the default block.
    ///
    /// Every early return is a place vanilla gives up, and they are ordered so
    /// the cheap tests come first — the random draw happens only after the
    /// veininess test passes, which is what keeps the stream in step.
    pub fn block_at(&self, ctx: Ctx) -> Option<NoiseBlock> {
        let veininess = self.toggle.compute(ctx);
        let y = ctx.y;
        let vein = if veininess > 0.0 { COPPER } else { IRON };

        let to_max = vein.max_y - y;
        let to_min = y - vein.min_y;
        if to_min < 0 || to_max < 0 {
            return None;
        }
        // Taper the vein off over the last 20 blocks of its band, so it does
        // not stop dead at the boundary.
        let edge = to_max.min(to_min);
        let roundoff = clamped_map(edge as f64, 0.0, 20.0, -0.2, 0.0);
        let strength = veininess.abs();
        if strength + roundoff < 0.4f32 as f64 {
            return None;
        }

        let mut r = self.random.at(ctx.x, y, ctx.z);
        // Three draws at most, always in this order.
        if r.next_f32() > 0.7 {
            return None;
        }
        if self.ridged.compute(ctx) >= 0.0 {
            return None;
        }
        let richness = clamped_map(
            strength,
            0.4f32 as f64,
            0.6f32 as f64,
            0.1f32 as f64,
            0.3f32 as f64,
        );
        if (r.next_f32() as f64) < richness && self.gap.compute(ctx) > -0.3f32 as f64 {
            return Some(if r.next_f32() < 0.02 {
                vein.raw_ore_block
            } else {
                vein.ore
            });
        }
        Some(vein.filler)
    }
}

/// `Mth.clampedMap`.
#[inline]
fn clamped_map(v: f64, from: f64, to: f64, from_value: f64, to_value: f64) -> f64 {
    let t = (v - from) / (to - from);
    if t < 0.0 {
        from_value
    } else if t > 1.0 {
        to_value
    } else {
        from_value + t * (to_value - from_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vein_bands_do_not_overlap_and_sit_where_the_ores_do() {
        // Copper rides above sea level's stone, iron lives in the deepslate.
        assert!(IRON.max_y < COPPER.min_y);
        assert_eq!((COPPER.min_y, COPPER.max_y), (0, 50));
        assert_eq!((IRON.min_y, IRON.max_y), (-60, -8));
    }

    #[test]
    fn block_names_are_namespaced() {
        for b in [
            NoiseBlock::Air,
            NoiseBlock::Water,
            NoiseBlock::Lava,
            NoiseBlock::Stone,
            NoiseBlock::Granite,
            NoiseBlock::Tuff,
            NoiseBlock::CopperOre,
            NoiseBlock::RawCopperBlock,
            NoiseBlock::DeepslateIronOre,
            NoiseBlock::RawIronBlock,
        ] {
            assert!(b.name().starts_with("minecraft:"), "{b:?}");
        }
    }

    #[test]
    fn the_edge_taper_only_bites_near_the_band_edge() {
        assert_eq!(clamped_map(0.0, 0.0, 20.0, -0.2, 0.0), -0.2);
        assert_eq!(clamped_map(20.0, 0.0, 20.0, -0.2, 0.0), 0.0);
        assert_eq!(clamped_map(100.0, 0.0, 20.0, -0.2, 0.0), 0.0);
        assert!((clamped_map(10.0, 0.0, 20.0, -0.2, 0.0) + 0.1).abs() < 1e-12);
    }
}
