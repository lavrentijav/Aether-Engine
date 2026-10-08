//! Registries sent during the 1.21 configuration phase.
//!
//! Since 1.20.5 the server, not the client, is the source of truth for these:
//! the client builds its dimension, biome and damage tables from what arrives
//! here, and refuses to enter play if a registry it considers mandatory is
//! missing. Everything below is the minimum this server actually uses, kept
//! deliberately small — one dimension, one biome, and the entry-less
//! registries the client still insists on receiving.
//!
//! **World height.** The engine generates `y = 0..128`, while 1.21 defaults to
//! `-64..320`. Rather than shifting coordinates on every packet — which would
//! desynchronise the shared world against 1.8 clients, who cannot represent
//! negative Y at all — the dimension is declared as `min_y = 0, height = 128`.
//! Engine and client coordinates then coincide exactly, in both directions,
//! and no translation is needed anywhere else in the codec.

use super::super::nbt::{compound, string, Nbt};

/// Lowest world Y this server exposes.
pub const MIN_Y: i32 = -64;
/// Number of blocks of world height; must be a multiple of 16.
///
/// The engine's range, `-64..=383`. This version's client can represent a
/// negative Y, so the engine's own coordinates go out unchanged; the versions
/// that cannot are shifted instead — see `Y_OFFSET` in their codecs.
///
/// Taller than vanilla's own 384. The dimension type carries `min_y` and
/// `height`, so the client believes what it is told; 28 sections still fit the
/// single 64-bit word every light bitset in this generation uses.
pub const HEIGHT: i32 = 448;
/// Sections in a column, derived so the two can never drift apart.
pub const SECTIONS: i32 = HEIGHT / 16;

/// The single dimension this server serves.
pub const DIMENSION_NAME: &str = "minecraft:overworld";

/// `(registry id, entries)` for everything sent in the configuration phase.
///
/// Each entry is `(key, value)`; a `None`-valued entry would mean "client
/// keeps its built-in copy", but sending explicit values keeps the server
/// self-describing.
pub fn registries() -> Vec<(&'static str, Vec<(String, Nbt)>)> {
    vec![
        (
            "minecraft:dimension_type",
            vec![(DIMENSION_NAME.into(), dimension_type())],
        ),
        ("minecraft:worldgen/biome", biomes()),
        ("minecraft:damage_type", damage_types()),
        // Registries the client validates the *presence* of even though this
        // server spawns none of the corresponding content. Each must carry at
        // least one entry: the client rejects an empty one outright ("Registry
        // must be non-empty") and stays stuck in configuration. One entry each
        // is enough — none of these mobs exist here.
        (
            "minecraft:painting_variant",
            vec![("minecraft:kebab".into(), painting())],
        ),
        (
            "minecraft:wolf_variant",
            vec![("minecraft:pale".into(), wolf_variant())],
        ),
        (
            "minecraft:wolf_sound_variant",
            vec![("minecraft:classic".into(), wolf_sound_variant())],
        ),
        (
            "minecraft:cat_variant",
            vec![(
                "minecraft:tabby".into(),
                asset_variant("minecraft:entity/cat/tabby"),
            )],
        ),
        (
            "minecraft:frog_variant",
            vec![(
                "minecraft:temperate".into(),
                asset_variant("minecraft:entity/frog/temperate_frog"),
            )],
        ),
        (
            "minecraft:zombie_nautilus_variant",
            vec![(
                "minecraft:temperate".into(),
                asset_variant("minecraft:entity/nautilus/zombie_nautilus"),
            )],
        ),
        (
            "minecraft:chicken_variant",
            vec![(
                "minecraft:temperate".into(),
                modeled_variant("normal", "minecraft:entity/chicken/temperate_chicken"),
            )],
        ),
        (
            "minecraft:cow_variant",
            vec![(
                "minecraft:temperate".into(),
                modeled_variant("normal", "minecraft:entity/cow/temperate_cow"),
            )],
        ),
        (
            "minecraft:pig_variant",
            vec![(
                "minecraft:temperate".into(),
                modeled_variant("normal", "minecraft:entity/pig/temperate_pig"),
            )],
        ),
    ]
}

