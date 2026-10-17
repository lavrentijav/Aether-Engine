//! The `minecraft:enchantment` registry entries `/enchant` resolves names
//! through, with the 1.21.11 ids, maximum levels and supported-item category
//! from the vanilla data pack.
//!
//! The engine does not yet run an enchanting table, so this is deliberately
//! just the lookup the command needs: a name (with or without the
//! `minecraft:` namespace) to its definition, and whether an item belongs to
//! the category that enchantment applies to. It is the same data the future
//! enchanting-table work (T2/E1) will index.

/// One vanilla enchantment: canonical name, display name, registry id,
/// maximum level and the `enchantable/…` category it applies to.
pub static ENCHANTMENTS: &[(&str, &str, u8, u8, &str)] = &[
    ("aqua_affinity", "Aqua Affinity", 0, 1, "head_armor"),
    ("bane_of_arthropods", "Bane of Arthropods", 1, 5, "weapon"),
    ("binding_curse", "Curse of Binding", 2, 1, "equippable"),
    ("blast_protection", "Blast Protection", 3, 4, "armor"),
    ("breach", "Breach", 4, 4, "mace"),
    ("channeling", "Channeling", 5, 1, "trident"),
    ("density", "Density", 6, 5, "mace"),
    ("depth_strider", "Depth Strider", 7, 3, "foot_armor"),
    ("efficiency", "Efficiency", 8, 5, "mining"),
    ("feather_falling", "Feather Falling", 9, 4, "foot_armor"),
    ("fire_aspect", "Fire Aspect", 10, 2, "fire_aspect"),
    ("fire_protection", "Fire Protection", 11, 4, "armor"),
    ("flame", "Flame", 12, 1, "bow"),
    ("fortune", "Fortune", 13, 3, "mining_loot"),
    ("frost_walker", "Frost Walker", 14, 2, "foot_armor"),
    ("impaling", "Impaling", 15, 5, "trident"),
    ("infinity", "Infinity", 16, 1, "bow"),
    ("knockback", "Knockback", 17, 2, "melee_weapon"),
    ("looting", "Looting", 18, 3, "melee_weapon"),
    ("loyalty", "Loyalty", 19, 3, "trident"),
    ("luck_of_the_sea", "Luck of the Sea", 20, 3, "fishing"),
    ("lunge", "Lunge", 21, 3, "lunge"),
    ("lure", "Lure", 22, 3, "fishing"),
    ("mending", "Mending", 23, 1, "durability"),
    ("multishot", "Multishot", 24, 1, "crossbow"),
    ("piercing", "Piercing", 25, 4, "crossbow"),
    ("power", "Power", 26, 5, "bow"),
    (
        "projectile_protection",
        "Projectile Protection",
        27,
        4,
        "armor",
    ),
    ("protection", "Protection", 28, 4, "armor"),
    ("punch", "Punch", 29, 2, "bow"),
    ("quick_charge", "Quick Charge", 30, 3, "crossbow"),
    ("respiration", "Respiration", 31, 3, "head_armor"),
    ("riptide", "Riptide", 32, 3, "trident"),
    ("sharpness", "Sharpness", 33, 5, "sharp_weapon"),
    ("silk_touch", "Silk Touch", 34, 1, "mining_loot"),
    ("smite", "Smite", 35, 5, "weapon"),
    ("soul_speed", "Soul Speed", 36, 3, "foot_armor"),
    ("sweeping_edge", "Sweeping Edge", 37, 3, "sweeping"),
    ("swift_sneak", "Swift Sneak", 38, 3, "leg_armor"),
    ("thorns", "Thorns", 39, 3, "armor"),
    ("unbreaking", "Unbreaking", 40, 3, "durability"),
    ("vanishing_curse", "Curse of Vanishing", 41, 1, "vanishing"),
    ("wind_burst", "Wind Burst", 42, 3, "mace"),
];

/// One resolved enchantment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Def {
    pub id: u8,
    pub name: &'static str,
    pub display: &'static str,
    pub max_level: u8,
    pub category: &'static str,
}

fn short(name: &str) -> &str {
    name.strip_prefix("minecraft:")
        .or_else(|| name.strip_prefix("aether:"))
        .unwrap_or(name)
}

/// The definition called `name`, with or without a namespace.
pub fn by_name(name: &str) -> Option<Def> {
    let short = short(name);
    ENCHANTMENTS.iter().find(|(n, ..)| *n == short).map(
        |(name, display, id, max_level, category)| Def {
            id: *id,
            name,
            display,
            max_level: *max_level,
            category,
        },
    )
}

/// The 1.21.11 display name of enchantment `id`.
pub fn display(id: u8) -> &'static str {
    ENCHANTMENTS
        .iter()
        .find(|(.., i, _, _)| *i == id)
        .map_or("Unknown enchantment", |(_, display, ..)| *display)
}

fn is_helmet(s: &str) -> bool {
    s.ends_with("_helmet")
        || s == "turtle_helmet"
        || s.ends_with("_head")
        || s.ends_with("_skull")
        || s == "carved_pumpkin"
}

