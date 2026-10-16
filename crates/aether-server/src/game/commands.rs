//! Gameplay commands: game mode, time, give, summon, kill, teleport.
//!
//! Everything that changes the world or hands out items is operator-only;
//! `/kill` and `/spawn` affect only whoever typed them. The op gate is the
//! tree's ([`nodes`]), not a check inside the bodies: the parser refuses an
//! unprivileged caller before a body is ever entered, and the refusal reads
//! exactly as it always has.

use aether_api::Vector3;

use super::{entities, interact, mobs};
use crate::commands::feedback::lang;
use crate::commands::tree::{self, Ctx, Deny, Node, Requirement, Source};
use crate::commands::Reply;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::{GameMode, ServerEvent};
use crate::session::DemoWorld;

/// The op gate for the gameplay verbs, with the historical message.
pub fn require_op(source: &Source) -> Result<(), Deny> {
    if source.is_operator() {
        Ok(())
    } else {
        Err(Deny::Message("You are not an operator.".into()))
    }
}

/// Register every gameplay command on the tree.
pub fn nodes() -> Vec<Node> {
    vec![
        tree::with_usage(
            tree::greedy(
                "gamemode",
                &["gm"],
                Requirement::Fn(require_op),
                exec_gamemode,
            ),
            "<gamemode> [<targets>]",
        ),
        tree::with_usage(
            tree::greedy("time", &[], Requirement::Fn(require_op), exec_time),
            "set <day|noon|night|midnight|ticks>",
        ),
        tree::with_usage(
            tree::greedy("give", &[], Requirement::Fn(require_op), exec_give),
            "<item> [<count>]",
        ),
        tree::with_usage(
            tree::greedy("summon", &[], Requirement::Fn(require_op), exec_summon),
            "<mob> [x y z]",
        ),
        tree::simple("killall", &[], Requirement::Fn(require_op), exec_killall),
        tree::simple("kill", &[], Requirement::None, exec_kill),
        tree::simple("spawn", &[], Requirement::None, exec_spawn),
        tree::with_usage(
            tree::greedy("msg", &["tell", "w"], Requirement::None, exec_msg),
            "<player> <message>",
        ),
        tree::with_usage(
            tree::greedy("teleport", &["tp"], Requirement::Fn(require_op), exec_tp),
            "<x> <y> <z> | <player>",
        ),
        tree::with_usage(
            tree::greedy("block", &[], Requirement::None, exec_block),
            "<x> <y> <z>",
        ),
        tree::with_usage(
            tree::greedy("food", &[], Requirement::Fn(require_op), exec_food),
            "[level]",
        ),
        tree::simple("heal", &[], Requirement::Fn(require_op), exec_heal),
        tree::with_usage(
            tree::greedy(
                "visibility",
                &[],
                Requirement::Fn(require_op),
                exec_visibility,
            ),
            "[on|off|status]",
        ),
    ]
}

fn exec_gamemode(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(gamemode(&args, handle, ctx.registry, ctx.world)),
        None => ctx.needs_player(),
    }
}

fn exec_time(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    Reply(time(&args, ctx.registry, ctx.world))
}

fn exec_give(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(give(&args, handle, ctx.registry, ctx.world)),
        None => ctx.needs_player(),
    }
}

fn exec_summon(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(summon(&args, handle)),
        None => ctx.needs_player(),
    }
}

fn exec_killall(_ctx: &mut Ctx) -> Reply {
    Reply(killall())
}

fn exec_kill(ctx: &mut Ctx) -> Reply {
    match ctx.player() {
        Some(handle) => Reply(kill(handle, ctx.registry, ctx.world)),
        None => ctx.needs_player(),
    }
}

fn exec_spawn(ctx: &mut Ctx) -> Reply {
    match ctx.player() {
        Some(handle) => Reply(spawn(handle, ctx.registry, ctx.world)),
        None => ctx.needs_player(),
    }
}

fn exec_msg(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(msg(&args, handle, ctx.registry, ctx.world)),
        None => ctx.needs_player(),
    }
}

