//! Lookups over the generated game data in [`super::data`], plus the handful
//! of numbers vanilla keeps in code rather than data: weapon damage, armour
//! points, smelting, fuel.
//!
//! Every lookup takes a `minecraft:`-qualified name — the same anchoring the
//! rest of the server uses — and the generated tables are sorted by name, so
//! each is a binary search.

use super::data;

/// What eating an item restores.
#[derive(Debug, Clone, Copy)]
pub struct Food {
    pub nutrition: u8,
    pub saturation: f32,
}

/// One possible drop from a block or a mob.
#[derive(Debug, Clone, Copy)]
pub struct LootDrop {
    pub item: &'static str,
    pub chance: f32,
    pub min: u8,
    pub max: u8,
    /// `1`: only with silk touch; `-1`: only without; `0`: either.
    pub silk: i8,
    /// Only when a player made the kill.
    pub player_kill: bool,
}

/// A shaped crafting recipe, `w`×`h`, cells row-major, `""` for empty.
#[derive(Debug)]
pub struct Shaped {
    pub w: usize,
    pub h: usize,
    pub cells: &'static [&'static str],
    pub result: &'static str,
    pub count: u8,
}

/// A shapeless crafting recipe.
#[derive(Debug)]
pub struct Shapeless {
    pub ingredients: &'static [&'static str],
    pub result: &'static str,
    pub count: u8,
}

fn find<'a, T>(table: &'a [(&'static str, T)], name: &str) -> Option<&'a T> {
    table
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| &table[i].1)
}

/// The most of `item` one slot may hold. 64 for anything unknown.
pub fn max_stack(item: &str) -> u8 {
    data::ITEMS
        .binary_search_by(|(n, _, _)| (*n).cmp(item))
        .map(|i| data::ITEMS[i].1)
        .unwrap_or(64)
}

/// How many uses a tool or armour piece has, `0` for items that do not wear.
pub fn max_durability(item: &str) -> u16 {
    data::ITEMS
        .binary_search_by(|(n, _, _)| (*n).cmp(item))
        .map(|i| data::ITEMS[i].2)
        .unwrap_or(0)
}

/// Whether `item` is a known item at all.
pub fn is_item(item: &str) -> bool {
    data::ITEMS
        .binary_search_by(|(n, _, _)| (*n).cmp(item))
        .is_ok()
}

/// What eating `item` restores, if it is food.
pub fn food(item: &str) -> Option<Food> {
    find(&data::FOODS, item).copied()
}

/// A block's mining data.
pub struct BlockInfo {
    /// Seconds-ish hardness; negative is unbreakable.
    pub hardness: f32,
    pub material: &'static str,
    /// Tools that harvest it; empty means anything (a hand included).
    pub tools: &'static [&'static str],
    pub drops: &'static [LootDrop],
}

/// Mining data for the block called `name`.
pub fn block_info(name: &str) -> Option<BlockInfo> {
    data::BLOCKS
        .binary_search_by(|(n, ..)| (*n).cmp(name))
        .ok()
        .map(|i| {
            let (_, hardness, material, tools, drops) = data::BLOCKS[i];
            BlockInfo {
                hardness,
                material,
                tools,
                drops,
            }
        })
}

/// The speed multiplier `tool` mines `material` at, `1.0` when it is not the
/// right tool. A material string may name several (`plant;mineable/axe`).
fn tool_speed(material: &str, tool: Option<&str>) -> f32 {
    let Some(tool) = tool else { return 1.0 };
    let mut best = 1.0f32;
    for m in material.split(';') {
        if let Some(list) = find(&data::MATERIALS, m) {
            if let Some((_, s)) = list.iter().find(|(t, _)| *t == tool) {
                best = best.max(*s);
            }
        }
    }
    best
}

/// Whether `tool` (or a bare hand) gets the drops of a block.
pub fn can_harvest(block: &str, tool: Option<&str>) -> bool {
    match block_info(block) {
        Some(info) if !info.tools.is_empty() => tool.is_some_and(|t| info.tools.contains(&t)),
        _ => true,
    }
}

