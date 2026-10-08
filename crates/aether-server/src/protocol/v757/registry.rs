//! The dimension codec sent inside the 1.18 Login packet.
//!
//! There is no configuration phase in this generation (that arrives in
//! 1.20.2). Instead the whole registry set travels as NBT *inside* the play
//! Login packet: `dimensionCodec` carries the registries, and `dimension`
//! repeats the element for the world being joined.
//!
//! **Field sets are version-specific.** The 1.18.2 dimension type has no
//! `monster_spawn_light_level` / `monster_spawn_block_light_limit` (those are
//! 1.19 additions) and the biome still requires `category` and a string
//! `precipitation` (both dropped later). The lists below are taken from
//! PrismarineJS `minecraft-data` `pc/1.18.2/loginPacket.json`, which is a
//! capture of what a real server sends, rather than adapted from the 1.21
//! codec next door.
//!
//! **World height.** Like the 1.21 codec, the dimension is declared as
//! `min_y = 0, height = 128` — exactly the engine's own column — so engine and
//! client coordinates coincide and nothing has to be shifted per packet.

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

/// Serialise `value` as a **named** root compound.
///
/// 1.18 predates the anonymous root form: the tag byte is followed by a
/// (here empty) name before the payload. [`Nbt::to_network`] writes the modern
/// nameless form the 1.21 codec needs, so this splices the two length bytes
/// back in rather than duplicating the writer.
pub fn named_root(value: &Nbt) -> Vec<u8> {
    let anon = value.to_network();
    let mut out = Vec::with_capacity(anon.len() + 2);
    out.push(anon[0]); // tag
    out.extend_from_slice(&[0, 0]); // empty root name
    out.extend_from_slice(&anon[1..]);
    out
}

/// One `{name, id, element}` registry entry.
fn entry(name: &str, id: i32, element: Nbt) -> Nbt {
    Nbt::Compound(vec![
        ("name".into(), string(name)),
        ("id".into(), Nbt::Int(id)),
        ("element".into(), element),
    ])
}

/// A `{type, value}` registry wrapper.
fn registry(id: &str, entries: Vec<Nbt>) -> Nbt {
    Nbt::Compound(vec![
        ("type".into(), string(id)),
        ("value".into(), Nbt::List(entries)),
    ])
}

/// The full `dimensionCodec` compound for the Login packet.
pub fn dimension_codec() -> Nbt {
    Nbt::Compound(vec![
        (
            "minecraft:dimension_type".into(),
            registry(
                "minecraft:dimension_type",
                vec![entry(DIMENSION_NAME, 0, dimension_type())],
            ),
        ),
        (
            "minecraft:worldgen/biome".into(),
            registry(
                "minecraft:worldgen/biome",
                vec![entry(BIOME_NAME, 0, plains())],
            ),
        ),
    ])
}

/// The overworld, cut down to the engine's actual 0..128 column.
pub fn dimension_type() -> Nbt {
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
        ("has_ceiling", Nbt::Byte(0)),
        ("ultrawarm", Nbt::Byte(0)),
        ("height", Nbt::Int(HEIGHT)),
    ])
}

/// A plains biome, matching the flat biome the 1.8 codec reports.
///
/// `category` and the string `precipitation` are mandatory in this generation
/// and were only dropped in later ones.
fn plains() -> Nbt {
    compound([
        ("precipitation", string("rain")),
        ("temperature", Nbt::Float(0.8)),
        ("downfall", Nbt::Float(0.4)),
        ("category", string("plains")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_is_a_whole_number_of_sections() {
        assert_eq!(HEIGHT % 16, 0, "height must be a multiple of 16");
        assert_eq!(MIN_Y % 16, 0, "min_y must be a multiple of 16");
        // Derived, not restated: asserting a literal here would only repeat
        // the constant. What matters is that the two agree.
        assert_eq!(SECTIONS, HEIGHT / 16);
    }

    #[test]
    fn named_root_carries_an_empty_name_the_anonymous_form_lacks() {
        // The 1.20.2+ form used by the 1.21 codec is tag-then-payload; this
        // generation needs the two name-length bytes back in between. Getting
        // it wrong shifts every following byte and the client rejects the
        // login packet outright.
        let value = compound([("a", Nbt::Byte(7))]);
        let anon = value.to_network();
        let named = named_root(&value);
        assert_eq!(named.len(), anon.len() + 2);
        assert_eq!(named[0], anon[0], "same root tag");
        assert_eq!(&named[1..3], &[0, 0], "empty root name length");
        assert_eq!(&named[3..], &anon[1..], "payload unchanged");
    }

    #[test]
    fn dimension_declares_the_engine_column_verbatim() {
        // The point of the height decision for this generation: its client can
        // hold a negative Y, so the engine's range goes out verbatim and
        // nothing is shifted. The versions that cannot are shifted instead —
        // see `Y_OFFSET` in their codecs.
        let Nbt::Compound(fields) = dimension_type() else {
            panic!("dimension must be a compound");
        };
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("min_y"), Some(Nbt::Int(MIN_Y)));
        assert_eq!(get("height"), Some(Nbt::Int(HEIGHT)));
        assert_eq!(get("logical_height"), Some(Nbt::Int(HEIGHT)));
    }

    #[test]
    fn dimension_type_omits_fields_this_generation_does_not_have() {
        // 1.19 added these two; sending them here would be inventing fields
        // for a version that has no slot for them.
        let Nbt::Compound(fields) = dimension_type() else {
            panic!("compound");
        };
        for absent in ["monster_spawn_light_level", "monster_spawn_block_light_limit"] {
            assert!(
                !fields.iter().any(|(n, _)| n == absent),
                "{absent} does not exist in 1.18"
            );
        }
    }

    #[test]
    fn codec_holds_both_registries_with_id_zero_entries() {
        // Chunk biome containers reference registry ids, and this server only
        // ever writes id 0 — so the entry it names had better be id 0.
        let Nbt::Compound(regs) = dimension_codec() else {
            panic!("compound");
        };
        let names: Vec<&str> = regs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["minecraft:dimension_type", "minecraft:worldgen/biome"]
        );
        for (_, reg) in &regs {
            let Nbt::Compound(fields) = reg else {
                panic!("registry compound");
            };
            let Some((_, Nbt::List(entries))) = fields.iter().find(|(n, _)| n == "value") else {
                panic!("registry needs a value list");
            };
            let Nbt::Compound(first) = &entries[0] else {
                panic!("entry compound");
            };
            assert_eq!(
                first.iter().find(|(n, _)| n == "id").map(|(_, v)| v.clone()),
                Some(Nbt::Int(0))
            );
        }
    }
}