/// The overworld, cut down to the engine's actual 0..128 column.
fn dimension_type() -> Nbt {
    compound([
        ("has_skylight", Nbt::Byte(1)),
        ("has_ceiling", Nbt::Byte(0)),
        ("ultrawarm", Nbt::Byte(0)),
        ("natural", Nbt::Byte(1)),
        ("coordinate_scale", Nbt::Double(1.0)),
        ("bed_works", Nbt::Byte(1)),
        ("respawn_anchor_works", Nbt::Byte(0)),
        ("min_y", Nbt::Int(MIN_Y)),
        ("height", Nbt::Int(HEIGHT)),
        ("logical_height", Nbt::Int(HEIGHT)),
        ("infiniburn", string("#minecraft:infiniburn_overworld")),
        ("effects", string("minecraft:overworld")),
        // Full-bright world: the engine pins light to maximum, so ambient
        // light is 1.0 rather than the vanilla 0.0.
        ("ambient_light", Nbt::Float(1.0)),
        ("piglin_safe", Nbt::Byte(0)),
        ("has_raids", Nbt::Byte(1)),
        ("monster_spawn_light_level", Nbt::Int(0)),
        ("monster_spawn_block_light_limit", Nbt::Int(0)),
    ])
}

/// Every biome 1.21.11 defines, as `(name, temperature, has_precipitation)`,
/// from `minecraft-data` pc/1.21.11. Sorted by name; the registry index of
/// each is its position here, which is what chunk data refers to.
const BIOMES: [(&str, f32, bool); 65] = [
    ("badlands", 2.0, false),
    ("bamboo_jungle", 0.95, true),
    ("basalt_deltas", 2.0, false),
    ("beach", 0.8, true),
    ("birch_forest", 0.6, true),
    ("cherry_grove", 0.5, true),
    ("cold_ocean", 0.5, true),
    ("crimson_forest", 2.0, false),
    ("dark_forest", 0.7, true),
    ("deep_cold_ocean", 0.5, true),
    ("deep_dark", 0.8, true),
    ("deep_frozen_ocean", 0.5, true),
    ("deep_lukewarm_ocean", 0.5, true),
    ("deep_ocean", 0.5, true),
    ("desert", 2.0, false),
    ("dripstone_caves", 0.8, true),
    ("end_barrens", 0.5, false),
    ("end_highlands", 0.5, false),
    ("end_midlands", 0.5, false),
    ("eroded_badlands", 2.0, false),
    ("flower_forest", 0.7, true),
    ("forest", 0.7, true),
    ("frozen_ocean", 0.0, true),
    ("frozen_peaks", -0.7, true),
    ("frozen_river", 0.0, true),
    ("grove", -0.2, true),
    ("ice_spikes", 0.0, true),
    ("jagged_peaks", -0.7, true),
    ("jungle", 0.95, true),
    ("lukewarm_ocean", 0.5, true),
    ("lush_caves", 0.5, true),
    ("mangrove_swamp", 0.8, true),
    ("meadow", 0.5, true),
    ("mushroom_fields", 0.9, true),
    ("nether_wastes", 2.0, false),
    ("ocean", 0.5, true),
    ("old_growth_birch_forest", 0.6, true),
    ("old_growth_pine_taiga", 0.3, true),
    ("old_growth_spruce_taiga", 0.25, true),
    ("pale_garden", 0.7, true),
    ("plains", 0.8, true),
    ("river", 0.5, true),
    ("savanna", 2.0, false),
    ("savanna_plateau", 2.0, false),
    ("small_end_islands", 0.5, false),
    ("snowy_beach", 0.05, true),
    ("snowy_plains", 0.0, true),
    ("snowy_slopes", -0.3, true),
    ("snowy_taiga", -0.5, true),
    ("soul_sand_valley", 2.0, false),
    ("sparse_jungle", 0.95, true),
    ("stony_peaks", 1.0, true),
    ("stony_shore", 0.2, true),
    ("sunflower_plains", 0.8, true),
    ("swamp", 0.8, true),
    ("taiga", 0.25, true),
    ("the_end", 0.5, false),
    ("the_void", 0.5, false),
    ("warm_ocean", 0.5, true),
    ("warped_forest", 2.0, false),
    ("windswept_forest", 0.2, true),
    ("windswept_gravelly_hills", 0.2, true),
    ("windswept_hills", 0.2, true),
    ("windswept_savanna", 2.0, false),
    ("wooded_badlands", 2.0, false),
];