/// How many ticks breaking `block` takes with `tool`, vanilla's formula
/// (no enchantments or effects). `None` for an unbreakable block; `Some(0)`
/// for one that breaks instantly.
pub fn dig_ticks(block: &str, tool: Option<&str>, on_ground: bool, in_water: bool) -> Option<u32> {
    let info = block_info(block)?;
    if info.hardness < 0.0 {
        return None;
    }
    if info.hardness == 0.0 {
        return Some(0);
    }
    let mut speed = tool_speed(info.material, tool);
    if in_water {
        speed /= 5.0;
    }
    if !on_ground {
        speed /= 5.0;
    }
    let divisor = if can_harvest(block, tool) {
        30.0
    } else {
        100.0
    };
    let per_tick = speed / info.hardness / divisor;
    if per_tick >= 1.0 {
        return Some(0);
    }
    Some((1.0 / per_tick).ceil() as u32)
}

/// A small, fast, non-cryptographic RNG for loot rolls and AI.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    /// Seeded from the clock, for code that has no seed of its own.
    pub fn from_time() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9);
        Self::new(t ^ 0x2545_F491_4F6C_DD1D)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in `0.0..1.0`.
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in `0.0..1.0`.
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i32
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.f32() < p
    }
}

fn roll(drops: &[LootDrop], silk: bool, player_kill: bool, rng: &mut Rng) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    for d in drops {
        if (d.silk == 1 && !silk) || (d.silk == -1 && silk) || (d.player_kill && !player_kill) {
            continue;
        }
        if d.chance < 1.0 && !rng.chance(d.chance) {
            continue;
        }
        let n = rng.range(d.min as i32, d.max.max(d.min) as i32);
        if n > 0 {
            out.push((d.item.to_owned(), n as u8));
        }
    }
    out
}

/// What breaking `block` with `tool` drops.
///
/// The generated loot covers most blocks; a few whose real tables have
/// conditions the data flattens away are written out here.
pub fn block_drops(block: &str, tool: Option<&str>, rng: &mut Rng) -> Vec<(String, u8)> {
    if !can_harvest(block, tool) {
        return Vec::new();
    }
    let shears = tool == Some("minecraft:shears");
    let short = block.strip_prefix("minecraft:").unwrap_or(block);
    // Leaves: saplings 5 %, sticks 2 %, apples 0.5 % from oak; the leaves
    // themselves only to shears.
    if short.ends_with("_leaves") {
        if shears {
            return vec![(block.to_owned(), 1)];
        }
        let mut out = Vec::new();
        let sapling = match short {
            "azalea_leaves" => "minecraft:azalea".to_owned(),
            "flowering_azalea_leaves" => "minecraft:flowering_azalea".to_owned(),
            "mangrove_leaves" => String::new(),
            _ => format!("minecraft:{}_sapling", short.trim_end_matches("_leaves")),
        };
        if !sapling.is_empty() && is_item(&sapling) && rng.chance(0.05) {
            out.push((sapling, 1));
        }
        if rng.chance(0.02) {
            out.push(("minecraft:stick".into(), rng.range(1, 2) as u8));
        }
        if matches!(short, "oak_leaves" | "dark_oak_leaves") && rng.chance(0.005) {
            out.push(("minecraft:apple".into(), 1));
        }
        return out;
    }
    match short {
        "short_grass" | "tall_grass" | "fern" | "large_fern" => {
            if shears {
                return vec![(block.to_owned(), 1)];
            }
            if rng.chance(0.125) {
                return vec![("minecraft:wheat_seeds".into(), 1)];
            }
            return Vec::new();
        }
        "gravel" => {
            let item = if rng.chance(0.1) {
                "minecraft:flint"
            } else {
                "minecraft:gravel"
            };
            return vec![(item.into(), 1)];
        }
        "glass" | "glass_pane" | "ice" => return Vec::new(),
        "snow" => return vec![("minecraft:snowball".into(), 1)],
        "wheat" => {
            return vec![
                ("minecraft:wheat".into(), 1),
                ("minecraft:wheat_seeds".into(), rng.range(1, 3) as u8),
            ]
        }
        _ => {}
    }
    if let Some(info) = block_info(block) {
        if !info.drops.is_empty() {
            return roll(info.drops, false, false, rng);
        }
    }
    // Most blocks drop themselves; the data omits some of them.
    if is_item(block) {
        vec![(block.to_owned(), 1)]
    } else {
        Vec::new()
    }
}

/// What killing a `kind` mob drops.
pub fn mob_drops(kind: &str, player_kill: bool, rng: &mut Rng) -> Vec<(String, u8)> {
    let Some(drops) = find(&data::ENTITY_LOOT, kind) else {
        return Vec::new();
    };
    let mut out = roll(drops, false, player_kill, rng);
    // The data lists "0–2" ranges as "1"; vanilla's common drops vary.
    for (item, n) in out.iter_mut() {
        if matches!(
            item.as_str(),
            "minecraft:rotten_flesh"
                | "minecraft:bone"
                | "minecraft:arrow"
                | "minecraft:string"
                | "minecraft:gunpowder"
                | "minecraft:feather"
                | "minecraft:leather"
        ) {
            *n = rng.range(0, 2) as u8;
        }
    }
    out.retain(|(_, n)| *n > 0);
    out
}

