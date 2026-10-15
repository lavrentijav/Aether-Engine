//! Block → RGB, for the top-down map.
//!
//! A column's pixel colour is one fixed colour per block name, so the map is
//! stable across renders (a tile that has not changed redraws byte-for-byte)
//! and cheap: no lighting, no texture atlas, no client at all. The palette is
//! the one the offline `vanilla_map` example uses, moved here so the two
//! cannot drift; anything the table does not name falls back to a colour
//! derived from the name, so a modded block still gets a stable, distinct
//! pixel instead of a hole.
//!
//! This is deliberately *not* vanilla's own grass/foliage tinting. The map is
//! an operator's overview, not a screenshot; a fixed palette the reviewer can
//! learn is more useful than biome-accurate colour.

use aether_world::registry::blocks;
use aether_world::BlockStateId;

/// The map colour of a block, by namespaced or bare block name.
pub fn block_color(name: &str) -> [u8; 3] {
    let n = name.strip_prefix("minecraft:").unwrap_or(name);
    match n {
        "grass_block" => [96, 158, 64],
        "short_grass" | "tall_grass" | "fern" | "large_fern" | "bush" => [84, 150, 58],
        "dirt" | "coarse_dirt" | "rooted_dirt" => [134, 96, 67],
        "podzol" => [106, 76, 40],
        "mycelium" => [111, 99, 105],
        "sand" => [219, 207, 163],
        "red_sand" => [190, 102, 33],
        "sandstone" => [216, 203, 155],
        "gravel" => [131, 127, 126],
        "stone" | "andesite" | "tuff" | "cobblestone" | "mossy_cobblestone" => [125, 125, 125],
        "granite" => [149, 103, 85],
        "diorite" => [188, 188, 188],
        "deepslate" => [80, 80, 82],
        "calcite" => [223, 224, 220],
        "water" => [52, 90, 210],
        "ice" | "packed_ice" | "blue_ice" => [160, 190, 250],
        "snow" | "snow_block" | "powder_snow" => [248, 250, 252],
        "lava" => [230, 90, 20],
        "clay" => [160, 166, 179],
        "mud" => [60, 57, 61],
        "terracotta" => [152, 94, 67],
        "white_terracotta" => [209, 178, 161],
        "orange_terracotta" => [161, 83, 37],
        "yellow_terracotta" => [186, 133, 35],
        "brown_terracotta" => [77, 51, 35],
        "red_terracotta" => [143, 61, 46],
        "light_gray_terracotta" => [135, 107, 98],
        "cactus" => [85, 127, 43],
        "sugar_cane" => [148, 192, 101],
        "bamboo" => [93, 144, 19],
        "lily_pad" => [32, 128, 48],
        "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass" => [40, 110, 90],
        "moss_block" | "moss_carpet" => [89, 109, 45],
        "dead_bush" | "short_dry_grass" | "tall_dry_grass" => [148, 112, 60],
        "pumpkin" => [198, 118, 24],
        "melon" => [111, 145, 30],
        "brown_mushroom_block" => [149, 111, 81],
        "red_mushroom_block" => [200, 46, 45],
        "mushroom_stem" => [203, 196, 185],
        "bedrock" => [40, 40, 40],
        _ => fallback(n),
    }
}

/// A stable colour for a block the palette does not name.
fn fallback(n: &str) -> [u8; 3] {
    if n.ends_with("_leaves") {
        return match n {
            "birch_leaves" => [96, 140, 60],
            "spruce_leaves" => [56, 90, 56],
            "cherry_leaves" => [233, 178, 205],
            "azalea_leaves" | "flowering_azalea_leaves" => [90, 120, 40],
            "pale_oak_leaves" => [160, 168, 150],
            _ => [48, 110, 30],
        };
    }
    if n.ends_with("_log") || n.ends_with("_wood") {
        return [104, 82, 50];
    }
    if is_flower(n) {
        return [220, 80, 160];
    }
    if n.contains("coral") {
        return [230, 120, 180];
    }
    if n.contains("ore") {
        return [150, 150, 150];
    }
    hash_color(n)
}

fn is_flower(n: &str) -> bool {
    const PARTS: &[&str] = &[
        "tulip",
        "poppy",
        "dandelion",
        "orchid",
        "allium",
        "bluet",
        "daisy",
        "cornflower",
        "lily_of_the_valley",
        "rose",
        "lilac",
        "peony",
        "sunflower",
        "petals",
        "wildflowers",
    ];
    PARTS.iter().any(|p| n.contains(p))
}

/// A deterministic colour from a name, so an unknown block is stable and
/// distinguishable rather than a single "unknown" grey.
fn hash_color(n: &str) -> [u8; 3] {
    let h = n
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    [
        (h & 0xff) as u8,
        ((h >> 8) & 0xff) as u8,
        ((h >> 16) & 0xff) as u8,
    ]
}

/// The map colour of a block state id.
///
/// Vanilla states answer from the compile-time table. An id the table does not
/// know — a modded block this world interned — has no name to look up here, so
/// it gets a colour from the id, which is stable for the life of the save.
pub fn color_of(id: BlockStateId) -> [u8; 3] {
    match blocks::block_of_state(id) {
        Some((_, name)) => block_color(name),
        None => hash_color(&id.raw().to_string()),
    }
}

/// Whether a block name is one of the three air states.
///
/// The map's top-block scan treats all three as "nothing here", matching the
/// offline `vanilla_map` example.
pub fn is_air(name: &str) -> bool {
    matches!(
        name.strip_prefix("minecraft:").unwrap_or(name),
        "air" | "cave_air" | "void_air"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_blocks_have_expected_colors() {
        assert_eq!(block_color("minecraft:grass_block"), [96, 158, 64]);
        assert_eq!(block_color("water"), [52, 90, 210]);
        assert_eq!(block_color("stone"), [125, 125, 125]);
        assert_eq!(block_color("oak_leaves"), [48, 110, 30]);
        assert_eq!(block_color("oak_log"), [104, 82, 50]);
        assert_eq!(block_color("diamond_ore"), [150, 150, 150]);
    }

    #[test]
    fn unknown_blocks_are_stable_and_distinct() {
        assert_eq!(block_color("mod:widget"), block_color("mod:widget"));
        assert_ne!(block_color("mod:widget"), block_color("mod:gadget"));
    }

    #[test]
    fn color_of_vanilla_id_matches_name() {
        assert_eq!(
            color_of(aether_world::registry::ids::GRASS_BLOCK),
            [96, 158, 64]
        );
        assert_eq!(color_of(aether_world::registry::ids::WATER), [52, 90, 210]);
        assert_eq!(
            color_of(aether_world::registry::ids::AIR),
            block_color("air")
        );
    }

    #[test]
    fn air_states_are_recognized() {
        assert!(is_air("minecraft:air"));
        assert!(is_air("cave_air"));
        assert!(!is_air("stone"));
    }
}
