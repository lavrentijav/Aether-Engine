//! `/particle`, `/playsound` and `/stopsound`: the client-effects commands.
//!
//! These are deliberately thin: the game's state is not changed, only what a
//! client is told to draw or play. The command layer resolves the vanilla
//! argument grammar (including CMD-06b selectors and CMD-06a coordinates) and
//! sends one neutral [`ServerEvent`] per recipient; each codec renders it in
//! its own release's packet shape.
//!
//! `/particle`'s name is vanilla's `minecraft:particle` argument: a particle
//! type followed, for the types whose codec has fields, by a JSON options
//! object — `dust{color:[1.0,0.0,0.0],scale:1.0}`,
//! `block{block_state:"minecraft:stone"}`, `item{item:{id:"minecraft:stone"}}`.
//! The object is parsed here against the 1.21.11 schemas and carried as
//! [`ParticleOptions`]; the codec writes the field shape its own release
//! expects (the dust colour was four floats through 1.21.3 and one int from
//! 1.21.4).
//!
//! Resource names are validated against the 1.21.11 registries. A particle
//! name another release does not have is a no-op for that client (cosmetic,
//! never a malformed packet); a sound id is sent inline, as vanilla does.

use std::sync::Arc;

use super::args;
use super::coordinates::{CoordinateSource, Coordinates, Rotation};
use super::feedback::lang;
use super::selector::{EntityRef, Gamemode, LcgRng, Selector, SelectorSource};
use super::snbt;
use super::tree::{ArgKind, Builder, Ctx, Node, Requirement, Source, Value};
use super::Reply;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::nbt::Nbt;
use crate::protocol::{resources, ParticleOptions, ServerEvent};
use aether_api::Vector3;
use aether_world::ItemTypeId;

/// Vanilla's particle distance limit when `force` is not given, in blocks.
const PARTICLE_RANGE: f64 = 32.0;
/// The distance limit with `force`, in blocks.
const PARTICLE_RANGE_FORCED: f64 = 512.0;

/// Every command node this module owns.
pub fn nodes() -> Vec<Node> {
    vec![particle_node(), playsound_node(), stopsound_node()]
}

fn gate(node: &'static str) -> Requirement {
    Requirement::Level { op: 2, node }
}

// ---------------------------------------------------------------------------
// Shared argument helpers
// ---------------------------------------------------------------------------

/// A source with no position, for resolving absolute coordinates typed at the
/// console. `~` and `^` still fail, exactly as they do everywhere else.
struct NoPosition;

impl CoordinateSource for NoPosition {
    fn position(&self) -> Option<Vector3> {
        None
    }
    fn rotation(&self) -> Option<Rotation> {
        None
    }
}

fn text_arg<'a>(ctx: &'a Ctx<'_, '_>, index: usize) -> Option<&'a str> {
    match &ctx.args.get(index)?.value {
        Value::Text(text) | Value::ResourceLocation(text) => Some(text),
        _ => Some(&ctx.args[index].text),
    }
}

fn int_arg(ctx: &Ctx, index: usize) -> Option<i32> {
    match &ctx.args.get(index)?.value {
        Value::Int(value) => Some(*value),
        _ => ctx.args[index].text.parse().ok(),
    }
}

fn float_arg(ctx: &Ctx, index: usize) -> Option<f32> {
    match &ctx.args.get(index)?.value {
        Value::Float(value) => Some(*value),
        Value::Double(value) => Some(*value as f32),
        Value::Int(value) => Some(*value as f32),
        _ => ctx.args[index].text.parse().ok(),
    }
}

fn coords_arg(ctx: &Ctx, index: usize) -> Option<Coordinates> {
    match &ctx.args.get(index)?.value {
        Value::Coordinates(c) => Some(*c),
        _ => None,
    }
}

fn resolve(ctx: &Ctx, c: Coordinates) -> Result<Vector3, String> {
    let result = match ctx.player() {
        Some(handle) => c.resolve(handle),
        None => c.resolve(&NoPosition),
    };
    result.map_err(|e| e.message())
}