/// The protocol id, width and height of an entity type.
pub fn entity_type(name: &str) -> Option<(i32, f32, f32)> {
    data::ENTITY_TYPES
        .binary_search_by(|(n, ..)| (*n).cmp(name))
        .ok()
        .map(|i| {
            let (_, id, w, h, _) = data::ENTITY_TYPES[i];
            (id, w, h)
        })
}

/// Attack damage and attacks per second for what is held, vanilla 1.9+.
pub fn weapon(item: Option<&str>) -> (f32, f32) {
    let Some(item) = item else { return (1.0, 4.0) };
    let short = item.strip_prefix("minecraft:").unwrap_or(item);
    let tier = |prefix: &str| -> f32 {
        match prefix {
            "wooden" | "golden" => 0.0,
            "stone" | "copper" => 1.0,
            "iron" => 2.0,
            "diamond" => 3.0,
            "netherite" => 4.0,
            _ => 0.0,
        }
    };
    if let Some(p) = short.strip_suffix("_sword") {
        return (4.0 + tier(p), 1.6);
    }
    if let Some(p) = short.strip_suffix("_axe") {
        let dmg = match p {
            "wooden" | "golden" => 7.0,
            "stone" | "copper" | "iron" | "diamond" => 9.0,
            _ => 10.0,
        };
        let speed = match p {
            "wooden" | "stone" | "copper" => 0.8,
            "iron" => 0.9,
            _ => 1.0,
        };
        return (dmg, speed);
    }
    if let Some(p) = short.strip_suffix("_pickaxe") {
        return (2.0 + tier(p), 1.2);
    }
    if let Some(p) = short.strip_suffix("_shovel") {
        return (2.5 + tier(p), 1.0);
    }
    if short.ends_with("_hoe") {
        return (1.0, 1.0 + tier(short.trim_end_matches("_hoe")));
    }
    if short == "trident" {
        return (9.0, 1.1);
    }
    if short == "mace" {
        return (6.0, 0.6);
    }
    (1.0, 4.0)
}

/// Which armour slot (window index 5..=8) an item goes in, if any.
pub fn armor_slot(item: &str) -> Option<usize> {
    let short = item.strip_prefix("minecraft:").unwrap_or(item);
    if short.ends_with("_helmet")
        || short == "turtle_helmet"
        || short.ends_with("_head")
        || short.ends_with("_skull")
        || short == "carved_pumpkin"
    {
        Some(5)
    } else if short.ends_with("_chestplate") || short == "elytra" {
        Some(6)
    } else if short.ends_with("_leggings") {
        Some(7)
    } else if short.ends_with("_boots") {
        Some(8)
    } else {
        None
    }
}

/// Armour points and toughness of one piece.
pub fn armor_points(item: &str) -> (f32, f32) {
    let short = item.strip_prefix("minecraft:").unwrap_or(item);
    let (material, piece) = match short.rsplit_once('_') {
        Some(v) => v,
        None => return (0.0, 0.0),
    };
    let pts: [f32; 4] = match material {
        "leather" => [1.0, 3.0, 2.0, 1.0],
        "golden" | "chainmail" => [2.0, 5.0, if material == "golden" { 3.0 } else { 4.0 }, 1.0],
        "copper" => [2.0, 4.0, 3.0, 1.0],
        "iron" => [2.0, 6.0, 5.0, 2.0],
        "diamond" => [3.0, 8.0, 6.0, 3.0],
        "netherite" => [3.0, 8.0, 6.0, 3.0],
        "turtle" => [2.0, 0.0, 0.0, 0.0],
        _ => return (0.0, 0.0),
    };
    let tough = match material {
        "diamond" => 2.0,
        "netherite" => 3.0,
        _ => 0.0,
    };
    let i = match piece {
        "helmet" => 0,
        "chestplate" => 1,
        "leggings" => 2,
        "boots" => 3,
        _ => return (0.0, 0.0),
    };
    (pts[i], tough)
}