/// The registry index of a biome, `minecraft:`-qualified or not; plains
/// for anything unknown.
pub fn biome_index(name: &str) -> u32 {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    BIOMES
        .binary_search_by(|(n, ..)| (*n).cmp(short))
        .or_else(|_| BIOMES.binary_search_by(|(n, ..)| (*n).cmp("plains")))
        .unwrap_or(0) as u32
}

/// Vanilla's sky colour for a temperature (`Biome.calculateSkyColor`).
fn sky_color(temperature: f32) -> i32 {
    let t = (temperature / 3.0).clamp(-1.0, 1.0);
    hsb_to_rgb(0.622_222_2 - t * 0.05, 0.5 + t * 0.1, 1.0)
}

/// `java.awt.Color.HSBtoRGB`.
fn hsb_to_rgb(hue: f32, sat: f32, bri: f32) -> i32 {
    let h = (hue - hue.floor()) * 6.0;
    let f = h - h.floor();
    let p = bri * (1.0 - sat);
    let q = bri * (1.0 - sat * f);
    let t = bri * (1.0 - sat * (1.0 - f));
    let (r, g, b) = match h as i32 {
        0 => (bri, t, p),
        1 => (q, bri, p),
        2 => (p, bri, t),
        3 => (p, q, bri),
        4 => (t, p, bri),
        _ => (bri, p, q),
    };
    let c = |v: f32| (v * 255.0 + 0.5) as i32;
    (c(r) << 16) | (c(g) << 8) | c(b)
}

/// A biome's rainfall, which with temperature picks grass and foliage
/// colour. Not in `minecraft-data`; vanilla's values by family.
fn downfall(name: &str) -> f32 {
    match name {
        n if n.contains("jungle") || n == "swamp" || n == "mangrove_swamp" => 0.9,
        "mushroom_fields" => 1.0,
        n if n.contains("desert") || n.contains("savanna") || n.contains("badlands") => 0.0,
        n if n.contains("nether")
            || n.contains("basalt")
            || n.contains("crimson")
            || n.contains("warped")
            || n.contains("soul") =>
        {
            0.0
        }
        n if n.contains("ocean") || n == "river" || n == "beach" => 0.5,
        "forest"
        | "flower_forest"
        | "dark_forest"
        | "taiga"
        | "old_growth_pine_taiga"
        | "old_growth_spruce_taiga"
        | "cherry_grove"
        | "pale_garden" => 0.8,
        "birch_forest" | "old_growth_birch_forest" => 0.6,
        "meadow" => 0.8,
        n if n.contains("snowy") || n.contains("frozen") || n == "ice_spikes" || n == "grove" => {
            0.5
        }
        _ => 0.4,
    }
}

fn water_color(name: &str) -> i32 {
    match name {
        "swamp" => 0x617B64,
        "mangrove_swamp" => 0x3A7A6A,
        "warm_ocean" => 0x43D5EE,
        "lukewarm_ocean" | "deep_lukewarm_ocean" => 0x45ADF2,
        "cold_ocean" | "deep_cold_ocean" | "snowy_taiga" | "snowy_beach" => 0x3D57D6,
        "frozen_ocean" | "deep_frozen_ocean" | "frozen_river" => 0x3938C9,
        "cherry_grove" => 0x5DB7EF,
        _ => 0x3F76E4,
    }
}

