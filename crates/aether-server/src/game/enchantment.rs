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
pub static ENCHANTMENTS: &[(&str, &str, u8, u8, &str, &str)] = &[
    ("aqua_affinity", "Aqua Affinity", 0, 1, "head_armor", ""),
    (
        "bane_of_arthropods",
        "Bane of Arthropods",
        1,
        5,
        "weapon",
        "damage",
    ),
    ("binding_curse", "Curse of Binding", 2, 1, "equippable", ""),
    (
        "blast_protection",
        "Blast Protection",
        3,
        4,
        "armor",
        "armor",
    ),
    ("breach", "Breach", 4, 4, "mace", "damage"),
    ("channeling", "Channeling", 5, 1, "trident", ""),
    ("density", "Density", 6, 5, "mace", "damage"),
    (
        "depth_strider",
        "Depth Strider",
        7,
        3,
        "foot_armor",
        "boots",
    ),
    ("efficiency", "Efficiency", 8, 5, "mining", ""),
    ("feather_falling", "Feather Falling", 9, 4, "foot_armor", ""),
    ("fire_aspect", "Fire Aspect", 10, 2, "fire_aspect", ""),
    (
        "fire_protection",
        "Fire Protection",
        11,
        4,
        "armor",
        "armor",
    ),
    ("flame", "Flame", 12, 1, "bow", ""),
    ("fortune", "Fortune", 13, 3, "mining_loot", "mining"),
    ("frost_walker", "Frost Walker", 14, 2, "foot_armor", "boots"),
    ("impaling", "Impaling", 15, 5, "trident", "damage"),
    ("infinity", "Infinity", 16, 1, "bow", "bow"),
    ("knockback", "Knockback", 17, 2, "melee_weapon", ""),
    ("looting", "Looting", 18, 3, "melee_weapon", ""),
    ("loyalty", "Loyalty", 19, 3, "trident", ""),
    ("luck_of_the_sea", "Luck of the Sea", 20, 3, "fishing", ""),
    ("lunge", "Lunge", 21, 3, "lunge", ""),
    ("lure", "Lure", 22, 3, "fishing", ""),
    ("mending", "Mending", 23, 1, "durability", ""),
    ("multishot", "Multishot", 24, 1, "crossbow", "crossbow"),
    ("piercing", "Piercing", 25, 4, "crossbow", "crossbow"),
    ("power", "Power", 26, 5, "bow", ""),
    (
        "projectile_protection",
        "Projectile Protection",
        27,
        4,
        "armor",
        "armor",
    ),
    ("protection", "Protection", 28, 4, "armor", "armor"),
    ("punch", "Punch", 29, 2, "bow", ""),
    ("quick_charge", "Quick Charge", 30, 3, "crossbow", ""),
    ("respiration", "Respiration", 31, 3, "head_armor", ""),
    ("riptide", "Riptide", 32, 3, "trident", "riptide"),
    ("sharpness", "Sharpness", 33, 5, "sharp_weapon", "damage"),
    ("silk_touch", "Silk Touch", 34, 1, "mining_loot", "mining"),
    ("smite", "Smite", 35, 5, "weapon", "damage"),
    ("soul_speed", "Soul Speed", 36, 3, "foot_armor", ""),
    ("sweeping_edge", "Sweeping Edge", 37, 3, "sweeping", ""),
    ("swift_sneak", "Swift Sneak", 38, 3, "leg_armor", ""),
    ("thorns", "Thorns", 39, 3, "armor", ""),
    ("unbreaking", "Unbreaking", 40, 3, "durability", ""),
    (
        "vanishing_curse",
        "Curse of Vanishing",
        41,
        1,
        "vanishing",
        "",
    ),
    ("wind_burst", "Wind Burst", 42, 3, "mace", ""),
];

/// The enchantments in one `exclusive_set/…` tag, by registry id, from the
/// 1.21.11 data pack. Vanilla's compatibility test only consults the
/// **candidate's** set: an existing enchantment in it makes the new one
/// incompatible (which is why re-applying Sharpness at any level is refused
/// when the item already has Sharpness).
pub static EXCLUSIVE_SETS: &[(&str, &[u8])] = &[
    ("armor", &[3, 11, 27, 28]),
    ("boots", &[7, 14]),
    ("bow", &[16, 23]),
    ("crossbow", &[24, 25]),
    ("damage", &[1, 4, 6, 15, 33, 35]),
    ("mining", &[13, 34]),
    ("riptide", &[19, 5]),
];