/// Vanilla's armour damage reduction.
pub fn after_armor(damage: f32, armor: f32, toughness: f32) -> f32 {
    let effective = (armor - damage / (2.0 + toughness / 4.0))
        .max(armor / 5.0)
        .min(20.0);
    damage * (1.0 - effective / 25.0)
}

/// Match a crafting grid against every recipe.
///
/// `grid` is row-major, `width` wide (2 or 3). Returns the result and count.
pub fn craft(grid: &[Option<&str>], width: usize) -> Option<(&'static str, u8)> {
    let height = grid.len() / width;
    // The occupied bounding box.
    let mut min_x = width;
    let mut max_x = 0;
    let mut min_y = height;
    let mut max_y = 0;
    let mut items: Vec<&str> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(it) = grid[y * width + x] {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                items.push(it);
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    let (bw, bh) = (max_x - min_x + 1, max_y - min_y + 1);
    let cell = |x: usize, y: usize| grid[(min_y + y) * width + min_x + x].unwrap_or("");
    for r in data::SHAPED.iter() {
        if r.w != bw || r.h != bh {
            continue;
        }
        let straight = (0..bh).all(|y| (0..bw).all(|x| r.cells[y * bw + x] == cell(x, y)));
        let mirrored = !straight
            && (0..bh).all(|y| (0..bw).all(|x| r.cells[y * bw + (bw - 1 - x)] == cell(x, y)));
        if straight || mirrored {
            return Some((r.result, r.count));
        }
    }
    let mut have = items.clone();
    have.sort_unstable();
    for r in data::SHAPELESS.iter() {
        if r.ingredients.len() != have.len() {
            continue;
        }
        let mut want: Vec<&str> = r.ingredients.to_vec();
        want.sort_unstable();
        if want == have {
            return Some((r.result, r.count));
        }
    }
    None
}

/// What smelting `item` makes.
pub fn smelt(item: &str) -> Option<&'static str> {
    let short = item.strip_prefix("minecraft:")?;
    Some(match short {
        "raw_iron" | "iron_ore" | "deepslate_iron_ore" => "minecraft:iron_ingot",
        "raw_gold" | "gold_ore" | "deepslate_gold_ore" | "nether_gold_ore" => {
            "minecraft:gold_ingot"
        }
        "raw_copper" | "copper_ore" | "deepslate_copper_ore" => "minecraft:copper_ingot",
        "coal_ore" | "deepslate_coal_ore" => "minecraft:coal",
        "diamond_ore" | "deepslate_diamond_ore" => "minecraft:diamond",
        "emerald_ore" | "deepslate_emerald_ore" => "minecraft:emerald",
        "lapis_ore" | "deepslate_lapis_ore" => "minecraft:lapis_lazuli",
        "redstone_ore" | "deepslate_redstone_ore" => "minecraft:redstone",
        "nether_quartz_ore" => "minecraft:quartz",
        "ancient_debris" => "minecraft:netherite_scrap",
        "sand" | "red_sand" => "minecraft:glass",
        "cobblestone" => "minecraft:stone",
        "stone" => "minecraft:smooth_stone",
        "cobbled_deepslate" => "minecraft:deepslate",
        "stone_bricks" => "minecraft:cracked_stone_bricks",
        "sandstone" => "minecraft:smooth_sandstone",
        "red_sandstone" => "minecraft:smooth_red_sandstone",
        "quartz_block" => "minecraft:smooth_quartz",
        "netherrack" => "minecraft:nether_brick",
        "clay_ball" => "minecraft:brick",
        "clay" => "minecraft:terracotta",
        "cactus" => "minecraft:green_dye",
        "kelp" => "minecraft:dried_kelp",
        "wet_sponge" => "minecraft:sponge",
        "chorus_fruit" => "minecraft:popped_chorus_fruit",
        "beef" => "minecraft:cooked_beef",
        "porkchop" => "minecraft:cooked_porkchop",
        "chicken" => "minecraft:cooked_chicken",
        "mutton" => "minecraft:cooked_mutton",
        "rabbit" => "minecraft:cooked_rabbit",
        "cod" => "minecraft:cooked_cod",
        "salmon" => "minecraft:cooked_salmon",
        "potato" => "minecraft:baked_potato",
        s if s.ends_with("_log") || s.ends_with("_wood") => {
            if s.contains("crimson") || s.contains("warped") {
                return None;
            }
            "minecraft:charcoal"
        }
        _ => return None,
    })
}