/// The underwater fog colour — murky green in swamps, as in vanilla.
fn water_fog_color(name: &str) -> i32 {
    match name {
        "swamp" => 0x232317,
        "mangrove_swamp" => 0x4D7A60,
        "warm_ocean" => 0x041F33,
        "lukewarm_ocean" | "deep_lukewarm_ocean" => 0x041633,
        "cherry_grove" => 0x5DB7EF,
        _ => 0x050533,
    }
}

/// The whole biome registry, in [`BIOMES`] order.
fn biomes() -> Vec<(String, Nbt)> {
    BIOMES
        .iter()
        .map(|(name, temperature, rain)| {
            (
                format!("minecraft:{name}"),
                compound([
                    ("has_precipitation", Nbt::Byte(*rain as i8)),
                    ("temperature", Nbt::Float(*temperature)),
                    ("downfall", Nbt::Float(downfall(name))),
                    (
                        "effects",
                        compound([
                            ("sky_color", Nbt::Int(sky_color(*temperature))),
                            ("water_color", Nbt::Int(water_color(name))),
                            ("water_fog_color", Nbt::Int(water_fog_color(name))),
                            ("fog_color", Nbt::Int(0xC0D8FF)),
                        ]),
                    ),
                ]),
            )
        })
        .collect()
}

/// The two `scaling` values vanilla damage types use.
const ALIVE: &str = "when_caused_by_living_non_player";
const ALWAYS: &str = "always";

/// Every damage type 1.21.11 defines, as `(name, message_id, scaling,
/// exhaustion)`, taken verbatim from a vanilla server's own registry dump.
///
/// The set has to be *complete*, not merely non-empty. Configuration accepts
/// whatever it is given, but on entering play the client resolves damage
/// sources by key and throws on the first one missing — an abbreviated list of
/// 23 cleared configuration and then died on `minecraft:campfire`.
const DAMAGE_TYPES: [(&str, &str, &str, f32); 50] = [
    ("arrow", "arrow", ALIVE, 0.1),
    ("bad_respawn_point", "badRespawnPoint", ALWAYS, 0.1),
    ("cactus", "cactus", ALIVE, 0.1),
    ("campfire", "inFire", ALIVE, 0.1),
    ("cramming", "cramming", ALIVE, 0.0),
    ("dragon_breath", "dragonBreath", ALIVE, 0.0),
    ("drown", "drown", ALIVE, 0.0),
    ("dry_out", "dryout", ALIVE, 0.1),
    ("ender_pearl", "fall", ALIVE, 0.0),
    ("explosion", "explosion", ALWAYS, 0.1),
    ("fall", "fall", ALIVE, 0.0),
    ("falling_anvil", "anvil", ALIVE, 0.1),
    ("falling_block", "fallingBlock", ALIVE, 0.1),
    ("falling_stalactite", "fallingStalactite", ALIVE, 0.1),
    ("fireball", "fireball", ALIVE, 0.1),
    ("fireworks", "fireworks", ALIVE, 0.1),
    ("fly_into_wall", "flyIntoWall", ALIVE, 0.0),
    ("freeze", "freeze", ALIVE, 0.0),
    ("generic", "generic", ALIVE, 0.0),
    ("generic_kill", "genericKill", ALIVE, 0.0),
    ("hot_floor", "hotFloor", ALIVE, 0.1),
    ("in_fire", "inFire", ALIVE, 0.1),
    ("in_wall", "inWall", ALIVE, 0.0),
    ("indirect_magic", "indirectMagic", ALIVE, 0.0),
    ("lava", "lava", ALIVE, 0.1),
    ("lightning_bolt", "lightningBolt", ALIVE, 0.1),
    ("mace_smash", "mace_smash", ALIVE, 0.1),
    ("magic", "magic", ALIVE, 0.0),
    ("mob_attack", "mob", ALIVE, 0.1),
    ("mob_attack_no_aggro", "mob", ALIVE, 0.1),
    ("mob_projectile", "mob", ALIVE, 0.1),
    ("on_fire", "onFire", ALIVE, 0.0),
    ("out_of_world", "outOfWorld", ALIVE, 0.0),
    ("outside_border", "outsideBorder", ALIVE, 0.0),
    ("player_attack", "player", ALIVE, 0.1),
    ("player_explosion", "explosion.player", ALWAYS, 0.1),
    ("sonic_boom", "sonic_boom", ALWAYS, 0.0),
    ("spear", "spear", ALIVE, 0.1),
    ("spit", "mob", ALIVE, 0.1),
    ("stalagmite", "stalagmite", ALIVE, 0.0),
    ("starve", "starve", ALIVE, 0.0),
    ("sting", "sting", ALIVE, 0.1),
    ("sweet_berry_bush", "sweetBerryBush", ALIVE, 0.1),
    ("thorns", "thorns", ALIVE, 0.1),
    ("thrown", "thrown", ALIVE, 0.1),
    ("trident", "trident", ALIVE, 0.1),
    ("unattributed_fireball", "onFire", ALIVE, 0.1),
    ("wind_charge", "mob", ALIVE, 0.1),
    ("wither", "wither", ALIVE, 0.0),
    ("wither_skull", "witherSkull", ALIVE, 0.1),
];