fn exec_tp(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(tp(&args, handle, ctx.registry, ctx.world)),
        None => ctx.needs_player(),
    }
}

fn exec_block(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    Reply(block(&args, ctx.world))
}

fn exec_food(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(food(&args, handle)),
        None => ctx.needs_player(),
    }
}

fn exec_heal(ctx: &mut Ctx) -> Reply {
    match ctx.player() {
        Some(handle) => Reply(heal(handle)),
        None => ctx.needs_player(),
    }
}

fn exec_visibility(ctx: &mut Ctx) -> Reply {
    let args = ctx.refs();
    match ctx.player() {
        Some(handle) => Reply(visibility(&args, handle, ctx.world)),
        None => ctx.needs_player(),
    }
}

/// `/gamemode <survival|creative> [player]`.
pub fn gamemode(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Vec<String> {
    let Some(mode) = args.first().and_then(|a| parse_mode(a)) else {
        return vec!["/gamemode <survival|creative> [player]".into()];
    };
    let target = match args.get(1) {
        Some(name) => match registry
            .snapshot()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            Some(p) => p,
            None => return vec![format!("{name} is not online")],
        },
        None => match registry.by_entity(handle.entity_id) {
            Some(p) => p,
            None => return Vec::new(),
        },
    };
    interact::set_mode(&target, world, mode);
    // Vanilla words it differently for the caller than for someone else; the
    // old reply did not, and this is the one place to fix that.
    if target.entity_id == handle.entity_id {
        vec![lang::format(
            lang::GAMEMODE_SUCCESS_SELF,
            &[game_mode_name(mode)],
        )]
    } else {
        vec![lang::format(
            lang::GAMEMODE_SUCCESS_OTHER,
            &[&target.name, game_mode_name(mode)],
        )]
    }
}

/// The vanilla `gameMode.*` display name.
fn game_mode_name(mode: GameMode) -> &'static str {
    match mode {
        GameMode::Survival => lang::GAMEMODE_SURVIVAL,
        GameMode::Creative => lang::GAMEMODE_CREATIVE,
        GameMode::Spectator => lang::GAMEMODE_SPECTATOR,
    }
}

/// `/time [set] <day|noon|night|midnight|ticks>` or a query.
pub fn time(args: &[&str], registry: &SharedRegistry, world: &DemoWorld) -> Vec<String> {
    let t = match (args.first().copied(), args.get(1).copied()) {
        (Some("set"), Some(v)) | (Some(v), None) => match v {
            "day" => 1000,
            "noon" => 6000,
            "night" => 13000,
            "midnight" => 18000,
            n => match n.parse::<i64>() {
                Ok(n) => n,
                Err(_) => return vec!["/time set <day|noon|night|midnight|ticks>".into()],
            },
        },
        _ => {
            // The bare `/time` is a query, the closest vanilla analogue being
            // `/time query daytime`; use its string (`commands.time.query`)
            // rather than an invented one. CMD-13a ports the full
            // `add/set/query` grammar onto the tree.
            return vec![lang::format(
                lang::TIME_QUERY,
                &[&super::time_of_day().to_string()],
            )];
        }
    };
    super::set_time_of_day(t);
    registry.broadcast(&super::time_event(), world);
    vec![lang::format(lang::TIME_SET, &[&t.to_string()])]
}

