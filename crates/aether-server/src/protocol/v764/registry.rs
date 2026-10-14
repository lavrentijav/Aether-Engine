//! The registry codec sent in the configuration phase of protocol 764 (1.20.2).
//!
//! 1.20.2 moved the registries out of the Play Login packet and into the new
//! **configuration phase**, but it still sends them the 1.20.1 way: as one NBT
//! blob keyed by registry name, not the packet-per-registry form that arrives
//! with 1.20.5 (protocol 766).
//!
//! Two shape details matter and are easy to get wrong:
//!
//! * the root compound is **anonymous** — tag byte then payload, no name. This
//!   is where that form begins; 1.20.1 still writes a (zero-length) name, which
//!   is why its module carries a `named_root` helper and this one does not;
//! * each registry is `{type, value: [{name, id, element}]}` — entries carry
//!   their own numeric ids rather than being positional.
//!
//! The contents mirror the 1.20.1 (763) blob this release is built on, with
//! one 1.20.2 change: `monster_spawn_light_level` is an `IntProvider` (a
//! `minecraft:uniform` compound) rather than a bare int.
//!
//! **World height.** The dimension is declared as `min_y = -64, height = 448`
//! — exactly the engine's own column — so engine and client coordinates
//! coincide and nothing needs shifting per packet. Newer releases always
//! support a negative Y.

use super::super::nbt::{compound, string, Nbt};

/// Lowest world Y this server exposes.
pub const MIN_Y: i32 = -64;
/// Blocks of world height; must be a multiple of 16.
pub const HEIGHT: i32 = 448;
/// Sections in a column, derived so the two cannot drift apart.
pub const SECTIONS: i32 = HEIGHT / 16;

/// The single dimension this server serves.
pub const DIMENSION_NAME: &str = "minecraft:overworld";
/// The single biome every column reports.
pub const BIOME_NAME: &str = "minecraft:plains";

/// The whole registry codec, serialised in the anonymous network form.
pub fn blob() -> Vec<u8> {
    codec().to_network()
}

/// One registry: `{type, value: [{name, id, element}, ...]}`.
fn registry(type_id: &str, entries: Vec<(String, Nbt)>) -> Nbt {
    let value = entries
        .into_iter()
        .enumerate()
        .map(|(i, (name, element))| {
            Nbt::Compound(vec![
                ("name".to_string(), Nbt::Str(name)),
                ("id".to_string(), Nbt::Int(i as i32)),
                ("element".to_string(), element),
            ])
        })
        .collect();
    compound([("type", string(type_id)), ("value", Nbt::List(value))])
}

/// The whole registry codec for the configuration phase.
pub fn codec() -> Nbt {
    Nbt::Compound(vec![
        (
            "minecraft:dimension_type".to_string(),
            registry(
                "minecraft:dimension_type",
                vec![(DIMENSION_NAME.to_string(), dimension_type())],
            ),
        ),
        (
            "minecraft:worldgen/biome".to_string(),
            registry(
                "minecraft:worldgen/biome",
                vec![(BIOME_NAME.to_string(), plains())],
            ),
        ),
        (
            "minecraft:chat_type".to_string(),
            registry(
                "minecraft:chat_type",
                vec![("minecraft:chat".to_string(), chat_type())],
            ),
        ),
    ])
}

/// The overworld, cut down to the engine's actual column.
fn dimension_type() -> Nbt {
    compound([
        ("piglin_safe", Nbt::Byte(0)),
        ("natural", Nbt::Byte(1)),
        // Full-bright world: the engine pins light to maximum, so ambient
        // light is 1.0 rather than the vanilla 0.0.
        ("ambient_light", Nbt::Float(1.0)),
        ("infiniburn", string("#minecraft:infiniburn_overworld")),
        ("respawn_anchor_works", Nbt::Byte(0)),
        ("has_skylight", Nbt::Byte(1)),
        ("bed_works", Nbt::Byte(1)),
        ("effects", string("minecraft:overworld")),
        ("has_raids", Nbt::Byte(1)),
        ("logical_height", Nbt::Int(HEIGHT)),
        ("coordinate_scale", Nbt::Double(1.0)),
        ("min_y", Nbt::Int(MIN_Y)),
        ("height", Nbt::Int(HEIGHT)),
        ("ultrawarm", Nbt::Byte(0)),
        ("has_ceiling", Nbt::Byte(0)),
        // 1.20.2: an IntProvider, not the bare int 1.20.1 sent.
        (
            "monster_spawn_light_level",
            compound([
                ("type", string("minecraft:uniform")),
                (
                    "value",
                    compound([
                        ("min_inclusive", Nbt::Int(0)),
                        ("max_inclusive", Nbt::Int(7)),
                    ]),
                ),
            ]),
        ),
        ("monster_spawn_block_light_limit", Nbt::Int(0)),
    ])
}