/// One resolved enchantment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Def {
    pub id: u8,
    pub name: &'static str,
    pub display: &'static str,
    pub max_level: u8,
    pub category: &'static str,
    /// The name (without `exclusive_set/`) of the tag that conflicts with
    /// this enchantment, or `""`.
    pub exclusive: &'static str,
}

impl Def {
    /// The `minecraft:enchantable/…` tag this enchantment applies to, the
    /// data pack's `supported_items`.
    pub fn tag(&self) -> String {
        format!("minecraft:enchantable/{}", self.category)
    }
}

fn short(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// The definition called `name`, with or without a namespace.
pub fn by_name(name: &str) -> Option<Def> {
    let short = short(name);
    ENCHANTMENTS.iter().find(|(n, ..)| *n == short).map(
        |(name, display, id, max_level, category, exclusive)| Def {
            id: *id,
            name,
            display,
            max_level: *max_level,
            category,
            exclusive,
        },
    )
}

/// Whether `def` is compatible with `existing` enchantment ids, vanilla's
/// `EnchantmentHelper.isEnchantmentCompatible(existing.keySet(), candidate)`.
pub fn is_compatible(existing: &[u8], def: &Def) -> bool {
    if def.exclusive.is_empty() {
        return true;
    }
    let Some((_, set)) = EXCLUSIVE_SETS.iter().find(|(n, _)| *n == def.exclusive) else {
        return true;
    };
    !existing.iter().any(|id| set.contains(id))
}

/// The vanilla `Enchantment.getFullname(level)`: the display name followed by
/// the level numeral, e.g. `Sharpness III`. Levels above ten (which no vanilla
/// enchantment has) fall back to the Arabic number.
pub fn full_name(def: &Def, level: i32) -> String {
    format!("{} {}", def.display, numeral(level))
}

/// A Roman numeral for `1..=10`, else the decimal number.
pub fn numeral(level: i32) -> String {
    const ROMAN: &[(i32, &str)] = &[(10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    if !(1..=10).contains(&level) {
        return level.to_string();
    }
    let mut n = level;
    let mut out = String::new();
    for (value, text) in ROMAN {
        while n >= *value {
            out.push_str(text);
            n -= *value;
        }
    }
    out
}

/// The 1.21.11 display name of enchantment `id`.
pub fn display(id: u8) -> &'static str {
    ENCHANTMENTS
        .iter()
        .find(|(.., i, _, _, _)| *i == id)
        .map_or("Unknown enchantment", |(_, display, ..)| *display)
}

fn is_helmet(s: &str) -> bool {
    s.ends_with("_helmet") || s == "turtle_helmet"
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

/// Whether `def` can be applied to an item of this name, by the vanilla
/// `enchantable/…` rules. This is the fallback for a server without a data
/// pack; when one is loaded the command asks the pack's real item tags (see
/// [`Def::tag`] and `game::recipes::item_tag`). A book is not in any
/// `enchantable` tag: vanilla's enchanting *table* special-cases books, but
/// `/enchant` does not.
pub fn can_enchant(item: &str, def: &Def) -> bool {
    item_in_category(item, def.category)
}

fn item_in_category(item: &str, cat: &str) -> bool {
    let s = short(item);
    match cat {
        "head_armor" => is_helmet(s),
        "foot_armor" => is_boots(s),
        "leg_armor" => is_leggings(s),
        "armor" | "chest_armor" | "armor_enchanted" => is_armor(s),
        // `weapon` = sharp_weapon + mace; `sharp_weapon` = melee_weapon +
        // axes; `melee_weapon` = swords + spears.
        "weapon" => is_melee_weapon(s) || is_spear(s) || s == "mace",
        "sharp_weapon" => is_melee_weapon(s) || is_spear(s),
        "melee_weapon" => is_sword(s) || is_spear(s),
        "sweeping" => is_sword(s),
        "mace" => s == "mace",
        "trident" => s == "trident",
        "bow" => s == "bow",
        "crossbow" => s == "crossbow",
        "fishing" | "fishing_rod" => s == "fishing_rod",
        "durability" => is_damageable(item),
        "mining" => is_mining_tool(s),
        // Silk Touch and Fortune exclude shears.
        "mining_loot" => is_pickaxe(s) || is_shovel(s) || is_axe(s) || is_hoe(s),
        "fire_aspect" => is_sword(s) || is_spear(s) || s == "mace",
        "lunge" => is_spear(s),
        "equippable" => {
            is_armor(s)
                || s == "elytra"
                || s == "carved_pumpkin"
                || s.ends_with("_head")
                || s.ends_with("_skull")
        }
        // `enchantable/vanishing` is durability + compass + carved pumpkin +
        // skulls, not everything.
        "vanishing" => {
            is_damageable(item) || s == "compass" || s == "carved_pumpkin" || s.ends_with("_skull")
        }
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
    fn the_table_matches_the_1_21_11_data_pack() {
        // Names, ids, maximum levels and `supported_items` tags were taken
        // from `data/minecraft/enchantment/*.json`; the ids are the registry
        // order the component and the command argument use.
        assert_eq!(ENCHANTMENTS.len(), 43);
        let ids: Vec<u8> = ENCHANTMENTS.iter().map(|(.., id, _, _, _)| *id).collect();
        assert_eq!(ids, (0..=42).collect::<Vec<u8>>(), "dense registry ids");
        let sharp = by_name("sharpness").unwrap();
        assert_eq!((sharp.id, sharp.max_level), (33, 5));
        assert_eq!(sharp.tag(), "minecraft:enchantable/sharp_weapon");
        assert_eq!(by_name("wind_burst").unwrap().max_level, 3);
        assert_eq!(
            by_name("mending").unwrap().tag(),
            "minecraft:enchantable/durability"
        );
        assert!(by_name("not_an_enchantment").is_none());
    }

    #[test]
    fn names_resolve_with_or_without_the_minecraft_namespace() {
        assert_eq!(by_name("sharpness").unwrap().id, 33);
        assert_eq!(by_name("minecraft:sharpness").unwrap().id, 33);
        assert!(
            by_name("aether:sharpness").is_none(),
            "only the vanilla registry namespace resolves"
        );
        assert!(by_name("not_an_enchantment").is_none());
    }

    #[test]
    fn exclusive_sets_match_the_data_pack() {
        let sharp = by_name("sharpness").unwrap();
        assert!(is_compatible(&[], &sharp));
        assert!(is_compatible(&[40], &sharp), "unbreaking is not in damage");
        for conflicting in [33, 35, 1] {
            assert!(
                !is_compatible(&[conflicting], &sharp),
                "id {conflicting} conflicts with Sharpness"
            );
        }
        // The test consults the candidate's set only, as vanilla does: Mending
        // has no exclusive set of its own, so it is compatible with an existing
        // Infinity even though Infinity conflicts with Mending.
        assert!(is_compatible(&[16], &by_name("mending").unwrap()));
        assert!(!is_compatible(&[23], &by_name("infinity").unwrap()));
    }

    #[test]
    fn the_full_name_appends_the_level_numeral() {
        let sharp = by_name("sharpness").unwrap();
        assert_eq!(full_name(&sharp, 1), "Sharpness I");
        assert_eq!(full_name(&sharp, 3), "Sharpness III");
        assert_eq!(full_name(&sharp, 5), "Sharpness V");
        assert_eq!(numeral(9), "IX");
        assert_eq!(numeral(10), "X");
        assert_eq!(numeral(11), "11");
        assert_eq!(numeral(0), "0");
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
    fn a_book_is_not_enchantable_by_the_command() {
        // Vanilla's enchanting table special-cases books; `/enchant` does
        // not, and a book is in no `enchantable/…` tag.
        let book = "minecraft:book";
        assert!(!can_enchant(book, &by_name("sharpness").unwrap()));
        assert!(!can_enchant(book, &by_name("silk_touch").unwrap()));
        assert!(!can_enchant(book, &by_name("wind_burst").unwrap()));
    }
}