fn source_position(ctx: &Ctx) -> Result<Vector3, String> {
    match ctx.player() {
        Some(handle) => {
            let p = handle.pos();
            Ok(Vector3::new(p.x, p.y, p.z))
        }
        None => Err("Unknown position".into()),
    }
}

/// A namespaced resource id, with `minecraft:` supplied when omitted.
fn normalize(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

/// A sound id: a syntactically valid `namespace:path`, namespaced if needed.
///
/// `/playsound` and `/stopsound` take vanilla's plain `resource_location`,
/// not a registry entry: a resource pack may define its own sound ids, and
/// vanilla accepts them. Only `/particle` checks its registry, because a
/// particle id is a codec-level enum a client cannot invent.
fn sound_resource(name: &str) -> Result<String, String> {
    let name = normalize(name);
    if !args::valid_resource(&name) {
        return Err(format!("Invalid resource location '{name}'"));
    }
    Ok(name)
}

fn source_name(source: u8) -> &'static str {
    match source {
        0 => "master",
        1 => "music",
        2 => "record",
        3 => "weather",
        4 => "block",
        5 => "hostile",
        6 => "neutral",
        7 => "player",
        8 => "ambient",
        9 => "voice",
        _ => "ui",
    }
}

// ---------------------------------------------------------------------------
// Selectors
// ---------------------------------------------------------------------------

/// The CMD-06b selector source over the live player registry.
struct Players<'a> {
    registry: &'a SharedRegistry,
    executor: Option<EntityRef>,
}

fn player_ref(handle: &PlayerHandle) -> EntityRef {
    let pos = handle.pos();
    EntityRef {
        id: handle.entity_id,
        uuid: handle.uuid,
        kind: "minecraft:player".into(),
        is_player: true,
        alive: true,
        name: Some(handle.name.clone()),
        pos: Vector3::new(pos.x, pos.y, pos.z),
        rotation: Rotation::new(pos.yaw, pos.pitch),
        tags: Vec::new(),
        team: None,
        gamemode: Some(Gamemode::from(handle.game().mode)),
        level: 0,
        scores: Vec::new(),
        advancements: Vec::new(),
        predicates: Vec::new(),
        nbt: Nbt::Compound(Vec::new()),
    }
}

impl SelectorSource for Players<'_> {
    fn executor(&self) -> Option<EntityRef> {
        self.executor.clone()
    }

    fn players(&self) -> Vec<EntityRef> {
        self.registry
            .snapshot()
            .iter()
            .map(|h| player_ref(h))
            .collect()
    }
}

/// Resolve a target list: a CMD-06b selector or a plain online player name.
fn resolve_players(
    registry: &SharedRegistry,
    source: Source<'_>,
    text: &str,
) -> Result<Vec<Arc<PlayerHandle>>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("No player was found".into());
    }
    if text.starts_with('@') {
        let selector = Selector::parse_players(text).map_err(|e| e.message())?;
        let players = Players {
            registry,
            executor: source.player().map(player_ref),
        };
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let found = selector
            .select(&players, &mut LcgRng::new(seed))
            .map_err(|e| e.message())?;
        let mut out = Vec::with_capacity(found.len());
        for entity in found {
            let handle = registry.by_uuid(entity.uuid).or_else(|| {
                let name = entity.name.clone().unwrap_or_default();
                registry
                    .snapshot()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&name))
            });
            if let Some(handle) = handle {
                out.push(handle);
            }
        }
        if out.is_empty() {
            return Err("No player was found".into());
        }
        return Ok(out);
    }
    registry
        .snapshot()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(text))
        .map(|p| vec![p])
        .ok_or_else(|| "No player was found".into())
}

// ---------------------------------------------------------------------------
// Particle options
// ---------------------------------------------------------------------------