/// A plains biome, matching the flat biome the older codecs report.
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

/// The one chat type. Only its presence matters here — this server sends
/// system chat, which is not routed through a chat type — but the client
/// validates the registry on join.
fn chat_type() -> Nbt {
    let decoration = |key: &str| {
        compound([
            ("translation_key", string(key)),
            (
                "parameters",
                Nbt::List(vec![string("sender"), string("content")]),
            ),
            ("style", Nbt::Compound(vec![])),
        ])
    };
    compound([
        ("chat", decoration("chat.type.text")),
        ("narration", decoration("chat.type.text.narrate")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_is_a_whole_number_of_sections() {
        assert_eq!(HEIGHT % 16, 0, "height must be a multiple of 16");
        assert_eq!(SECTIONS, HEIGHT / 16);
    }

    #[test]
    fn blob_root_is_anonymous() {
        // The distinguishing change from 1.20.1: tag byte (10) then straight
        // into the first field's tag, with no two-byte name length.
        let bytes = blob();
        assert_eq!(bytes[0], 10, "root tag is TAG_Compound");
        // First field: TAG_Compound (10), then the key length — never the
        // [0, 0] of an empty named root. The first key is dimension_type.
        assert_eq!(bytes[1], 10, "first field is itself a compound");
        assert_ne!(&bytes[2..4], &[0, 0], "no empty root name");
        assert_eq!(
            &bytes[2..4],
            &(b"minecraft:dimension_type".len() as u16).to_be_bytes(),
            "registry key length"
        );
    }

    #[test]
    fn codec_carries_the_three_registries() {
        let Nbt::Compound(fields) = codec() else {
            panic!("codec must be a compound");
        };
        let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"minecraft:dimension_type"));
        assert!(names.contains(&"minecraft:worldgen/biome"));
        assert!(names.contains(&"minecraft:chat_type"));
    }

    #[test]
    fn dimension_declares_the_engine_column_verbatim() {
        let Nbt::Compound(fields) = dimension_type() else {
            panic!("dimension must be a compound");
        };
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("min_y"), Some(Nbt::Int(MIN_Y)));
        assert_eq!(get("height"), Some(Nbt::Int(HEIGHT)));
    }

    #[test]
    fn monster_spawn_light_level_is_an_int_provider_in_this_release() {
        // 1.20.1 sent a bare int; 1.20.2 wraps it in a uniform provider, and a
        // client that expects the compound rejects the int form.
        let Nbt::Compound(fields) = dimension_type() else {
            panic!()
        };
        let level = fields
            .iter()
            .find(|(n, _)| n == "monster_spawn_light_level")
            .map(|(_, v)| v.clone());
        let Some(Nbt::Compound(inner)) = level else {
            panic!("must be a compound");
        };
        assert!(inner.iter().any(|(n, _)| n == "type"));
        assert!(inner.iter().any(|(n, _)| n == "value"));
    }

    #[test]
    fn registry_entries_are_numbered_from_zero() {
        let r = registry("minecraft:test", vec![("a".into(), Nbt::Byte(1))]);
        let Nbt::Compound(fields) = r else { panic!() };
        let Some((_, Nbt::List(value))) = fields.iter().find(|(n, _)| n == "value") else {
            panic!("registry needs a value list");
        };
        let Nbt::Compound(entry) = &value[0] else {
            panic!()
        };
        assert_eq!(entry[1], ("id".to_string(), Nbt::Int(0)));
    }
}