/// `/give <item> [count]`.
pub fn give(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Vec<String> {
    let Some(raw) = args.first() else {
        return vec!["/give <item> [count]".into()];
    };
    let item = if raw.contains(':') {
        raw.to_string()
    } else {
        format!("minecraft:{raw}")
    };
    let Some(item_id) = world.item_id(&item) else {
        return vec![format!("No such item: {item}")];
    };
    let count: u64 = args
        .get(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(1)
        .min(64 * 36);
    crate::ground::give_or_drop(handle, registry, world, item_id, count);
    vec![lang::format(
        lang::GIVE_SUCCESS_SINGLE,
        &[&count.to_string(), &item, &handle.name],
    )]
}

/// `/summon <mob> [x y z]`.
pub fn summon(args: &[&str], handle: &PlayerHandle) -> Vec<String> {
    let Some(def) = args.first().and_then(|n| mobs::by_name(n)) else {
        let names: Vec<&str> = mobs::ALL
            .iter()
            .map(|d| d.kind.trim_start_matches("minecraft:"))
            .collect();
        return vec![format!("/summon <{}> [x y z]", names.join("|"))];
    };
    let p = handle.pos();
    let coords: Vec<f64> = args[1..].iter().filter_map(|a| a.parse().ok()).collect();
    let at = match coords[..] {
        [x, y, z] => Vector3::new(x, y, z),
        _ => {
            let yaw = p.yaw.to_radians() as f64;
            Vector3::new(p.x - yaw.sin() * 3.0, p.y, p.z + yaw.cos() * 3.0)
        }
    };
    entities::spawn_mob(def, at, p.yaw + 180.0);
    vec![lang::format(lang::SUMMON_SUCCESS, &[def.kind])]
}

/// `/killall`.
pub fn killall() -> Vec<String> {
    vec![format!("Removed {} mobs", entities::kill_all_mobs())]
}

/// `/kill`.
pub fn kill(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld) -> Vec<String> {
    {
        let mut st = handle.game();
        if st.dead {
            return Vec::new();
        }
        st.health = 0.0;
        st.dead = true;
    }
    handle.emit(
        &ServerEvent::Health {
            health: 0.0,
            food: handle.game().food,
            saturation: 0.0,
        },
        world,
    );
    super::combat::die(handle, "minecraft:generic_kill", None, registry, world);
    Vec::new()
}

/// `/spawn`.
pub fn spawn(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld) -> Vec<String> {
    let s = handle.game().spawn;
    teleport(handle, registry, world, s.0, s.1, s.2);
    vec!["Teleported to spawn".into()]
}

/// `/msg <player> <message>` and its `/tell` `/w` aliases: a private message
/// to exactly one player.
///
/// The sender and the recipient each see their own side of it, and the line is
/// logged and journalled (CH1). A target that is not a player is refused, as
/// in vanilla; targeting yourself is allowed.
pub fn msg(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Vec<String> {
    let Some(name) = args.first().copied() else {
        return vec!["/msg <player> <message>".into()];
    };
    let message = args[1..].join(" ");
    if message.is_empty() {
        return vec!["/msg <player> <message>".into()];
    }
    match registry
        .snapshot()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
    {
        Some(target) => {
            crate::chatlog::log_private(handle, &target, &message, world);
            target.emit(
                &ServerEvent::Chat(lang::format(
                    lang::MESSAGE_INCOMING,
                    &[&handle.name, &message],
                )),
                world,
            );
            vec![lang::format(
                lang::MESSAGE_OUTGOING,
                &[&target.name, &message],
            )]
        }
        None => vec![format!("{name} is not online")],
    }
}

/// `/tp <x> <y> <z>` or `/tp <player>`.
pub fn tp(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Vec<String> {
    match args {
        [x, y, z] => match (x.parse::<f64>(), y.parse::<f64>(), z.parse::<f64>()) {
            (Ok(x), Ok(y), Ok(z)) => {
                teleport(handle, registry, world, x, y, z);
                vec![lang::format(
                    lang::TELEPORT_SUCCESS_LOCATION_SINGLE,
                    &[&handle.name, &x.to_string(), &y.to_string(), &z.to_string()],
                )]
            }
            _ => vec!["/tp <x> <y> <z> | /tp <player>".into()],
        },
        [name] => match registry
            .snapshot()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            Some(p) => {
                let to = p.pos();
                teleport(handle, registry, world, to.x, to.y, to.z);
                vec![lang::format(
                    lang::TELEPORT_SUCCESS_ENTITY_SINGLE,
                    &[&handle.name, &p.name],
                )]
            }
            None => vec![format!("{name} is not online")],
        },
        _ => vec!["/tp <x> <y> <z> | /tp <player>".into()],
    }
}

/// `/block <x> <y> <z>`.
pub fn block(args: &[&str], world: &DemoWorld) -> Vec<String> {
    let parsed: Vec<i32> = args.iter().filter_map(|a| a.parse().ok()).collect();
    let [x, y, z] = parsed[..] else {
        return vec!["/block <x> <y> <z>".into()];
    };
    let b = world.get_block(x, y, z);
    vec![format!(
        "{x} {y} {z}: {}",
        world.block_name_of(b).unwrap_or_default()
    )]
}

/// `/food [level]`.
pub fn food(args: &[&str], handle: &PlayerHandle) -> Vec<String> {
    let n: i32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(20);
    let mut st = handle.game();
    st.food = n.clamp(0, 20);
    st.saturation = 0.0;
    vec![format!("Food set to {}", st.food)]
}

/// `/heal`.
pub fn heal(handle: &PlayerHandle) -> Vec<String> {
    let mut st = handle.game();
    st.health = 20.0;
    st.food = 20;
    st.saturation = 5.0;
    vec!["Healed".into()]
}

/// `/visibility [on|off]`.
pub fn visibility(args: &[&str], handle: &PlayerHandle, world: &DemoWorld) -> Vec<String> {
    let action = args.first().copied();
    // `/visibility off` is the explicit per-player bypass toggle (an alias kept
    // alongside `/spectate xray off`); `/visibility status` reports the
    // effective state for this player. Operators are **not** bypassed
    // implicitly — see `crate::anti_xray_bypass`. `reveal` is set only by an
    // explicit turn-off, so `/visibility status` never re-sends anything.
    let (reveal, status) = {
        let mut st = handle.visibility();
        match action {
            Some("on") => {
                st.bypass = false;
                // A bypassed player is skipped on the tick, so force the next
                // search when culling comes back.
                st.mark_dirty();
                (false, false)
            }
            Some("off") => {
                st.bypass = true;
                (true, false)
            }
            // The new subcommand: the live, effective state for this player.
            Some("status") => (false, true),
            // The bare readback keeps its old wording.
            None => (false, false),
            Some(other) => return vec![format!("/visibility on|off|status (not `{other}`)")],
        }
    };
    // Turning culling off must reveal the columns already sent as stone, not
    // wait for them to re-stream. Done with the visibility lock released:
    // re-encoding a column takes it again.
    if reveal {
        crate::visibility::reveal_all(handle, world);
    }
    if status {
        return vec![crate::visibility::player_status(handle)];
    }
    // The plain toggle replies stay as they were: `on`/`off` change the
    // per-player bypass flag, which the server-level enable sits apart from.
    vec![match action {
        Some("on") => "Visibility culling on".to_string(),
        Some("off") => "Visibility culling off: you are sent every section".to_string(),
        // Bare `/visibility`.
        _ => format!(
            "Visibility culling is {}",
            if handle.visibility().bypass {
                "off"
            } else {
                "on"
            }
        ),
    }]
}

fn parse_mode(s: &str) -> Option<GameMode> {
    match s {
        "s" | "0" => Some(GameMode::Survival),
        "c" | "1" => Some(GameMode::Creative),
        other => other.parse().ok(),
    }
}

/// Move a player, telling their client and everyone watching.
///
/// The move itself is the core teleport service
/// ([`crate::teleport::teleport`]); this keeps the old command-facing shape
/// (three coordinates, current look preserved) so `/tp`, `/spawn` and staff
/// spectate stay byte-for-byte what they were.
pub fn teleport(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: f64,
    y: f64,
    z: f64,
) {
    let pos = handle.pos();
    let _ = crate::teleport::teleport(
        handle,
        registry,
        world,
        crate::teleport::TeleportTarget::Exact(Vector3::new(x, y, z), pos.yaw, pos.pitch),
        crate::teleport::TeleportCause::Command,
    );
}
