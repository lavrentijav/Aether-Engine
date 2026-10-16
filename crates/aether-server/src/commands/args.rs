//! The non-entity argument parsers.
//!
//! Brigadier gives each argument type a class that both parses a slice of the
//! line and renders it for a suggestion. Here the types are the [`ArgKind`]
//! variants and this module is their one parser; the tree calls
//! [`parse`] once it has decided which node matched.
//!
//! Coordinates live in [`super::coordinates`] and SNBT in [`super::snbt`]; the
//! rest are small, exact validators: a resource location, a gamemode word, a
//! time with its suffix, a block state resolved through the world's block
//! table, an item stack with its optional component compound and count.

use aether_world::registry::blocks;
use aether_world::registry::props;
use aether_world::{BlockStateId, ItemTypeId};

use super::coordinates::{CoordError, Coordinates, Coordinates2, Swizzle, WorldCoordinate};
use super::snbt;
use super::tree::{ArgKind, ParseError, StringReader, Value};
use crate::protocol::nbt::Nbt;

/// An item stack argument: an item, its optional component compound and a count.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemStack {
    /// The item type.
    pub item: ItemTypeId,
    /// How many; at least one.
    pub count: i32,
    /// The `{…}` component compound, if the argument carried one.
    pub nbt: Option<Nbt>,
}