/// The registry index of a damage type, as the client numbers it: the order
/// the entries were sent in.
pub fn damage_type_index(name: &str) -> Option<usize> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    DAMAGE_TYPES.iter().position(|(n, ..)| *n == short)
}

/// Damage types the client resolves by name for its death screen. Nothing here
/// can actually kill a player on this server, but the registry must exist and
/// be complete.
fn damage_types() -> Vec<(String, Nbt)> {
    DAMAGE_TYPES
        .into_iter()
        .map(|(name, message_id, scaling, exhaustion)| {
            (
                format!("minecraft:{name}"),
                compound([
                    ("message_id", string(message_id)),
                    ("scaling", string(scaling)),
                    // `exhaustion`, not `exposure` — the wrong name made the
                    // client reject every entry in this registry and refuse to
                    // leave the configuration phase.
                    ("exhaustion", Nbt::Float(exhaustion)),
                ]),
            )
        })
        .collect()
}

fn painting() -> Nbt {
    compound([
        ("asset_id", string("minecraft:kebab")),
        ("height", Nbt::Int(1)),
        ("width", Nbt::Int(1)),
    ])
}

/// Wolf textures live in a nested `assets` compound; they were flat
/// `*_texture` fields in earlier 1.21 releases, and sending the old shape made
/// the client reject the entry.
fn wolf_variant() -> Nbt {
    compound([(
        "assets",
        compound([
            ("wild", string("minecraft:entity/wolf/wolf")),
            ("tame", string("minecraft:entity/wolf/wolf_tame")),
            ("angry", string("minecraft:entity/wolf/wolf_angry")),
        ]),
    )])
}

/// The six sounds a wolf variant is required to name.
fn wolf_sound_variant() -> Nbt {
    compound([
        ("ambient_sound", string("minecraft:entity.wolf.ambient")),
        ("hurt_sound", string("minecraft:entity.wolf.hurt")),
        ("death_sound", string("minecraft:entity.wolf.death")),
        ("growl_sound", string("minecraft:entity.wolf.growl")),
        ("whine_sound", string("minecraft:entity.wolf.whine")),
        ("pant_sound", string("minecraft:entity.wolf.pant")),
    ])
}

/// An entity variant identified by a texture alone (cat, frog, ...).
fn asset_variant(asset: &str) -> Nbt {
    Nbt::Compound(vec![("asset_id".to_string(), string(asset))])
}

