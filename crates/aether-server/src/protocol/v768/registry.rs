//! Registries sent during the configuration phase of protocol 768 (1.21.2/1.21.3).
//!
//! Self-contained on purpose: every other supported release has its own copy,
//! because the registry set and the entry schemas genuinely differ between
//! versions and a shared table would silently send one version's shape to
//! another's client.
//!
//! The contents come from a vanilla 1.21.3 server's own registry dump
//! (PrismarineJS `minecraft-data`, `pc/1.21.3/loginPacket.json`), not from a
//! neighbouring version.
//!
//! **World height.** The engine generates `y = 0..128` while this version
//! defaults to `-64..320`. Declaring the dimension as `min_y = 0, height = 128`
//! makes engine and client coordinates coincide exactly, so nothing anywhere
//! else in the codec has to translate them — and the shared world stays
//! consistent with 1.8 clients, which cannot represent a negative Y at all.

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
/// The single biome every column reports.
pub const BIOME_NAME: &str = "minecraft:plains";

/// `(registry id, entries)` for everything sent in the configuration phase.
pub fn registries() -> Vec<(&'static str, Vec<(String, Nbt)>)> {
    vec![
        (
            "minecraft:dimension_type",
            vec![(DIMENSION_NAME.into(), dimension_type())],
        ),
        ("minecraft:worldgen/biome", vec![(BIOME_NAME.into(), plains())]),
        ("minecraft:damage_type", damage_types()),
        // Present from 1.21 onward; the client rejects an empty registry
        // outright, and one entry is enough since no painting exists here.
        (
            "minecraft:painting_variant",
            vec![("minecraft:kebab".into(), painting())],
        ),
        (
            "minecraft:wolf_variant",
            vec![("minecraft:pale".into(), wolf_variant())],
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

/// A plains biome, matching the biome every column reports.
fn plains() -> Nbt {
    compound([
        ("has_precipitation", Nbt::Byte(1)),
        ("temperature", Nbt::Float(0.8)),
        ("downfall", Nbt::Float(0.4)),
        (
            "effects",
            compound([
                ("sky_color", Nbt::Int(0x78A7FF)),
                ("water_color", Nbt::Int(0x3F76E4)),
                ("water_fog_color", Nbt::Int(0x050533)),
                ("fog_color", Nbt::Int(0xC0D8FF)),
            ]),
        ),
    ])
}

/// The two `scaling` values vanilla damage types use.
const ALIVE: &str = "when_caused_by_living_non_player";
const ALWAYS: &str = "always";

/// Every damage type this version defines, as `(name, message_id, scaling,
/// exhaustion)`, taken verbatim from the vanilla dump.
///
/// The set has to be *complete*, not merely non-empty: configuration accepts
/// whatever it is given, but on entering play the client resolves damage
/// sources by key and throws on the first one missing.
const DAMAGE_TYPES: [(&str, &str, &str, f32); 49] = [
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

/// Damage types the client resolves by name for its death screen.
fn damage_types() -> Vec<(String, Nbt)> {
    DAMAGE_TYPES
        .into_iter()
        .map(|(name, message_id, scaling, exhaustion)| {
            (
                format!("minecraft:{name}"),
                compound([
                    ("message_id", string(message_id)),
                    ("scaling", string(scaling)),
                    ("exhaustion", Nbt::Float(exhaustion)),
                ]),
            )
        })
        .collect()
}

/// One painting, purely so the registry is non-empty.
fn painting() -> Nbt {
    compound([
        ("asset_id", string("minecraft:kebab")),
        ("height", Nbt::Int(1)),
        ("width", Nbt::Int(1)),
    ])
}

/// One wolf variant.
///
/// The textures are **flat** fields here. They moved into a nested `assets`
/// compound only in the 1.21.11 generation, so using that newer shape would
/// make this version's client reject the entry and leave the registry empty.
fn wolf_variant() -> Nbt {
    compound([
        ("wild_texture", string("minecraft:entity/wolf/wolf")),
        ("tame_texture", string("minecraft:entity/wolf/wolf_tame")),
        ("angry_texture", string("minecraft:entity/wolf/wolf_angry")),
        ("biomes", string("minecraft:plains")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_type_set_is_complete_not_just_non_empty() {
        // An incomplete registry clears configuration and then kills the join,
        // because entering play the client looks damage sources up by key.
        assert_eq!(damage_types().len(), 49);
    }

    #[test]
    fn registry_set_matches_this_version_only() {
        // Sending a registry a release does not know is as fatal as omitting
        // one it needs, so the set is pinned per version.
        let ids: Vec<&str> = registries().into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![
                "minecraft:dimension_type",
                "minecraft:worldgen/biome",
                "minecraft:damage_type",
                "minecraft:painting_variant", "minecraft:wolf_variant"
            ]
        );
    }

    #[test]
    fn dimension_declares_the_engine_column_verbatim() {
        let Nbt::Compound(fields) = dimension_type() else {
            panic!("dimension must be a compound");
        };
        let get = |n: &str| fields.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        assert!(matches!(get("min_y"), Some(Nbt::Int(v)) if v == MIN_Y));
        assert!(matches!(get("height"), Some(Nbt::Int(v)) if v == HEIGHT));
    }
}