/// Parse one argument of `kind` from `r`, returning its value and source text.
pub(crate) fn parse(kind: ArgKind, r: &mut StringReader) -> Result<(Value, String), ParseError> {
    match kind {
        // String shapes.
        ArgKind::GreedyString | ArgKind::Message => {
            let t = r.read_remainder()?;
            Ok((Value::Text(t.clone()), t))
        }
        ArgKind::String | ArgKind::Quotable => {
            let t = r.read_string_arg()?;
            Ok((Value::Text(t.clone()), t))
        }
        // The primitive numbers were built in CMD-03.
        ArgKind::Integer => {
            let t = read_required(r)?;
            t.parse::<i32>()
                .map(|n| (Value::Int(n), t.clone()))
                .map_err(|_| ParseError::InvalidInteger(t))
        }
        ArgKind::Long => {
            let t = read_required(r)?;
            t.parse::<i64>()
                .map(|n| (Value::Long(n), t.clone()))
                .map_err(|_| ParseError::InvalidLong(t))
        }
        ArgKind::Float => {
            let t = read_required(r)?;
            t.parse::<f32>()
                .map(|n| (Value::Float(n), t.clone()))
                .map_err(|_| ParseError::InvalidFloat(t))
        }
        ArgKind::Double => {
            let t = read_required(r)?;
            t.parse::<f64>()
                .map(|n| (Value::Double(n), t.clone()))
                .map_err(|_| ParseError::InvalidDouble(t))
        }
        ArgKind::Bool => {
            let t = read_required(r)?;
            match t.as_str() {
                "true" => Ok((Value::Bool(true), t)),
                "false" => Ok((Value::Bool(false), t)),
                _ => Err(ParseError::InvalidBool(t)),
            }
        }
        ArgKind::Enum(words) => {
            let t = read_required(r)?;
            if words.contains(&t.as_str()) {
                Ok((Value::Text(t.clone()), t))
            } else {
                Err(ParseError::IncorrectArg)
            }
        }
        // Coordinates.
        ArgKind::BlockPos => {
            let start = r.cursor;
            let c = coordinates(r, true)?;
            Ok((Value::Coordinates(c), r.text_from(start)))
        }
        ArgKind::Vec3 => {
            let start = r.cursor;
            let c = coordinates(r, false)?;
            Ok((Value::Coordinates(c), r.text_from(start)))
        }
        ArgKind::Vec2 => {
            let start = r.cursor;
            let c = coordinates2(r)?;
            Ok((Value::Vec2(c), r.text_from(start)))
        }
        ArgKind::Rotation => {
            let start = r.cursor;
            let c = coordinates2(r)?;
            Ok((Value::Rotation(c), r.text_from(start)))
        }
        ArgKind::Swizzle => {
            let t = read_required(r)?;
            match Swizzle::parse(&t) {
                Some(s) => Ok((Value::Swizzle(s), t)),
                None => Err(ParseError::IncorrectArg),
            }
        }
        ArgKind::Angle => {
            let t = read_required(r)?;
            let c = WorldCoordinate::parse(&t).map_err(coord_error)?;
            Ok((Value::Angle(c), t))
        }
        // The block, item, NBT and registry-backed kinds.
        ArgKind::BlockState => {
            let t = read_component(r)?;
            let id = parse_block_state(&t)?;
            Ok((Value::BlockState(id), t))
        }
        ArgKind::ItemStack => parse_item_stack(r),
        ArgKind::NbtCompound => {
            let t = read_component(r)?;
            let nbt = snbt::parse_compound(&t)
                .map_err(|e| ParseError::Argument(format!("Invalid NBT: {e}")))?;
            Ok((Value::Nbt(nbt), t))
        }
        ArgKind::Component => {
            let t = read_component(r)?;
            let nbt = snbt::parse(&t)
                .map_err(|e| ParseError::Argument(format!("Invalid component: {e}")))?;
            Ok((Value::Nbt(nbt), t))
        }
        ArgKind::ResourceLocation => {
            let t = read_required(r)?;
            let norm = normalize_resource(&t);
            if !valid_resource(&norm) {
                return Err(ParseError::Argument(format!(
                    "Invalid resource location '{t}'"
                )));
            }
            Ok((Value::ResourceLocation(norm), t))
        }
        ArgKind::ResourceOrTag => {
            let t = read_required(r)?;
            let (name, tag) = match t.strip_prefix('#') {
                Some(rest) => (rest, true),
                None => (t.as_str(), false),
            };
            let norm = normalize_resource(name);
            if !valid_resource(&norm) {
                return Err(ParseError::Argument(format!(
                    "Invalid resource location '{t}'"
                )));
            }
            let rendered = if tag { format!("#{norm}") } else { norm };
            Ok((Value::ResourceLocation(rendered), t))
        }
        ArgKind::GameProfile => {
            let t = read_required(r)?;
            Ok((Value::Text(t.clone()), t))
        }
        ArgKind::Time => parse_time(r),
        ArgKind::Gamemode => {
            let t = read_required(r)?;
            let canonical = match t.to_ascii_lowercase().as_str() {
                "survival" | "s" | "0" => "survival",
                "creative" | "c" | "1" => "creative",
                "adventure" | "a" | "2" => "adventure",
                "spectator" | "sp" | "3" => "spectator",
                _ => return Err(ParseError::Argument(format!("Unknown game mode '{t}'"))),
            };
            Ok((Value::Text(canonical.into()), t))
        }
        ArgKind::Difficulty => {
            let t = read_required(r)?;
            let canonical = match t.to_ascii_lowercase().as_str() {
                "peaceful" | "p" => "peaceful",
                "easy" | "e" => "easy",
                "normal" | "n" => "normal",
                "hard" | "h" => "hard",
                _ => return Err(ParseError::Argument(format!("Unknown difficulty '{t}'"))),
            };
            Ok((Value::Text(canonical.into()), t))
        }
        ArgKind::Particle => {
            // A particle id, then whatever the type-specific parameters are;
            // kept whole so the executor can hand the tail to its own parser.
            let t = read_component(r)?;
            Ok((Value::Text(t.clone()), t))
        }
        ArgKind::ScoreHolder => {
            let t = read_required(r)?;
            Ok((Value::Text(t.clone()), t))
        }
        ArgKind::Operation => {
            let t = read_required(r)?;
            match t.as_str() {
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<" | ">" | "><" => {
                    Ok((Value::Text(t.clone()), t))
                }
                _ => Err(ParseError::IncorrectArg),
            }
        }
        // `uuid` and the entity kinds are CMD-06b's; they read a token here.
        ArgKind::Uuid | ArgKind::Entity | ArgKind::Entities | ArgKind::NbtPath => {
            let t = read_required(r)?;
            Ok((Value::Text(t.clone()), t))
        }
    }
}

/// One token, or an error when the line ended first.
fn read_required(r: &mut StringReader) -> Result<String, ParseError> {
    r.skip_whitespace();
    if !r.can_read() {
        return Err(ParseError::IncorrectArg);
    }
    Ok(r.read_token())
}

/// A brace-aware token, for block states, NBT and components.
fn read_component(r: &mut StringReader) -> Result<String, ParseError> {
    r.skip_whitespace();
    if !r.can_read() {
        return Err(ParseError::IncorrectArg);
    }
    let t = r.read_component_token();
    if t.is_empty() {
        return Err(ParseError::IncorrectArg);
    }
    Ok(t)
}