/// The 1.21.11 particles whose argument carries an options object.
///
/// Everything else is a `SimpleParticleType`, whose argument ends after the
/// id. The set is the one whose codec has fields, not the set whose wire
/// payload is non-empty: some of these had no wire fields on older releases
/// (`entity_effect` before 1.20.5, `effect` before 1.21.9), and the codec
/// writes the fields its own release has.
const PARTICLE_OPTION_NAMES: &[&str] = &[
    "minecraft:block",
    "minecraft:block_marker",
    "minecraft:falling_dust",
    "minecraft:dust_pillar",
    "minecraft:block_crumble",
    "minecraft:dust",
    "minecraft:dust_color_transition",
    "minecraft:entity_effect",
    "minecraft:item",
    "minecraft:sculk_charge",
    "minecraft:shriek",
    "minecraft:vibration",
    "minecraft:trail",
    "minecraft:tinted_leaves",
    "minecraft:dragon_breath",
    "minecraft:effect",
    "minecraft:instant_effect",
    "minecraft:flash",
];

fn particle_has_options(name: &str) -> bool {
    PARTICLE_OPTION_NAMES.contains(&name)
}

/// The error `CommandSyntaxException` body for a bad options object.
fn invalid_options(detail: impl std::fmt::Display) -> String {
    lang::format(lang::PARTICLE_INVALID_OPTIONS, &[&detail.to_string()])
}

fn field<'a>(obj: &'a [(String, Nbt)], key: &str) -> Result<&'a Nbt, String> {
    obj.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .ok_or_else(|| format!("missing field `{key}`"))
}

fn number(value: &Nbt) -> Option<f64> {
    Some(match value {
        Nbt::Byte(v) => f64::from(*v),
        Nbt::Short(v) => f64::from(*v),
        Nbt::Int(v) => f64::from(*v),
        Nbt::Long(v) => *v as f64,
        Nbt::Float(v) => f64::from(*v),
        Nbt::Double(v) => *v,
        _ => return None,
    })
}

fn integer(value: &Nbt) -> Option<i64> {
    match value {
        Nbt::Byte(v) => Some(i64::from(*v)),
        Nbt::Short(v) => Some(i64::from(*v)),
        Nbt::Int(v) => Some(i64::from(*v)),
        Nbt::Long(v) => Some(*v),
        Nbt::Float(v) if v.fract() == 0.0 => Some(*v as i64),
        Nbt::Double(v) if v.fract() == 0.0 => Some(*v as i64),
        _ => None,
    }
}

fn as_f32(value: &Nbt, key: &str) -> Result<f32, String> {
    number(value)
        .map(|v| v as f32)
        .ok_or_else(|| format!("expected a float for `{key}`"))
}

fn as_i32(value: &Nbt, key: &str) -> Result<i32, String> {
    let n = integer(value).ok_or_else(|| format!("expected an integer for `{key}`"))?;
    i32::try_from(n).map_err(|_| format!("`{key}` is out of range"))
}

fn as_str<'a>(value: &'a Nbt, key: &str) -> Result<&'a str, String> {
    match value {
        Nbt::Str(text) => Ok(text),
        _ => Err(format!("expected a string for `{key}`")),
    }
}