fn is_chestplate(s: &str) -> bool {
    s.ends_with("_chestplate") || s == "elytra" || s == "wolf_armor"
}

fn is_leggings(s: &str) -> bool {
    s.ends_with("_leggings")
}

fn is_boots(s: &str) -> bool {
    s.ends_with("_boots")
}

fn is_armor(s: &str) -> bool {
    is_helmet(s) || is_chestplate(s) || is_leggings(s) || is_boots(s)
}

fn is_sword(s: &str) -> bool {
    s.ends_with("_sword")
}

fn is_axe(s: &str) -> bool {
    s.ends_with("_axe")
}

fn is_pickaxe(s: &str) -> bool {
    s.ends_with("_pickaxe")
}

fn is_shovel(s: &str) -> bool {
    s.ends_with("_shovel")
}

fn is_hoe(s: &str) -> bool {
    s.ends_with("_hoe")
}

fn is_spear(s: &str) -> bool {
    s.ends_with("_spear")
}

fn is_mining_tool(s: &str) -> bool {
    is_pickaxe(s) || is_shovel(s) || is_axe(s) || is_hoe(s) || s == "shears"
}

fn is_melee_weapon(s: &str) -> bool {
    is_sword(s) || is_axe(s)
}

fn is_damageable(item: &str) -> bool {
    crate::game::tables::max_durability(item) > 0
}

/// Whether `def` can be applied to an item of this name, following the
/// vanilla `enchantable/…` tags the category names. A book accepts every
/// non-treasure enchantment, as vanilla hardcodes.
pub fn can_enchant(item: &str, def: &Def) -> bool {
    let s = short(item);
    if s == "book" || s == "enchanted_book" {
        return def.category != "mace" && def.category != "trident";
    }
    item_in_category(item, def.category)
}

fn item_in_category(item: &str, cat: &str) -> bool {
    let s = short(item);
    if s == "book" || s == "enchanted_book" {
        return true;
    }
    match cat {
        "head_armor" => is_helmet(s),
        "foot_armor" => is_boots(s),
        "leg_armor" => is_leggings(s),
        "armor" | "chest_armor" | "armor_enchanted" => is_armor(s),
        "weapon" | "sharp_weapon" | "melee_weapon" => is_melee_weapon(s),
        "sweeping" => is_sword(s),
        "mace" => s == "mace",
        "trident" => s == "trident",
        "bow" => s == "bow",
        "crossbow" => s == "crossbow",
        "fishing" | "fishing_rod" => s == "fishing_rod",
        "durability" => is_damageable(item),
        "mining" | "mining_loot" => is_mining_tool(s),
        "fire_aspect" => is_melee_weapon(s) || is_spear(s) || s == "mace",
        "lunge" => is_spear(s),
        "equippable" => is_armor(s) || s == "elytra" || is_spear(s),
        "vanishing" => true,
        // An unknown category (a custom tag the engine cannot resolve) is
        // permissive rather than exclusive, as in the future custom
        // enchantment registry.
        _ => true,
    }
}

/// A human-readable item name for command feedback: `diamond_sword` →
/// `Diamond Sword`. The English table is the client's; the server does not
/// carry it, so the same rule the selector layer uses for entity names is
/// applied here.
pub fn item_display(item: &str) -> String {
    let short = short(item);
    let mut out = String::with_capacity(short.len());
    for (i, word) in short.split('_').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_with_or_without_the_namespace() {
        assert_eq!(by_name("sharpness").unwrap().id, 33);
        assert_eq!(by_name("minecraft:sharpness").unwrap().id, 33);
        assert!(by_name("not_an_enchantment").is_none());
    }

    #[test]
    fn the_vanilla_categories_accept_the_right_items() {
        let sharp = by_name("sharpness").unwrap();
        assert!(can_enchant("minecraft:diamond_sword", &sharp));
        assert!(can_enchant("minecraft:diamond_axe", &sharp));
        assert!(!can_enchant("minecraft:diamond_pickaxe", &sharp));
        assert!(!can_enchant("minecraft:stone", &sharp));

        let eff = by_name("efficiency").unwrap();
        assert!(can_enchant("minecraft:diamond_pickaxe", &eff));
        assert!(can_enchant("minecraft:shears", &eff));
        assert!(!can_enchant("minecraft:diamond_sword", &eff));

        let unb = by_name("unbreaking").unwrap();
        assert!(can_enchant("minecraft:diamond_pickaxe", &unb));
        assert!(!can_enchant("minecraft:stone", &unb));
    }

    #[test]
    fn a_book_takes_non_treasure_enchantments() {
        let book = "minecraft:book";
        assert!(can_enchant(book, &by_name("sharpness").unwrap()));
        assert!(can_enchant(book, &by_name("silk_touch").unwrap()));
        assert!(!can_enchant(book, &by_name("wind_burst").unwrap()));
    }
}
