//! `/particle`, `/playsound` and `/stopsound`: the client-effects commands.
//!
//! These are deliberately thin: the game's state is not changed, only what a
//! client is told to draw or play. The command layer resolves the vanilla
//! argument grammar (including CMD-06b selectors and CMD-06a coordinates) and
//! sends one neutral [`ServerEvent`] per recipient; each codec renders it in
//! its own release's packet shape.
//!
//! Resource names are validated against the 1.21.11 registries. A particle
//! name another release does not have is a no-op for that client (cosmetic,
//! never a malformed packet); a sound id is sent inline, as vanilla does.

use std::sync::Arc;

use aether_api::Vector3;

use super::coordinates::{CoordinateSource, Coordinates, Rotation};
use super::feedback::lang;
use super::selector::{EntityRef, Gamemode, LcgRng, Selector, SelectorSource};
use super::tree::{ArgKind, Builder, Ctx, Node, Requirement, Source, Value};
use super::Reply;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::nbt::Nbt;
use crate::protocol::{resources, ServerEvent};

/// Vanilla's particle distance limit when `force` is not given, in blocks.
const PARTICLE_RANGE: f64 = 32.0;

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
// /particle
// ---------------------------------------------------------------------------

fn particle_node() -> Node {
    fn force_branch(force: bool) -> Builder {
        let name = if force { "force" } else { "normal" };
        Builder::literal(name)
            .executable(move |ctx| exec_particle(ctx, force))
            .then(
                Builder::arg("viewers", ArgKind::Entity)
                    .executable(move |ctx| exec_particle(ctx, force)),
            )
    }

    let count = Builder::arg("count", ArgKind::Integer)
        .executable(|ctx| exec_particle(ctx, false))
        .then(force_branch(false))
        .then(force_branch(true));
    let speed = Builder::arg("speed", ArgKind::Float)
        .executable(|ctx| exec_particle(ctx, false))
        .then(count);
    let delta = Builder::arg("delta", ArgKind::Vec3(false))
        .executable(|ctx| exec_particle(ctx, false))
        .then(speed);
    let pos = Builder::arg("pos", ArgKind::Vec3(false))
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
    let name = normalize(raw);
    if !resources::particle_exists(&name) {
        return ctx.fail(lang::format(lang::PARTICLE_NOT_FOUND, &[&name]));
    }
    let count = int_arg(ctx, 4).unwrap_or(1);
    if count < 0 {
        return ctx.fail(lang::format(
            lang::ARG_INTEGER_LOW,
            &["0", &count.to_string()],
        ));
    }
    let speed = float_arg(ctx, 3).unwrap_or(0.0);
    if speed < 0.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_LOW,
            &["0", &speed.to_string()],
        ));
    }
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

    let recipients = match text_arg(ctx, 5) {
        Some(viewers) => match resolve_players(ctx.registry, ctx.source, viewers) {
            Ok(players) => players,
            Err(e) => return ctx.fail(e),
        },
        None => {
            let all = ctx.registry.snapshot();
            if force {
                all
            } else {
                all.into_iter()
                    .filter(|handle| {
                        let p = handle.pos();
                        let (dx, dy, dz) = (p.x - pos.x, p.y - pos.y, p.z - pos.z);
                        dx * dx + dy * dy + dz * dz <= PARTICLE_RANGE * PARTICLE_RANGE
                    })
                    .collect()
            }
        }
    };
    if recipients.is_empty() {
        return ctx.fail(lang::PARTICLE_FAILED);
    }

    let event = ServerEvent::Particle {
        name: name.into(),
        x: pos.x,
        y: pos.y,
        z: pos.z,
        dx: delta.x as f32,
        dy: delta.y as f32,
        dz: delta.z as f32,
        speed,
        count,
        force,
    };
    for handle in recipients {
        handle.emit(&event, ctx.world);
    }
    // Vanilla's success line is only for command blocks/functions; an
    // operator typing it sees nothing.
    Reply(Vec::new())
}

// ---------------------------------------------------------------------------
// /playsound
// ---------------------------------------------------------------------------

/// The optional tail after a sound source: targets, position, volume, pitch
/// and minVolume. `source` is captured by the branch that owns the literal.
fn playsound_tail(source: u8) -> Builder {
    let min_volume = Builder::arg("minVolume", ArgKind::Float)
        .executable(move |ctx| exec_playsound(ctx, source));
    let pitch = Builder::arg("pitch", ArgKind::Float)
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(min_volume);
    let volume = Builder::arg("volume", ArgKind::Float)
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(pitch);
    let pos = Builder::arg("pos", ArgKind::Vec3(false))
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(volume);
    Builder::arg("targets", ArgKind::Entities)
        .executable(move |ctx| exec_playsound(ctx, source))
        .then(pos)
}

fn playsound_node() -> Node {
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
    // The brief's grammar allows targets without spelling a source. It is
    // added after the source literals so `master` means the category.
    sound = sound.then(playsound_tail(0));

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
    let sound = normalize(raw);
    if !resources::sound_exists(&sound) {
        return ctx.fail(format!(
            "Can't find element '{sound}' in registry 'minecraft:sound_event'"
        ));
    }
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
    let volume = float_arg(ctx, 3).unwrap_or(1.0);
    let pitch = float_arg(ctx, 4).unwrap_or(1.0);
    let min_volume = float_arg(ctx, 5).unwrap_or(0.0);
    if volume < 0.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_LOW,
            &["0", &volume.to_string()],
        ));
    }
    if pitch < 0.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_LOW,
            &["0", &pitch.to_string()],
        ));
    }
    if pitch > 2.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_BIG,
            &["2", &pitch.to_string()],
        ));
    }
    if min_volume < 0.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_LOW,
            &["0", &min_volume.to_string()],
        ));
    }
    if min_volume > 1.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_BIG,
            &["1", &min_volume.to_string()],
        ));
    }
    // Vanilla clamps the minimum volume to the actual volume: a sound cannot
    // be guaranteed louder than it is.
    let _min_volume = min_volume.min(volume);

    let event = ServerEvent::Sound {
        name: sound.clone().into(),
        category: source,
        x: pos.x,
        y: pos.y,
        z: pos.z,
        volume,
        pitch,
    };
    let count = targets.len();
    let single = if count == 1 {
        Some(targets[0].name.clone())
    } else {
        None
    };
    for handle in targets {
        handle.emit(&event, ctx.world);
    }
    match single {
        Some(name) => Reply::one(lang::format(
            lang::PLAYSOUND_SUCCESS_SINGLE,
            &[&sound, &name],
        )),
        None => Reply::one(lang::format(
            lang::PLAYSOUND_SUCCESS_MULTIPLE,
            &[&sound, &count.to_string()],
        )),
    }
}

// ---------------------------------------------------------------------------
// /stopsound
// ---------------------------------------------------------------------------

fn stopsound_node() -> Node {
    let mut targets = Builder::arg("targets", ArgKind::Entities)
        .executable(|ctx| exec_stopsound(ctx, None))
        .then(
            Builder::literal("*")
                .executable(|ctx| exec_stopsound(ctx, None))
                .then(
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
    let sound = text_arg(ctx, 1).map(normalize);
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
