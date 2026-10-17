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
//! table, an item stack with its optional component patch.

use aether_world::registry::blocks;
use aether_world::registry::props;
use aether_world::{BlockStateId, ItemTypeId};

use super::coordinates::{CoordError, Coordinates, Coordinates2, Swizzle, WorldCoordinate};
use super::snbt;
use super::tree::{ArgKind, ParseError, StringReader, Value};
use crate::protocol::nbt::Nbt;

/// A block-state argument: the state id and the optional block-entity NBT a
/// `{…}` suffix carries (vanilla's `BlockArgumentParser.block(…, true)`).
#[derive(Clone, Debug, PartialEq)]
pub struct BlockState {
    /// The resolved block state.
    pub id: BlockStateId,
    /// The block-entity compound, if a `{…}` suffix was given.
    pub nbt: Option<Nbt>,
}

/// One entry of an item stack's `[…]` component patch.
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentPatch {
    /// `!name`: remove the component.
    Removed(String),
    /// `name=<snbt>`: add the component with an explicit value.
    Value(String, Nbt),
}

/// An item stack argument: an item and its optional component patch.
///
/// Vanilla 1.21.11's `ItemStringReader`: an item id followed by an optional
/// `[component=value,!component]` patch. There is no count here — a count is a
/// separate argument (`/give … <targets> <item> [<count>]`).
///
/// A component's *syntax* is checked (a resource location, `=` required,
/// no repeats); its *existence* is not, because the data-component registry
/// that would answer that is not part of CMD-06a. A component id is carried
/// through verbatim for the executor that owns it.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemStack {
    /// The item type.
    pub item: ItemTypeId,
    /// The component patch entries, in order; empty when no `[…]` was given.
    pub components: Vec<ComponentPatch>,
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
            let c = coordinates(r, true, false)?;
            Ok((Value::Coordinates(c), r.text_from(start)))
        }
        // `vec3(true)` centers absolute X and Z (not Y); `vec3(false)` is the
        // delta form `/particle` uses.
        ArgKind::Vec3(center) => {
            let start = r.cursor;
            let c = coordinates(r, false, center)?;
            Ok((Value::Coordinates(c), r.text_from(start)))
        }
        ArgKind::Vec2(center) => {
            let start = r.cursor;
            let c = coordinates2(r, center)?;
            Ok((Value::Vec2(c), r.text_from(start)))
        }
        ArgKind::Rotation => {
            let start = r.cursor;
            // A rotation is `vec2(false)`: never centered.
            let c = coordinates2(r, false)?;
            Ok((Value::Rotation(c), r.text_from(start)))
        }
        ArgKind::Swizzle => {
            let t = read_required(r)?;
            match Swizzle::parse(&t) {
                Some(s) => Ok((Value::Swizzle(s), t)),
                None => Err(ParseError::Argument(
                    "Invalid swizzle, expected combination of 'x', 'y' and 'z'".into(),
                )),
            }
        }
        ArgKind::Angle => {
            let t = read_required(r)?;
            let c = WorldCoordinate::parse(&t)
                .map_err(|_| ParseError::Argument("Invalid angle".into()))?;
            Ok((Value::Angle(c), t))
        }
        // The block, item, NBT and registry-backed kinds.
        ArgKind::BlockState => {
            let t = read_component(r)?;
            let bs = parse_block_state(&t)?;
            Ok((Value::BlockState(bs), t))
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
            let t = read_selector_token(r)?;
            Ok((Value::Text(t.clone()), t))
        }
        ArgKind::Time => parse_time(r),
        ArgKind::Gamemode => {
            let t = read_required(r)?;
            // `GameMode.CODEC`/`GameType.byName`: an exact, lowercase name only.
            match t.as_str() {
                "survival" | "creative" | "adventure" | "spectator" => {
                    Ok((Value::Text(t.clone()), t))
                }
                _ => Err(ParseError::Argument(format!("Unknown game mode '{t}'"))),
            }
        }
        ArgKind::Difficulty => {
            let t = read_required(r)?;
            // `/difficulty` takes its full literals; there are no initials.
            match t.as_str() {
                "peaceful" | "easy" | "normal" | "hard" => Ok((Value::Text(t.clone()), t)),
                _ => Err(ParseError::Argument(format!("Unknown difficulty '{t}'"))),
            }
        }
        ArgKind::Particle => {
            // A particle id, then an optional `{…}` options object. Vanilla
            // skips whitespace before the `{`, so `dust {…}` is the same
            // argument as `dust{…}`; when no object follows, the cursor is
            // left before that whitespace for the next argument.
            let start = r.cursor;
            let mut t = read_component(r)?;
            let after_id = r.cursor;
            r.skip_whitespace();
            if r.peek() == Some('{') {
                t.push_str(&r.read_component_token());
            } else {
                r.cursor = after_id;
            }
            Ok((Value::Text(t.clone()), r.text_from(start)))
        }
        ArgKind::ScoreHolder => {
            let t = read_selector_token(r)?;
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
        // `uuid` and `nbt_path` are single tokens; the entity kinds are
        // CMD-06b's, read as one selector-aware token so a quoted filter with
        // a space (`@a[name="Bob Smith"]`) stays whole.
        ArgKind::Entity | ArgKind::Entities => {
            let t = read_selector_token(r)?;
            Ok((Value::Text(t.clone()), t))
        }
        ArgKind::Uuid | ArgKind::NbtPath => {
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

/// A selector-or-name token: whitespace inside `[...]` and quotes does not end
/// it, and a plain `"name"` loses its outer quotes. Entity, player and game
/// profile arguments use this so `@a[name="Bob Smith"]` is one argument.
fn read_selector_token(r: &mut StringReader) -> Result<String, ParseError> {
    let t = read_component(r)?;
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        Ok(t[1..t.len() - 1].to_string())
    } else {
        Ok(t)
    }
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

fn coordinates(r: &mut StringReader, block: bool, center: bool) -> Result<Coordinates, ParseError> {
    // `Vec3ArgumentType` centers X and Z (when `centerIntegers`), never Y.
    let x = coordinate(r, block, 3, center)?;
    let y = coordinate(r, block, 3, false)?;
    let z = coordinate(r, block, 3, center)?;
    Ok(Coordinates::new(x, y, z))
}

fn coordinates2(r: &mut StringReader, center: bool) -> Result<Coordinates2, ParseError> {
    // `Vec2ArgumentType` centers both components.
    let x = coordinate(r, false, 2, center)?;
    let y = coordinate(r, false, 2, center)?;
    Ok(Coordinates2::new(x, y))
}

fn coordinate(
    r: &mut StringReader,
    block: bool,
    expected: u8,
    center: bool,
) -> Result<WorldCoordinate, ParseError> {
    r.skip_whitespace();
    if !r.can_read() {
        return Err(ParseError::IncompletePos { expected });
    }
    let t = r.read_token();
    let parsed = if block {
        WorldCoordinate::parse_block(&t)
    } else if center {
        WorldCoordinate::parse_centered(&t)
    } else {
        WorldCoordinate::parse(&t)
    };
    parsed.map_err(coord_error)
}

fn coord_error(e: CoordError) -> ParseError {
    ParseError::Argument(e.message())
}

/// Resolve a block state through the vanilla block table, naming what is wrong.
///
/// The shape is `id[prop=value,…]{blockEntityNbt}`; both the property list and
/// the NBT are optional (vanilla's `BlockArgumentParser.block(…, true)`).
pub(crate) fn parse_block_state(text: &str) -> Result<BlockState, ParseError> {
    // Split off an optional `{…}` block-entity tag first: it comes last, and
    // the base/property part never contains a `{`.
    let (head, nbt) = match text.find('{') {
        Some(i) => {
            if !text.ends_with('}') {
                return Err(ParseError::Argument(format!(
                    "Invalid block state '{text}'"
                )));
            }
            let nbt = snbt::parse_compound(&text[i..])
                .map_err(|e| ParseError::Argument(format!("Invalid block state '{text}': {e}")))?;
            (&text[..i], Some(nbt))
        }
        None => (text, None),
    };
    let (base, props_str) = match head.split_once('[') {
        Some((base, rest)) => {
            let rest = rest
                .strip_suffix(']')
                .ok_or_else(|| ParseError::Argument(format!("Invalid block state '{text}'")))?;
            (base, Some(rest))
        }
        None => (head, None),
    };
    let base = normalize_block(base);
    let block = blocks::block_id_of(&base)
        .ok_or_else(|| ParseError::Argument(format!("Unknown block type '{text}'")))?;
    let id = match props_str {
        // `stone` and `stone[]` both mean the block's default state; an empty
        // property list is accepted, as vanilla's parser never enters the loop.
        None => BlockStateId(blocks::DEFAULT_STATE[block as usize]),
        Some(list) if list.trim().is_empty() => BlockStateId(blocks::DEFAULT_STATE[block as usize]),
        Some(list) => {
            let mut pairs: Vec<(&str, &str)> = Vec::new();
            let mut seen: Vec<&str> = Vec::new();
            for kv in list.split(',') {
                let Some((k, v)) = kv.split_once('=') else {
                    // `argument.block.property.novalue`.
                    return Err(ParseError::Argument(format!(
                        "Expected value for property in block state '{text}'"
                    )));
                };
                let (k, v) = (k.trim(), v.trim());
                if seen.contains(&k) {
                    // `argument.block.property.duplicate`.
                    return Err(ParseError::Argument(format!(
                        "Property '{k}' is already defined for block '{base}'"
                    )));
                }
                seen.push(k);
                pairs.push((k, v));
            }
            let state = props::state_with(block, &pairs).ok_or_else(|| {
                // `argument.block.property.unknown` / `.invalid` share a body
                // here: `props::state_with` cannot tell them apart.
                ParseError::Argument(format!("Unknown property or value in block state '{text}'"))
            })?;
            BlockStateId(state)
        }
    };
    Ok(BlockState { id, nbt })
}

fn parse_item_stack(r: &mut StringReader) -> Result<(Value, String), ParseError> {
    let start = r.cursor;
    let text = read_component(r)?;
    // `ItemStringReader`: `id` then an optional `[…]` component patch.
    let id_end = text.find('[').unwrap_or(text.len());
    let name = &text[..id_end];
    if name.is_empty() {
        return Err(ParseError::Argument(format!("Invalid item '{text}'")));
    }
    let norm = normalize_resource(name);
    let item = ItemTypeId::vanilla(&norm)
        .ok_or_else(|| ParseError::Argument(format!("Unknown item '{name}'")))?;
    let mut components = Vec::new();
    if id_end < text.len() {
        // The token is brace/bracket-balanced, so a patch ends with `]`.
        if !text.ends_with(']') || text.len() < id_end + 2 {
            return Err(ParseError::Argument(format!("Invalid item '{text}'")));
        }
        let inner = &text[id_end + 1..text.len() - 1];
        components = parse_component_patch(inner, &text)?;
    }
    Ok((
        Value::ItemStack(ItemStack { item, components }),
        r.text_from(start),
    ))
}

/// Parse the inside of an item's `[…]` patch.
///
/// Vanilla `ItemStringReader`: every entry is either `!name` (remove) or
/// `name=<snbt>` (add) — a bare `name` with no `=` is an error
/// (`arguments.item.component.expected`). A component named twice is
/// `arguments.item.component.repeated`.
fn parse_component_patch(inner: &str, whole: &str) -> Result<Vec<ComponentPatch>, ParseError> {
    let invalid = || ParseError::Argument(format!("Invalid item components '{}'", whole));
    if inner.trim().is_empty() {
        return Err(invalid());
    }
    let mut out: Vec<ComponentPatch> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for entry in split_top_level(inner) {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(invalid());
        }
        let patch = if let Some(rest) = entry.strip_prefix('!') {
            let name = component_name(rest).ok_or_else(invalid)?;
            ComponentPatch::Removed(name)
        } else if let Some((name, value)) = entry.split_once('=') {
            let name = component_name(name).ok_or_else(invalid)?;
            let value = snbt::parse(value.trim()).map_err(|_| invalid())?;
            ComponentPatch::Value(name, value)
        } else {
            // No `=` and no `!`: vanilla's `reader.expect('=')` fails here.
            return Err(ParseError::Argument(
                "Expected a component value after '='".into(),
            ));
        };
        let name = match &patch {
            ComponentPatch::Removed(n) | ComponentPatch::Value(n, _) => n.clone(),
        };
        if seen.contains(&name) {
            return Err(ParseError::Argument(format!(
                "Component '{name}' is already defined"
            )));
        }
        seen.push(name);
        out.push(patch);
    }
    Ok(out)
}

/// A component id, namespaced if the user left the namespace off.
fn component_name(name: &str) -> Option<String> {
    let norm = normalize_resource(name.trim());
    valid_resource(&norm).then_some(norm)
}

/// Split on top-level commas: commas inside `{}`, `[]` or quotes are kept.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_time(r: &mut StringReader) -> Result<(Value, String), ParseError> {
    let t = read_required(r)?;
    // Strict, lowercase unit suffixes: `d`/`s`/`t` or none. `1D` is an error.
    let (body, per) = match t.chars().last() {
        Some('d') => (&t[..t.len() - 1], 24_000.0),
        Some('s') => (&t[..t.len() - 1], 20.0),
        Some('t') => (&t[..t.len() - 1], 1.0),
        // `argument.time.invalid_unit`: any other trailing letter, or a letter
        // anywhere the number should be.
        Some(c) if c.is_ascii_alphabetic() => {
            return Err(ParseError::Argument("Invalid unit".into()))
        }
        _ => (t.as_str(), 1.0),
    };
    let value: f64 = body
        .parse()
        .map_err(|_| ParseError::Argument(format!("Invalid time '{t}'")))?;
    // `Math.round(amount * ticksPerUnit)`, not a truncation. Brigadier's time
    // argument is an `int`: saturate instead of wrapping, so
    // `/time set 3000000000` cannot turn negative on the way in. The minimum
    // is per-command (`time()` is 0, `time(1)` is 1), so it is checked by the
    // executor rather than here.
    let ticks = (value * per).round() as i64;
    let ticks = ticks.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    Ok((Value::Int(ticks as i32), t))
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
pub(crate) fn valid_resource(name: &str) -> bool {
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
    use crate::commands::coordinates::{CoordinateSource, Rotation};
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
        let Value::BlockState(bs) = value(ArgKind::BlockState, "minecraft:stone").unwrap() else {
            panic!("not a block state")
        };
        assert_eq!(bs.id, aether_world::registry::ids::STONE);
        assert!(bs.nbt.is_none());

        let Value::BlockState(bs) = value(ArgKind::BlockState, "oak_stairs[facing=east]").unwrap()
        else {
            panic!("not a block state")
        };
        // The property round-trips through the registry's own name.
        let reg = aether_world::registry::BlockRegistry::new();
        let name = reg.name_of(bs.id).unwrap();
        assert!(name.contains("facing=east"), "{name}");

        // Vanilla allows a `{…}` block-entity tag after the state.
        let Value::BlockState(bs) = value(
            ArgKind::BlockState,
            "minecraft:chest{Items:[],CustomName:\"x\"}",
        )
        .unwrap() else {
            panic!("not a block state")
        };
        assert!(matches!(bs.nbt, Some(Nbt::Compound(_))));

        // An empty property list is accepted and means the default state.
        let Value::BlockState(bs) = value(ArgKind::BlockState, "minecraft:stone[]").unwrap() else {
            panic!("not a block state")
        };
        assert_eq!(bs.id, aether_world::registry::ids::STONE);

        // Unknown block, unknown property, bad value, duplicate property: all
        // errors.
        for bad in [
            "minecraft:not_a_block",
            "minecraft:oak_stairs[colour=red]",
            "minecraft:oak_stairs[facing=upside]",
            "minecraft:oak_stairs[facing=east,facing=west]",
            "minecraft:oak_stairs[facing]",
            "minecraft:oak_stairs[unterminated",
            "minecraft:stone{unterminated",
        ] {
            assert!(
                matches!(err(ArgKind::BlockState, bad), ParseError::Argument(_)),
                "{bad}"
            );
        }
    }

    #[test]
    fn item_stacks_use_the_bracket_component_patch() {
        let Value::ItemStack(stack) = value(ArgKind::ItemStack, "minecraft:stone").unwrap() else {
            panic!("not an item")
        };
        assert_eq!(stack.item, ItemTypeId::vanilla("minecraft:stone").unwrap());
        assert!(stack.components.is_empty());

        // 1.21.11 syntax: `id[component=value,!component]`.
        let Value::ItemStack(stack) = value(
            ArgKind::ItemStack,
            "minecraft:stone[minecraft:damage=5,!minecraft:foo]",
        )
        .unwrap() else {
            panic!("not an item")
        };
        assert_eq!(stack.components.len(), 2);
        assert_eq!(
            stack.components[0],
            ComponentPatch::Value("minecraft:damage".into(), Nbt::Int(5))
        );
        assert_eq!(
            stack.components[1],
            ComponentPatch::Removed("minecraft:foo".into())
        );

        // Namespacing is optional.
        let Value::ItemStack(stack) = value(ArgKind::ItemStack, "stone[damage=5]").unwrap() else {
            panic!("not an item")
        };
        assert_eq!(
            stack.components[0],
            ComponentPatch::Value("minecraft:damage".into(), Nbt::Int(5))
        );

        assert!(matches!(
            err(ArgKind::ItemStack, "minecraft:not_an_item"),
            ParseError::Argument(_)
        ));
        // The old `{…}`-NBT form is not vanilla 1.21.11, nor is a built-in count.
        assert!(matches!(
            err(ArgKind::ItemStack, "minecraft:stone{nope}"),
            ParseError::Argument(_)
        ));
        // A bare component name needs `=`, and a repeat is refused.
        for bad in [
            "stone[]",
            "stone[=5]",
            "stone[!]",
            "stone[damage=]",
            "stone[unbreakable]",
            "stone[damage=5,damage=6]",
            "stone[!foo,foo=1]",
            "stone[a=1",
        ] {
            assert!(
                matches!(err(ArgKind::ItemStack, bad), ParseError::Argument(_)),
                "{bad}"
            );
        }
    }

    #[test]
    fn gamemodes_and_difficulties_take_only_the_full_names() {
        for name in ["survival", "creative", "adventure", "spectator"] {
            assert_eq!(
                value(ArgKind::Gamemode, name).unwrap(),
                Value::Text(name.into())
            );
        }
        // Initials, digits, mixed case and unknown words are all refused,
        // exactly as `GameType.byName` does.
        for bad in ["c", "s", "sp", "0", "1", "Creative", "sideways"] {
            assert!(
                matches!(err(ArgKind::Gamemode, bad), ParseError::Argument(_)),
                "{bad}"
            );
        }
        for name in ["peaceful", "easy", "normal", "hard"] {
            assert_eq!(
                value(ArgKind::Difficulty, name).unwrap(),
                Value::Text(name.into())
            );
        }
        for bad in ["p", "e", "n", "h", "Peaceful", "impossible"] {
            assert!(
                matches!(err(ArgKind::Difficulty, bad), ParseError::Argument(_)),
                "{bad}"
            );
        }
    }

    #[test]
    fn time_rounds_and_takes_only_d_s_t() {
        assert_eq!(value(ArgKind::Time, "20").unwrap(), Value::Int(20));
        assert_eq!(value(ArgKind::Time, "1s").unwrap(), Value::Int(20));
        assert_eq!(value(ArgKind::Time, "1d").unwrap(), Value::Int(24_000));
        assert_eq!(value(ArgKind::Time, "0.5s").unwrap(), Value::Int(10));
        // `Math.round`, not truncation: 0.9 ticks rounds up to 1.
        assert_eq!(value(ArgKind::Time, "0.9t").unwrap(), Value::Int(1));
        assert_eq!(value(ArgKind::Time, "0.4t").unwrap(), Value::Int(0));
        // `argument.time.invalid_unit`: `1D`/`1S`/`1T` and other letters error.
        for bad in ["1D", "1S", "1T", "10x", "soon"] {
            let e = err(ArgKind::Time, bad);
            assert!(
                matches!(&e, ParseError::Argument(m) if m == "Invalid unit"),
                "{bad} => {e:?}"
            );
        }
        // The minimum is the argument's flag (`time()` 0, `time(1)` 1) and is
        // checked by the command, so a negative still parses here.
        assert_eq!(value(ArgKind::Time, "-1").unwrap(), Value::Int(-1));
        assert_eq!(value(ArgKind::Time, "-1t").unwrap(), Value::Int(-1));
        // Exactly zero is fine.
        assert_eq!(value(ArgKind::Time, "0t").unwrap(), Value::Int(0));
    }

    #[test]
    fn swizzles_angles_and_operations_validate() {
        assert_eq!(
            value(ArgKind::Swizzle, "xz").unwrap(),
            Value::Swizzle(Swizzle::XZ)
        );
        // Any order/subset is fine; a repeat or uppercase is not.
        assert_eq!(
            value(ArgKind::Swizzle, "zx").unwrap(),
            Value::Swizzle(Swizzle::XZ)
        );
        let e = err(ArgKind::Swizzle, "xx");
        assert!(
            matches!(&e, ParseError::Argument(m) if m.contains("Invalid swizzle")),
            "{e:?}"
        );
        assert_eq!(
            value(ArgKind::Angle, "~90").unwrap(),
            Value::Angle(WorldCoordinate::Relative(90.0))
        );
        // An angle that is not a number errors with the vanilla key.
        let e = err(ArgKind::Angle, "sideways");
        assert!(
            matches!(&e, ParseError::Argument(m) if m == "Invalid angle"),
            "{e:?}"
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
        let Value::Coordinates(c) = value(ArgKind::Vec3(true), "1.5 ~ ^2").unwrap() else {
            panic!("not coordinates")
        };
        // Local mixed with world is only rejected at resolve time, not here.
        assert!(!c.is_local());
        // `vec2(true)` centers both components, so 1 becomes 1.5.
        let Value::Vec2(c) = value(ArgKind::Vec2(true), "1 2").unwrap() else {
            panic!("not a vec2")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(1.5));
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
            err(ArgKind::Vec2(true), "1"),
            ParseError::IncompletePos { .. }
        ));
    }

    #[test]
    fn vec3_centers_absolute_x_and_z_but_not_y() {
        // `vec3(true)`: `/tp 10 64 10` names block centres 10.5 / 64.0 / 10.5.
        let Value::Coordinates(c) = value(ArgKind::Vec3(true), "10 64 10").unwrap() else {
            panic!("not coordinates")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(10.5));
        assert_eq!(c.y, WorldCoordinate::Absolute(64.0));
        assert_eq!(c.z, WorldCoordinate::Absolute(10.5));

        // `vec3(false)` is the raw delta form: no centering.
        let Value::Coordinates(c) = value(ArgKind::Vec3(false), "10 64 10").unwrap() else {
            panic!("not coordinates")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(10.0));
        assert_eq!(c.z, WorldCoordinate::Absolute(10.0));

        // A component written with a decimal point is NOT centered (vanilla
        // checks the text for `.`), even under `centerIntegers`.
        let Value::Coordinates(c) = value(ArgKind::Vec3(true), "10.0 64 10.").unwrap() else {
            panic!("not coordinates")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(10.0));
        assert_eq!(c.z, WorldCoordinate::Absolute(10.0));
        // A dotted fractional value is left as written (and Y never centers).
        let Value::Coordinates(c) = value(ArgKind::Vec3(true), "10.5 64.5 10.5").unwrap() else {
            panic!("not coordinates")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(10.5));
        assert_eq!(c.y, WorldCoordinate::Absolute(64.5));

        // Relative and local components are never centered.
        let Value::Coordinates(c) = value(ArgKind::Vec3(true), "~10 ~64 ~10").unwrap() else {
            panic!("not coordinates")
        };
        assert_eq!(c.x, WorldCoordinate::Relative(10.0));
        assert_eq!(c.z, WorldCoordinate::Relative(10.0));

        // `vec2(true)` centers both components.
        let Value::Vec2(c) = value(ArgKind::Vec2(true), "3 4").unwrap() else {
            panic!("not a vec2")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(3.5));
        assert_eq!(c.y, WorldCoordinate::Absolute(4.5));
        // A rotation is `vec2(false)`, so it is never centered.
        let Value::Rotation(c) = value(ArgKind::Rotation, "90 0").unwrap() else {
            panic!("not a rotation")
        };
        assert_eq!(c.x, WorldCoordinate::Absolute(90.0));
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

    impl CoordinateSource for Source {
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
            let Ok(Value::Coordinates(c)) = value(ArgKind::Vec3(true), &text) else {
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
            ArgKind::Vec3(true),
            ArgKind::Vec3(false),
            ArgKind::Vec2(true),
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