/// An entity variant that also names a body model (chicken, cow, pig).
fn modeled_variant(model: &str, asset: &str) -> Nbt {
    compound([("model", string(model)), ("asset_id", string(asset))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_is_a_whole_number_of_sections() {
        assert_eq!(HEIGHT % 16, 0, "height must be a multiple of 16");
        // Derived, not restated: asserting a literal here would only repeat
        // the constant. What matters is that the two agree.
        assert_eq!(SECTIONS, HEIGHT / 16);
    }

    #[test]
    fn dimension_declares_the_engine_column_verbatim() {
        // The whole point of the height decision: no coordinate shifting, so
        // min_y/height must be exactly the engine's own range.
        let Nbt::Compound(fields) = dimension_type() else {
            panic!("dimension must be a compound");
        };
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("min_y"), Some(Nbt::Int(MIN_Y)));
        assert_eq!(get("height"), Some(Nbt::Int(HEIGHT)));
    }

    #[test]
    fn every_registry_has_at_least_one_entry() {
        // An empty registry makes the client fall back to built-ins or fail
        // outright, depending on the registry — never ship one.
        for (id, entries) in registries() {
            assert!(!entries.is_empty(), "{id} has no entries");
        }
    }

    #[test]
    fn mandatory_registries_are_all_present() {
        // A 1.21.11 client refuses to leave the configuration phase unless it
        // receives every one of these non-empty ("Registry must be
        // non-empty"). Dropping one is a crash on join, not a degraded
        // experience, so pin the list here rather than rediscovering it from a
        // client crash report.
        const MANDATORY: [&str; 12] = [
            "minecraft:dimension_type",
            "minecraft:worldgen/biome",
            "minecraft:damage_type",
            "minecraft:painting_variant",
            "minecraft:wolf_variant",
            "minecraft:wolf_sound_variant",
            "minecraft:cat_variant",
            "minecraft:chicken_variant",
            "minecraft:cow_variant",
            "minecraft:frog_variant",
            "minecraft:pig_variant",
            "minecraft:zombie_nautilus_variant",
        ];
        let sent = registries();
        for id in MANDATORY {
            let Some((_, entries)) = sent.iter().find(|(sent_id, _)| *sent_id == id) else {
                panic!("mandatory registry {id} is not sent at all");
            };
            assert!(!entries.is_empty(), "mandatory registry {id} is empty");
        }
    }

    #[test]
    fn damage_types_use_the_exhaustion_field() {
        // The client parses damage types strictly: the field is `exhaustion`,
        // and naming it `exposure` silently failed every single entry.
        let (_, value) = damage_types().into_iter().next().expect("a damage type");
        let Nbt::Compound(fields) = value else {
            panic!("damage type must be a compound");
        };
        let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"exhaustion"), "got fields {names:?}");
        assert!(!names.contains(&"exposure"));
    }

    #[test]
    fn damage_type_set_is_complete_not_just_non_empty() {
        // Configuration accepts any set, so an incomplete registry passes it
        // and then kills the join: entering play the client resolves damage
        // sources by key and throws on the first one missing. A 23-entry list
        // cleared configuration and died on `campfire`, so pin the full set
        // and a few keys an abbreviated hand-written list tends to drop.
        let types = damage_types();
        assert_eq!(types.len(), 50, "vanilla 1.21.11 defines 50 damage types");
        let keys: Vec<&str> = types.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "minecraft:campfire",
            "minecraft:player_attack",
            "minecraft:mob_attack",
            "minecraft:sonic_boom",
            "minecraft:ender_pearl",
        ] {
            assert!(keys.contains(&required), "{required} missing");
        }
    }

    #[test]
    fn wolf_textures_are_nested_under_assets() {
        // Flat `wild_texture`/`tame_texture`/`angry_texture` is the pre-1.21.5
        // shape and is rejected outright.
        let Nbt::Compound(fields) = wolf_variant() else {
            panic!("wolf variant must be a compound");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "assets");
        let Nbt::Compound(assets) = &fields[0].1 else {
            panic!("assets must be a compound");
        };
        let names: Vec<&str> = assets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["wild", "tame", "angry"]);
    }
}