fn coordinates(r: &mut StringReader, block: bool) -> Result<Coordinates, ParseError> {
    let x = coordinate(r, block, 3)?;
    let y = coordinate(r, block, 3)?;
    let z = coordinate(r, block, 3)?;
    Ok(Coordinates::new(x, y, z))
}

fn coordinates2(r: &mut StringReader) -> Result<Coordinates2, ParseError> {
    let x = coordinate(r, false, 2)?;
    let y = coordinate(r, false, 2)?;
    Ok(Coordinates2::new(x, y))
}

fn coordinate(
    r: &mut StringReader,
    block: bool,
    expected: u8,
) -> Result<WorldCoordinate, ParseError> {
    r.skip_whitespace();
    if !r.can_read() {
        return Err(ParseError::IncompletePos { expected });
    }
    let t = r.read_token();
    if block {
        WorldCoordinate::parse_block(&t)
    } else {
        WorldCoordinate::parse(&t)
    }
    .map_err(coord_error)
}

fn coord_error(e: CoordError) -> ParseError {
    ParseError::Argument(e.message())
}

/// Resolve a block state through the vanilla block table, naming what is wrong.
fn parse_block_state(text: &str) -> Result<BlockStateId, ParseError> {
    let (base, props_str) = match text.split_once('[') {
        Some((base, rest)) => (base, Some(rest.strip_suffix(']').unwrap_or(rest))),
        None => (text, None),
    };
    let base = normalize_block(base);
    let block = blocks::block_id_of(&base)
        .ok_or_else(|| ParseError::Argument(format!("Unknown block type '{text}'")))?;
    match props_str {
        None => Ok(BlockStateId(blocks::DEFAULT_STATE[block as usize])),
        Some(list) => {
            if list.is_empty() {
                return Err(ParseError::Argument(format!(
                    "Invalid block state '{text}'"
                )));
            }
            let mut pairs = Vec::new();
            for kv in list.split(',') {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| ParseError::Argument(format!("Invalid block state '{text}'")))?;
                pairs.push((k.trim(), v.trim()));
            }
            let state = props::state_with(block, &pairs).ok_or_else(|| {
                ParseError::Argument(format!("Unknown property or value in block state '{text}'"))
            })?;
            Ok(BlockStateId(state))
        }
    }
}

fn parse_item_stack(r: &mut StringReader) -> Result<(Value, String), ParseError> {
    let start = r.cursor;
    let text = read_component(r)?;
    let (name, nbt) = match text.find('{') {
        Some(i) => (&text[..i], Some(&text[i..])),
        None => (text.as_str(), None),
    };
    if name.is_empty() {
        return Err(ParseError::Argument(format!("Invalid item '{text}'")));
    }
    let norm = normalize_resource(name);
    let item = ItemTypeId::vanilla(&norm)
        .ok_or_else(|| ParseError::Argument(format!("Unknown item '{name}'")))?;
    let nbt = match nbt {
        Some(s) => Some(
            snbt::parse_compound(s)
                .map_err(|e| ParseError::Argument(format!("Invalid component: {e}")))?,
        ),
        None => None,
    };
    let mut count = 1;
    let save = r.cursor;
    r.skip_whitespace();
    if r.can_read() {
        let token = r.read_token();
        match token.parse::<i32>() {
            Ok(n) if n > 0 => count = n,
            _ => r.cursor = save,
        }
    } else {
        r.cursor = save;
    }
    Ok((
        Value::ItemStack(ItemStack { item, count, nbt }),
        r.text_from(start),
    ))
}

fn parse_time(r: &mut StringReader) -> Result<(Value, String), ParseError> {
    let t = read_required(r)?;
    let (body, per) = match t.chars().last() {
        Some('d') | Some('D') => (&t[..t.len() - 1], 24_000.0),
        Some('s') | Some('S') => (&t[..t.len() - 1], 20.0),
        Some('t') | Some('T') => (&t[..t.len() - 1], 1.0),
        _ => (t.as_str(), 1.0),
    };
    let value: f64 = body
        .parse()
        .map_err(|_| ParseError::Argument(format!("Invalid time '{t}'")))?;
    Ok((Value::Int((value * per) as i32), t))
}