/// How many ticks `item` burns as furnace fuel.
pub fn fuel_ticks(item: &str) -> u32 {
    let Some(short) = item.strip_prefix("minecraft:") else {
        return 0;
    };
    match short {
        "lava_bucket" => 20000,
        "coal_block" => 16000,
        "dried_kelp_block" => 4001,
        "blaze_rod" => 2400,
        "coal" | "charcoal" => 1600,
        "stick" | "bamboo" => 100,
        "scaffolding" => 50,
        s if s.ends_with("_sapling") || s.ends_with("_wool") || s.ends_with("_carpet") => 100,
        s if s.ends_with("_planks")
            || s.ends_with("_log")
            || s.ends_with("_wood")
            || s.ends_with("_fence")
            || s.ends_with("_fence_gate")
            || s.ends_with("_stairs") && is_wooden(s)
            || s == "crafting_table"
            || s == "chest"
            || s == "bookshelf"
            || s == "barrel" =>
        {
            if s.contains("crimson") || s.contains("warped") {
                0
            } else {
                300
            }
        }
        s if s.ends_with("_slab") && is_wooden(s) => 150,
        s if s.starts_with("wooden_") => 200,
        _ => 0,
    }
}

fn is_wooden(s: &str) -> bool {
    [
        "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry", "pale_oak",
        "bamboo",
    ]
    .iter()
    .any(|w| s.starts_with(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_and_durability_come_from_the_data() {
        assert_eq!(max_stack("minecraft:stone"), 64);
        assert_eq!(max_stack("minecraft:diamond_sword"), 1);
        assert_eq!(max_stack("minecraft:ender_pearl"), 16);
        assert_eq!(max_durability("minecraft:diamond_sword"), 1561);
    }

    #[test]
    fn stone_needs_a_pickaxe_and_drops_cobblestone() {
        let mut rng = Rng::new(1);
        assert!(block_drops("minecraft:stone", None, &mut rng).is_empty());
        let d = block_drops(
            "minecraft:stone",
            Some("minecraft:wooden_pickaxe"),
            &mut rng,
        );
        assert_eq!(d, vec![("minecraft:cobblestone".to_string(), 1)]);
        let d = block_drops("minecraft:grass_block", None, &mut rng);
        assert_eq!(d, vec![("minecraft:dirt".to_string(), 1)]);
    }

    #[test]
    fn dig_times_follow_the_vanilla_formula() {
        // Stone by hand: 1.5 hardness, wrong tool → 1/(1/1.5/100) = 150 ticks.
        assert_eq!(dig_ticks("minecraft:stone", None, true, false), Some(150));
        // With a wooden pickaxe (speed 2): 1/(2/1.5/30) = 22.5 → 23.
        assert_eq!(
            dig_ticks(
                "minecraft:stone",
                Some("minecraft:wooden_pickaxe"),
                true,
                false
            ),
            Some(23)
        );
        assert_eq!(dig_ticks("minecraft:bedrock", None, true, false), None);
        assert_eq!(
            dig_ticks("minecraft:short_grass", None, true, false),
            Some(0)
        );
    }

    #[test]
    fn planks_sticks_and_tables_craft() {
        let log = Some("minecraft:oak_log");
        assert_eq!(
            craft(&[log, None, None, None], 2),
            Some(("minecraft:oak_planks", 4))
        );
        let p = Some("minecraft:oak_planks");
        assert_eq!(
            craft(&[p, p, p, p], 2),
            Some(("minecraft:crafting_table", 1))
        );
        assert_eq!(craft(&[None, p, None, p], 2), Some(("minecraft:stick", 4)));
        let c = Some("minecraft:cobblestone");
        let s = Some("minecraft:stick");
        let grid = [c, c, c, None, s, None, None, s, None];
        assert_eq!(craft(&grid, 3), Some(("minecraft:stone_pickaxe", 1)));
        assert_eq!(craft(&[c, None, None, None], 2), None);
    }

    #[test]
    fn mirrored_shapes_match() {
        let p = Some("minecraft:oak_planks");
        let s = Some("minecraft:stick");
        // An axe drawn facing either way.
        let left = [p, p, None, p, s, None, None, s, None];
        let right = [None, p, p, None, s, p, None, s, None];
        assert_eq!(craft(&left, 3), Some(("minecraft:wooden_axe", 1)));
        assert_eq!(craft(&right, 3), Some(("minecraft:wooden_axe", 1)));
    }

    #[test]
    fn entity_types_resolve() {
        assert_eq!(entity_type("minecraft:zombie").map(|t| t.0), Some(150));
        assert_eq!(entity_type("minecraft:item").map(|t| t.0), Some(71));
    }
}
