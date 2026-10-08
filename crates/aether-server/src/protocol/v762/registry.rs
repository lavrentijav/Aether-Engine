//! The registry codec sent inside the 1.19.4 Play Login packet.
//!
//! This generation has **no configuration phase** — that arrives in 1.20.2.
//! Everything 1.21 ships as separate Registry Data packets travels here
//! instead, as one NBT blob inside Login itself.
//!
//! Two shape details matter and are easy to get wrong:
//!
//! * the root compound is **named** (an empty name, but the two length bytes
//!   are present). The anonymous root only starts at 1.20.2, which is why
//!   [`named_root`] exists rather than [`Nbt::to_network`] being used directly;
//! * each registry is `{type, value: [{name, id, element}]}` — entries carry
//!   their own numeric ids rather than being positional.
//!
//!
//! The biome carries `has_precipitation` (a byte): 1.19.4 replaced the
//! older `precipitation` string.
//!
//! `minecraft:chat_type` is mandatory from 1.19 on: the client resolves every
//! incoming chat message against it and refuses to enter play without it, even
//! though this server only ever sends system chat.
//!
//! **World height.** The dimension is declared as `min_y = 0, height = 128` —
//! exactly the engine's own column — so engine and client coordinates coincide
//! and nothing needs shifting per packet.

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

/// Serialise `nbt` as a **named** root with an empty name.
///
/// [`Nbt::to_network`] writes the anonymous root used from 1.20.2 onward: tag
/// byte then payload. This version expects a name between the two, so splice
/// in its (zero) length.
pub fn named_root(nbt: &Nbt) -> Vec<u8> {
    let anon = nbt.to_network();
    let mut out = Vec::with_capacity(anon.len() + 2);
    out.push(anon[0]);
    out.extend_from_slice(&[0, 0]); // name length: 0
    out.extend_from_slice(&anon[1..]);
    out
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

/// The whole registry codec for the Login packet.
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

/// The overworld, cut down to the engine's actual 0..128 column.
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
        ("monster_spawn_light_level", Nbt::Int(0)),
        ("monster_spawn_block_light_limit", Nbt::Int(0)),
    ])
}

/// A plains biome, matching the flat biome the 1.8 codec reports.
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
        // Derived, not restated: asserting a literal here would only repeat
        // the constant. What matters is that the two agree.
        assert_eq!(SECTIONS, HEIGHT / 16);
    }

    #[test]
    fn root_is_named_unlike_the_1_21_form() {
        // The distinguishing byte pattern: tag, then a two-byte name length
        // that the anonymous 1.20.2+ form does not have.
        let nbt = compound([("a", Nbt::Byte(7))]);
        let named = named_root(&nbt);
        let anon = nbt.to_network();
        assert_eq!(named[0], 10, "root tag is TAG_Compound");
        assert_eq!(&named[1..3], &[0, 0], "empty root name length");
        assert_eq!(&named[3..], &anon[1..], "payload is otherwise identical");
        assert_eq!(named.len(), anon.len() + 2);
    }

    #[test]
    fn dimension_declares_the_engine_column_verbatim() {
        // No coordinate shifting anywhere depends on this holding.
        let Nbt::Compound(fields) = dimension_type() else {
            panic!("dimension must be a compound");
        };
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("min_y"), Some(Nbt::Int(MIN_Y)));
        assert_eq!(get("height"), Some(Nbt::Int(HEIGHT)));
    }

    #[test]
    fn codec_carries_the_three_mandatory_registries() {
        // Missing any of these and the client disconnects before play;
        // chat_type in particular is new in 1.19 and easy to forget.
        let Nbt::Compound(fields) = codec() else {
            panic!("codec must be a compound");
        };
        let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"minecraft:dimension_type"));
        assert!(names.contains(&"minecraft:worldgen/biome"));
        assert!(names.contains(&"minecraft:chat_type"));
    }

    #[test]
    fn biome_uses_this_releases_precipitation_field() {
        let Nbt::Compound(f) = plains() else {
            panic!("biome must be a compound")
        };
        let names: Vec<String> = f.into_iter().map(|(n, _)| n).collect();
        assert!(
            names.contains(&"has_precipitation".to_string()),
            "1.19.4+ uses a boolean here, not a precipitation string"
        );
        assert!(!names.contains(&"precipitation".to_string()));
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