/// An RGB colour: an integer (`-1`, `0xFF0000`) or three floats (`[1.0,0,0]`).
///
/// Vanilla's `ExtraCodecs.RGB_COLOR_CODEC` accepts either form; the float
/// list is packed as opaque ARGB, exactly as `FastColor.ARGB32.colorFromFloat`
/// does.
fn as_color(value: &Nbt) -> Result<i32, String> {
    if let Some(n) = integer(value) {
        return Ok(n as i32);
    }
    let Nbt::List(list) = value else {
        return Err("expected an integer or a list of three floats".into());
    };
    if list.len() != 3 {
        return Err("expected a list of three floats".into());
    }
    let mut rgb = [0u8; 3];
    for (slot, value) in rgb.iter_mut().zip(list) {
        let f = number(value).ok_or_else(|| "expected a number in a colour list".to_string())?;
        *slot = (f.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    Ok(i32::from_be_bytes([0xFF, rgb[0], rgb[1], rgb[2]]))
}

fn as_vec3(value: &Nbt, key: &str) -> Result<(f64, f64, f64), String> {
    let Nbt::List(list) = value else {
        return Err(format!("expected a list of three numbers for `{key}`"));
    };
    if list.len() != 3 {
        return Err(format!("expected a list of three numbers for `{key}`"));
    }
    let mut out = [0.0; 3];
    for (slot, value) in out.iter_mut().zip(list) {
        *slot = number(value).ok_or_else(|| format!("expected a number in `{key}`"))?;
    }
    Ok((out[0], out[1], out[2]))
}

fn as_block_pos(value: &Nbt, key: &str) -> Result<(i32, i32, i32), String> {
    let (x, y, z) = as_vec3(value, key)?;
    let int = |v: f64| {
        if v.fract() == 0.0 {
            Ok(v as i32)
        } else {
            Err(format!("expected whole numbers in `{key}`"))
        }
    };
    Ok((int(x)?, int(y)?, int(z)?))
}

fn parse_item(value: &Nbt) -> Result<(ItemTypeId, u8), String> {
    let Nbt::Compound(stack) = value else {
        return Err("expected an item stack object".into());
    };
    let id = as_str(field(stack, "id")?, "id")?;
    let id = normalize(id);
    if !args::valid_resource(&id) {
        return Err(format!("invalid item id '{id}'"));
    }
    let item = ItemTypeId::vanilla(&id).ok_or_else(|| format!("unknown item '{id}'"))?;
    let count = match stack.iter().find(|(name, _)| name == "count") {
        None => 1,
        Some((_, value)) => {
            let n = integer(value).ok_or_else(|| "expected an integer for `count`".to_string())?;
            if !(1..=99).contains(&n) {
                return Err(format!("item count {n} is out of range 1..=99"));
            }
            n as u8
        }
    };
    Ok((item, count))
}

/// Parse the options object of a particle argument.
fn parse_particle_options(name: &str, text: Option<&str>) -> Result<ParticleOptions, String> {
    let Some(text) = text else {
        return Err("Expected particle options".into());
    };
    // Vanilla parses the object with Gson's lenient reader, which accepts
    // bare keys; SNBT is the same shape and the tree already carries a
    // strict, panic-free parser for it.
    let value = snbt::parse(text).map_err(|e| e.to_string())?;
    let Nbt::Compound(obj) = value else {
        return Err("Expected particle options".into());
    };
    Ok(match name {
        "minecraft:block"
        | "minecraft:block_marker"
        | "minecraft:falling_dust"
        | "minecraft:dust_pillar"
        | "minecraft:block_crumble" => {
            let text = as_str(field(&obj, "block_state")?, "block_state")?;
            let state = args::parse_block_state(text).map_err(|e| e.message())?;
            if state.nbt.is_some() {
                return Err("a particle block state cannot carry block-entity data".into());
            }
            ParticleOptions::Block(state.id)
        }
        "minecraft:dust" => ParticleOptions::Dust {
            color: as_color(field(&obj, "color")?)?,
            scale: as_f32(field(&obj, "scale")?, "scale")?,
        },
        "minecraft:dust_color_transition" => ParticleOptions::DustColorTransition {
            from: as_color(field(&obj, "from_color")?)?,
            to: as_color(field(&obj, "to_color")?)?,
            scale: as_f32(field(&obj, "scale")?, "scale")?,
        },
        "minecraft:entity_effect" => ParticleOptions::EntityEffect {
            color: as_color(field(&obj, "color")?)?,
        },
        "minecraft:item" => {
            let (item, count) = parse_item(field(&obj, "item")?)?;
            ParticleOptions::Item { item, count }
        }
        "minecraft:sculk_charge" => ParticleOptions::SculkCharge {
            roll: as_f32(field(&obj, "roll")?, "roll")?,
        },
        "minecraft:shriek" => ParticleOptions::Shriek {
            delay: as_i32(field(&obj, "delay")?, "delay")?,
        },
        "minecraft:vibration" => {
            let destination = field(&obj, "destination")?;
            let Nbt::Compound(destination) = destination else {
                return Err("expected an object for `destination`".into());
            };
            let kind = as_str(field(destination, "type")?, "type")?;
            let kind = kind.strip_prefix("minecraft:").unwrap_or(kind);
            if kind != "block" {
                return Err(format!("unsupported vibration destination type '{kind}'"));
            }
            let (x, y, z) = as_block_pos(field(destination, "pos")?, "pos")?;
            let ticks = as_i32(field(&obj, "arrival_in_ticks")?, "arrival_in_ticks")?;
            ParticleOptions::VibrationBlock { x, y, z, ticks }
        }
        "minecraft:trail" => {
            let (x, y, z) = as_vec3(field(&obj, "target")?, "target")?;
            ParticleOptions::Trail {
                x,
                y,
                z,
                color: as_color(field(&obj, "color")?)?,
                duration: as_i32(field(&obj, "duration")?, "duration")?,
            }
        }
        "minecraft:tinted_leaves" => ParticleOptions::TintedLeaves {
            color: as_color(field(&obj, "color")?)?,
        },
        "minecraft:dragon_breath" => ParticleOptions::DragonBreath {
            power: as_f32(field(&obj, "power")?, "power")?,
        },
        "minecraft:effect" => ParticleOptions::Effect {
            color: as_color(field(&obj, "color")?)?,
            power: as_f32(field(&obj, "power")?, "power")?,
        },
        "minecraft:instant_effect" => ParticleOptions::InstantEffect {
            color: as_color(field(&obj, "color")?)?,
            power: as_f32(field(&obj, "power")?, "power")?,
        },
        "minecraft:flash" => ParticleOptions::Flash {
            color: as_color(field(&obj, "color")?)?,
        },
        _ => ParticleOptions::None,
    })
}

// ---------------------------------------------------------------------------
// /particle
// ---------------------------------------------------------------------------

fn particle_node() -> Node {
    fn force_branch(force: bool) -> Builder {
        let name = if force { "force" } else { "normal" };
        Builder::literal(name)
            .executable(move |ctx| exec_particle(ctx, force))
            .then(
                Builder::arg("viewers", ArgKind::Players)
                    .executable(move |ctx| exec_particle(ctx, force)),
            )
    }

    let count = Builder::arg("count", ArgKind::Integer)
        .bounds(Some(0.0), None)
        .executable(|ctx| exec_particle(ctx, false))
        .then(force_branch(false))
        .then(force_branch(true));
    // Vanilla marks only `name`, `pos`, `count`, the mode words and
    // `viewers` executable: a line may not stop after `delta` or `speed`.
    let speed = Builder::arg("speed", ArgKind::Float)
        .bounds(Some(0.0), None)
        .then(count);
    let delta = Builder::arg("delta", ArgKind::Vec3(false)).then(speed);
    let pos = Builder::arg("pos", ArgKind::Vec3(true))
        .executable(|ctx| exec_particle(ctx, false))
        .then(delta);
    let name = Builder::arg("name", ArgKind::Particle)
        .executable(|ctx| exec_particle(ctx, false))
        .then(pos);

    Builder::literal("particle")
        .requires(gate("minecraft.command.particle"))
        .usage("<name> [<pos>] [<delta>] [<speed>] [<count>] [force|normal] [<viewers>]")
        .executable(|ctx| {
            ctx.fail(
                "/particle <name> [<pos>] [<delta>] [<speed>] [<count>] [force|normal] [<viewers>]",
            )
        })
        .then(name)
        .build()
}

fn exec_particle(ctx: &mut Ctx, force: bool) -> Reply {
    let Some(raw) = text_arg(ctx, 0) else {
        return ctx.fail(
            "/particle <name> [<pos>] [<delta>] [<speed>] [<count>] [force|normal] [<viewers>]",
        );
    };
    // The numeric bounds are enforced while the tree parses the arguments
    // (see `Node::bounds`), before this executor runs.
    let speed = float_arg(ctx, 3).unwrap_or(0.0);
    let count = int_arg(ctx, 4).unwrap_or(0);

    let (id, options_text) = match raw.find('{') {
        Some(index) => (&raw[..index], Some(&raw[index..])),
        None => (raw, None),
    };
    let name = normalize(id.trim());
    if !args::valid_resource(&name) {
        return ctx.fail(format!("Invalid resource location '{name}'"));
    }
    if !resources::particle_exists(&name) {
        return ctx.fail(lang::format(lang::PARTICLE_NOT_FOUND, &[&name]));
    }
    let options = if particle_has_options(&name) {
        match parse_particle_options(&name, options_text) {
            Ok(options) => options,
            Err(detail) => return ctx.fail(invalid_options(detail)),
        }
    } else {
        // Vanilla's `MapCodec.unit` ignores an object on a simple particle
        // (`/particle flame{color:1}` is accepted and the object dropped).
        ParticleOptions::None
    };

    let pos = match coords_arg(ctx, 1) {
        Some(c) => match resolve(ctx, c) {
            Ok(p) => p,
            Err(e) => return ctx.fail(e),
        },
        None => match source_position(ctx) {
            Ok(p) => p,
            Err(e) => return ctx.fail(e),
        },
    };
    let delta = match coords_arg(ctx, 2) {
        Some(c) => match resolve(ctx, c) {
            Ok(p) => p,
            Err(e) => return ctx.fail(e),
        },
        None => Vector3::ZERO,
    };

    let range = if force {
        PARTICLE_RANGE_FORCED
    } else {
        PARTICLE_RANGE
    };
    let in_range = |handle: &Arc<PlayerHandle>| {
        let p = handle.pos();
        let (dx, dy, dz) = (p.x - pos.x, p.y - pos.y, p.z - pos.z);
        dx * dx + dy * dy + dz * dz <= range * range
    };
    let recipients = match text_arg(ctx, 5) {
        Some(viewers) => match resolve_players(ctx.registry, ctx.source, viewers) {
            Ok(players) => players,
            Err(e) => return ctx.fail(e),
        },
        None => ctx.registry.snapshot(),
    };
    // `force` widens the range to 512 blocks; it does not target the whole
    // server, and the same range applies to explicitly selected viewers.
    // Vanilla also skips a target in another level, so the sender's world
    // scopes the set when the command has one.
    let world = ctx.player().map(|handle| handle.world().to_string());
    let recipients: Vec<Arc<PlayerHandle>> = recipients
        .into_iter()
        .filter(|handle| world.as_deref().map_or(true, |w| handle.world() == w))
        .filter(|handle| in_range(handle))
        .collect();
    if recipients.is_empty() {
        return ctx.fail(lang::PARTICLE_FAILED);
    }

    let event = ServerEvent::Particle {
        name: name.clone().into(),
        x: pos.x,
        y: pos.y,
        z: pos.z,
        dx: delta.x as f32,
        dy: delta.y as f32,
        dz: delta.z as f32,
        speed,
        count,
        force,
        options,
    };
    for handle in recipients {
        handle.emit(&event, ctx.world);
    }
    // `ParticleCommand` calls `sendSuccess(..., true)`; the line is shown to
    // the executor as well.
    Reply::one(lang::format(lang::PARTICLE_SUCCESS, &[&name]))
}

// ---------------------------------------------------------------------------
// /playsound
// ---------------------------------------------------------------------------

/// The optional tail after a sound source: targets, position, volume, pitch
/// and minVolume. `source` is captured by the branch that owns the literal.
fn playsound_tail(source: u8) -> Builder {
    let min_volume = Builder::arg("minVolume", ArgKind::Float)
        .bounds(Some(0.0), Some(1.0))
        .executable(move |ctx| exec_playsound(ctx, source));
    let pitch = Builder::arg("pitch", ArgKind::Float)
        .bounds(Some(0.0), Some(2.0))
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(min_volume);
    let volume = Builder::arg("volume", ArgKind::Float)
        .bounds(Some(0.0), None)
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(pitch);
    let pos = Builder::arg("pos", ArgKind::Vec3(true))
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(volume);
    Builder::arg("targets", ArgKind::Players)
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(pos)
}

fn playsound_node() -> Node {
    // The source is a literal in 1.21.11, so `targets` is only reachable once
    // a source has been named; the sound argument itself plays to the caller
    // at `master`.
    let mut sound =
        Builder::arg("sound", ArgKind::ResourceLocation).executable(|ctx| exec_playsound(ctx, 0));
    for (name, source) in [
        ("master", 0),
        ("music", 1),
        ("record", 2),
        ("weather", 3),
        ("block", 4),
        ("hostile", 5),
        ("neutral", 6),
        ("player", 7),
        ("ambient", 8),
        ("voice", 9),
        ("ui", 10),
    ] {
        sound = sound.then(
            Builder::literal(name)
                .executable(move |ctx| exec_playsound(ctx, source))
                .then(playsound_tail(source)),
        );
    }

    Builder::literal("playsound")
        .requires(gate("minecraft.command.playsound"))
        .usage("<sound> [<source>] [<targets>] [<pos>] [<volume>] [<pitch>] [<minVolume>]")
        .executable(|ctx| {
            ctx.fail("/playsound <sound> [<source>] [<targets>] [<pos>] [<volume>] [<pitch>] [<minVolume>]")
        })
        .then(sound)
        .build()
}

fn exec_playsound(ctx: &mut Ctx, source: u8) -> Reply {
    let Some(raw) = text_arg(ctx, 0) else {
        return ctx.fail(
            "/playsound <sound> [<source>] [<targets>] [<pos>] [<volume>] [<pitch>] [<minVolume>]",
        );
    };
    // The bounds are enforced while the tree parses the arguments; these are
    // the values the executor needs.
    let volume = float_arg(ctx, 3).unwrap_or(1.0);
    let pitch = float_arg(ctx, 4).unwrap_or(1.0);
    let min_volume = float_arg(ctx, 5).unwrap_or(0.0);

    let sound = match sound_resource(raw) {
        Ok(sound) => sound,
        Err(e) => return ctx.fail(e),
    };
    let targets = match text_arg(ctx, 1) {
        Some(text) => match resolve_players(ctx.registry, ctx.source, text) {
            Ok(players) => players,
            Err(e) => return ctx.fail(e),
        },
        None => match ctx.player() {
            Some(handle) => vec![ctx.registry.by_uuid(handle.uuid).unwrap_or_else(|| {
                ctx.registry
                    .by_entity(handle.entity_id)
                    .expect("a command source is registered")
            })],
            None => return ctx.fail("No player was found"),
        },
    };
    let pos = match coords_arg(ctx, 2) {
        Some(c) => match resolve(ctx, c) {
            Ok(p) => p,
            Err(e) => return ctx.fail(e),
        },
        None => match source_position(ctx) {
            Ok(p) => p,
            Err(e) => return ctx.fail(e),
        },
    };

    // Vanilla's audible range: 16 blocks at volume 1 or less, scaled by the
    // volume above that. Outside it a sound is skipped unless `minVolume`
    // asks for it, in which case it is replayed two blocks from the target
    // at `minVolume`.
    let range = if volume > 1.0 { volume * 16.0 } else { 16.0 };
    let max_distance_sq = f64::from(range) * f64::from(range);
    let mut heard = Vec::new();
    for handle in targets {
        let p = handle.pos();
        let (dx, dy, dz) = (p.x - pos.x, p.y - pos.y, p.z - pos.z);
        let distance_sq = dx * dx + dy * dy + dz * dz;
        if distance_sq > max_distance_sq {
            if min_volume > 0.0 {
                // `target + normalize(source - target) * 2`: two blocks from
                // the listener, towards the source.
                let distance = distance_sq.sqrt();
                let offset = 2.0 / distance;
                handle.emit(
                    &ServerEvent::Sound {
                        name: sound.clone().into(),
                        category: source,
                        x: p.x + (pos.x - p.x) * offset,
                        y: p.y + (pos.y - p.y) * offset,
                        z: p.z + (pos.z - p.z) * offset,
                        volume: min_volume,
                        pitch,
                    },
                    ctx.world,
                );
                heard.push(handle.name.clone());
            }
            continue;
        }
        handle.emit(
            &ServerEvent::Sound {
                name: sound.clone().into(),
                category: source,
                x: pos.x,
                y: pos.y,
                z: pos.z,
                volume,
                pitch,
            },
            ctx.world,
        );
        heard.push(handle.name.clone());
    }
    if heard.is_empty() {
        return ctx.fail(lang::PLAYSOUND_FAILED);
    }
    // The success count is the players who actually heard it.
    if heard.len() == 1 {
        Reply::one(lang::format(
            lang::PLAYSOUND_SUCCESS_SINGLE,
            &[&sound, &heard[0]],
        ))
    } else {
        Reply::one(lang::format(
            lang::PLAYSOUND_SUCCESS_MULTIPLE,
            &[&sound, &heard.len().to_string()],
        ))
    }
}

// ---------------------------------------------------------------------------
// /stopsound
// ---------------------------------------------------------------------------

fn stopsound_node() -> Node {
    let mut targets = Builder::arg("targets", ArgKind::Players)
        .executable(|ctx| exec_stopsound(ctx, None))
        .then(
            // `*` names no source but still needs a sound; vanilla does not
            // make the literal itself executable.
            Builder::literal("*").then(
                Builder::arg("sound", ArgKind::ResourceLocation)
                    .executable(|ctx| exec_stopsound(ctx, None)),
            ),
        );
    for (name, source) in [
        ("master", 0),
        ("music", 1),
        ("record", 2),
        ("weather", 3),
        ("block", 4),
        ("hostile", 5),
        ("neutral", 6),
        ("player", 7),
        ("ambient", 8),
        ("voice", 9),
        ("ui", 10),
    ] {
        targets = targets.then(
            Builder::literal(name)
                .executable(move |ctx| exec_stopsound(ctx, Some(source)))
                .then(
                    Builder::arg("sound", ArgKind::ResourceLocation)
                        .executable(move |ctx| exec_stopsound(ctx, Some(source))),
                ),
        );
    }

    Builder::literal("stopsound")
        .requires(gate("minecraft.command.stopsound"))
        .usage("<targets> [<source>] [<sound>]")
        .executable(|ctx| ctx.fail("/stopsound <targets> [<source>] [<sound>]"))
        .then(targets)
        .build()
}

fn exec_stopsound(ctx: &mut Ctx, source: Option<u8>) -> Reply {
    let Some(targets_text) = text_arg(ctx, 0) else {
        return ctx.fail("/stopsound <targets> [<source>] [<sound>]");
    };
    let targets = match resolve_players(ctx.registry, ctx.source, targets_text) {
        Ok(players) => players,
        Err(e) => return ctx.fail(e),
    };
    let sound = match text_arg(ctx, 1) {
        Some(raw) => match sound_resource(raw) {
            Ok(sound) => Some(sound),
            Err(e) => return ctx.fail(e),
        },
        None => None,
    };
    let event = ServerEvent::StopSound {
        source,
        sound: sound.clone().map(Into::into),
    };
    for handle in targets {
        handle.emit(&event, ctx.world);
    }
    let line = match (source, sound) {
        (Some(source), Some(sound)) => {
            lang::format(lang::STOPSOUND_SOURCE_SOUND, &[&sound, source_name(source)])
        }
        (Some(source), None) => lang::format(lang::STOPSOUND_SOURCE_ANY, &[source_name(source)]),
        (None, Some(sound)) => lang::format(lang::STOPSOUND_SOUND, &[&sound]),
        (None, None) => lang::STOPSOUND_ANY.into(),
    };
    Reply::one(line)
}