/// Add the default `minecraft:` namespace when the user left it off.
fn normalize_resource(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

/// The same, for a block's base name (before any `[...]`).
fn normalize_block(name: &str) -> String {
    normalize_resource(name)
}

/// A syntactically valid `namespace:path`.
fn valid_resource(name: &str) -> bool {
    let Some((ns, path)) = name.split_once(':') else {
        return false;
    };
    let ok_ns = !ns.is_empty()
        && ns
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    let ok_path = !path.is_empty()
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-' | '/')
        });
    ok_ns && ok_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::coordinates::Swizzle;
    use crate::commands::coordinates::{CommandSource, Rotation};
    use aether_api::Vector3 as Vec3;

    fn value(kind: ArgKind, text: &str) -> Result<Value, ParseError> {
        let mut r = StringReader::new(text);
        kind.parse(&mut r).map(|(v, _)| v)
    }

    fn err(kind: ArgKind, text: &str) -> ParseError {
        match value(kind, text) {
            Ok(v) => panic!("`{text}` parsed as {v:?}"),
            Err(e) => e,
        }
    }

    #[test]
    fn resource_locations_are_normalised_and_checked() {
        assert_eq!(
            value(ArgKind::ResourceLocation, "stone").unwrap(),
            Value::ResourceLocation("minecraft:stone".into())
        );
        assert_eq!(
            value(ArgKind::ResourceLocation, "mymod:widget").unwrap(),
            Value::ResourceLocation("mymod:widget".into())
        );
        assert_eq!(
            value(ArgKind::ResourceOrTag, "#minecraft:logs").unwrap(),
            Value::ResourceLocation("#minecraft:logs".into())
        );
        assert!(matches!(
            err(ArgKind::ResourceLocation, "BadName"),
            ParseError::Argument(_)
        ));
        assert!(matches!(
            err(ArgKind::ResourceLocation, "a:"),
            ParseError::Argument(_)
        ));
    }

    #[test]
    fn block_states_resolve_and_unknown_ones_error() {
        let Value::BlockState(id) = value(ArgKind::BlockState, "minecraft:stone").unwrap() else {
            panic!("not a block state")
        };
        assert_eq!(id, aether_world::registry::ids::STONE);

        let Value::BlockState(id) = value(ArgKind::BlockState, "oak_stairs[facing=east]").unwrap()
        else {
            panic!("not a block state")
        };
        // The property round-trips through the registry's own name.
        let reg = aether_world::registry::BlockRegistry::new();
        let name = reg.name_of(id).unwrap();
        assert!(name.contains("facing=east"), "{name}");

        // Unknown block, unknown property, bad value: all errors.
        for bad in [
            "minecraft:not_a_block",
            "minecraft:oak_stairs[colour=red]",
            "minecraft:oak_stairs[facing=upside]",
            "minecraft:oak_stairs[unterminated",
        ] {
            assert!(
                matches!(err(ArgKind::BlockState, bad), ParseError::Argument(_)),
                "{bad}"
            );
        }
    }

    #[test]
    fn item_stacks_carry_components_and_a_count() {
        let Value::ItemStack(stack) = value(ArgKind::ItemStack, "minecraft:stone").unwrap() else {
            panic!("not an item")
        };
        assert_eq!(stack.item, ItemTypeId::vanilla("minecraft:stone").unwrap());
        assert_eq!(stack.count, 1);
        assert!(stack.nbt.is_none());

        // Count with a space, per the brief's `minecraft:stone{…} 3`.
        let Value::ItemStack(stack) = value(
            ArgKind::ItemStack,
            "minecraft:stone{display:{Name:\"x\"}} 3",
        )
        .unwrap() else {
            panic!("not an item")
        };
        assert_eq!(stack.count, 3);
        assert!(matches!(stack.nbt, Some(Nbt::Compound(_))));

        assert!(matches!(
            err(ArgKind::ItemStack, "minecraft:not_an_item"),
            ParseError::Argument(_)
        ));
        // An invalid component compound errors.
        assert!(matches!(
            err(ArgKind::ItemStack, "minecraft:stone{nope}"),
            ParseError::Argument(_)
        ));
    }

    #[test]
    fn gamemodes_and_difficulties_accept_the_shorthands() {
        for (text, want) in [
            ("creative", "creative"),
            ("c", "creative"),
            ("1", "creative"),
        ] {
            assert_eq!(
                value(ArgKind::Gamemode, text).unwrap(),
                Value::Text(want.into())
            );
        }
        assert_eq!(
            value(ArgKind::Gamemode, "SP").unwrap(),
            Value::Text("spectator".into())
        );
        assert!(matches!(
            err(ArgKind::Gamemode, "sideways"),
            ParseError::Argument(_)
        ));
        assert_eq!(
            value(ArgKind::Difficulty, "p").unwrap(),
            Value::Text("peaceful".into())
        );
        assert!(matches!(
            err(ArgKind::Difficulty, "impossible"),
            ParseError::Argument(_)
        ));
    }

    #[test]
    fn time_reads_day_second_and_tick_suffixes() {
        assert_eq!(value(ArgKind::Time, "20").unwrap(), Value::Int(20));
        assert_eq!(value(ArgKind::Time, "1s").unwrap(), Value::Int(20));
        assert_eq!(value(ArgKind::Time, "1d").unwrap(), Value::Int(24_000));
        assert_eq!(value(ArgKind::Time, "0.5s").unwrap(), Value::Int(10));
        assert!(matches!(
            err(ArgKind::Time, "soon"),
            ParseError::Argument(_)
        ));
    }

    #[test]
    fn swizzles_angles_and_operations_validate() {
        assert_eq!(
            value(ArgKind::Swizzle, "xz").unwrap(),
            Value::Swizzle(Swizzle::XZ)
        );
        assert_eq!(err(ArgKind::Swizzle, "xx"), ParseError::IncorrectArg);
        assert_eq!(
            value(ArgKind::Angle, "~90").unwrap(),
            Value::Angle(WorldCoordinate::Relative(90.0))
        );
        for op in ["=", "+=", "-=", "*=", "/=", "%=", "<", ">", "><"] {
            assert_eq!(
                value(ArgKind::Operation, op).unwrap(),
                Value::Text(op.into())
            );
        }
        assert_eq!(err(ArgKind::Operation, "++"), ParseError::IncorrectArg);
    }

    #[test]
    fn nbt_and_components_parse_as_snbt() {
        assert!(matches!(
            value(ArgKind::NbtCompound, "{a:1,b:\"two\"}").unwrap(),
            Value::Nbt(Nbt::Compound(_))
        ));
        assert!(matches!(
            value(ArgKind::Component, "\"hello world\"").unwrap(),
            Value::Nbt(Nbt::Str(_))
        ));
        assert!(matches!(
            value(ArgKind::Component, "{text:\"hi\",bold:1b}").unwrap(),
            Value::Nbt(Nbt::Compound(_))
        ));
        assert!(matches!(
            err(ArgKind::NbtCompound, "[1,2]"),
            ParseError::Argument(_)
        ));
    }

    #[test]
    fn coordinate_kinds_read_three_and_two_tokens() {
        let Value::Coordinates(c) = value(ArgKind::BlockPos, "~0 ~ ~").unwrap() else {
            panic!("not coordinates")
        };
        assert!(c.is_relative());
        let Value::Coordinates(c) = value(ArgKind::Vec3, "1.5 ~ ^2").unwrap() else {
            panic!("not coordinates")
        };
        // Local mixed with world is only rejected at resolve time, not here.
        assert!(!c.is_local());
        let Value::Vec2(c) = value(ArgKind::Vec2, "1 2").unwrap() else {
            panic!("not a vec2")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(1.0));
        let Value::Rotation(c) = value(ArgKind::Rotation, "~90 ~").unwrap() else {
            panic!("not a rotation")
        };
        assert_eq!(c.x, WorldCoordinate::Relative(90.0));

        // Too few numbers is incomplete.
        assert!(matches!(
            err(ArgKind::BlockPos, "1 2"),
            ParseError::IncompletePos { .. }
        ));
        assert!(matches!(
            err(ArgKind::Vec2, "1"),
            ParseError::IncompletePos { .. }
        ));
    }

    #[test]
    fn message_is_greedy_and_quote_aware() {
        let Value::Text(m) = value(ArgKind::Message, "\"hello world\" /tail").unwrap() else {
            panic!("not text")
        };
        assert_eq!(m, "hello world /tail");
        let Value::Text(m) = value(ArgKind::Message, "plain text/with/slashes").unwrap() else {
            panic!("not text")
        };
        assert_eq!(m, "plain text/with/slashes");
        let Value::Text(m) = value(ArgKind::Message, "\"a\\\"b\"").unwrap() else {
            panic!("not text")
        };
        assert_eq!(m, "a\"b");
        // An unterminated quote is a syntax error, not an empty message.
        assert_eq!(err(ArgKind::Message, "\"oops"), ParseError::InvalidQuoted);
    }

    /// A tiny deterministic PRNG, so the fuzz-ish tests are reproducible.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 11
        }
    }

    struct Source;

    impl CommandSource for Source {
        fn position(&self) -> Option<Vec3> {
            Some(Vec3::new(0.5, 64.25, -0.5))
        }
        fn rotation(&self) -> Option<Rotation> {
            Some(Rotation::new(37.0, -21.0))
        }
    }

    fn random_coordinate_component(rng: &mut Lcg, block: bool) -> String {
        match rng.next() % 4 {
            0 => "~".to_string(),
            1 => format!("~{}", (rng.next() % 200) as i64 - 100),
            2 if !block => format!("^{}", (rng.next() % 200) as i64 - 100),
            _ => {
                let whole = (rng.next() % 200) as i64 - 100;
                if block || rng.next() % 2 == 0 {
                    format!("{whole}")
                } else {
                    format!("{whole}.{}", rng.next() % 100)
                }
            }
        }
    }

    #[test]
    fn random_valid_coordinates_parse_and_resolve() {
        let source = Source;
        let mut rng = Lcg(0x1234_5678_9abc_def0);
        for _ in 0..2_000 {
            let text = format!(
                "{} {} {}",
                random_coordinate_component(&mut rng, false),
                random_coordinate_component(&mut rng, false),
                random_coordinate_component(&mut rng, false)
            );
            let Ok(Value::Coordinates(c)) = value(ArgKind::Vec3, &text) else {
                panic!("valid coordinates `{text}` did not parse")
            };
            // A per-component local/world mix is the only failure allowed.
            match c.resolve(&source) {
                Ok(p) => assert!(
                    p.x.is_finite() && p.y.is_finite() && p.z.is_finite(),
                    "{p:?}"
                ),
                Err(CoordError::MixedLocal) => {}
                Err(e) => panic!("`{text}` failed to resolve: {e:?}"),
            }
            // The same text as a block position either parses or errors on a
            // fractional absolute; it must never panic.
            let _ = value(ArgKind::BlockPos, &text);
        }
    }

    #[test]
    fn garbage_never_panics() {
        const CHARSET: &[char] = &[
            '{', '}', '[', ']', '"', '\'', ':', ';', ',', '~', '^', '-', '+', '.', 'e', 'E', '0',
            '1', '9', 'a', 'Z', '#', '/', '\\', ' ', '\t', '\n', '=', '_',
        ];
        const KINDS: &[ArgKind] = &[
            ArgKind::String,
            ArgKind::GreedyString,
            ArgKind::Message,
            ArgKind::Integer,
            ArgKind::Long,
            ArgKind::Float,
            ArgKind::Double,
            ArgKind::Bool,
            ArgKind::BlockPos,
            ArgKind::Vec3,
            ArgKind::Vec2,
            ArgKind::Rotation,
            ArgKind::Swizzle,
            ArgKind::Angle,
            ArgKind::BlockState,
            ArgKind::ItemStack,
            ArgKind::NbtCompound,
            ArgKind::Component,
            ArgKind::ResourceLocation,
            ArgKind::ResourceOrTag,
            ArgKind::GameProfile,
            ArgKind::Time,
            ArgKind::Gamemode,
            ArgKind::Difficulty,
            ArgKind::Particle,
            ArgKind::ScoreHolder,
            ArgKind::Operation,
            ArgKind::NbtPath,
            ArgKind::Uuid,
        ];
        let mut rng = Lcg(0xdead_beef_cafe_f00d);
        for _ in 0..4_000 {
            let len = (rng.next() % 20) as usize;
            let s: String = (0..len)
                .map(|_| CHARSET[(rng.next() % CHARSET.len() as u64) as usize])
                .collect();
            for &kind in KINDS {
                let mut r = StringReader::new(&s);
                let _ = kind.parse(&mut r);
            }
            let _ = snbt::parse(&s);
        }
    }
}
