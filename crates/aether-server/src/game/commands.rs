//! Gameplay commands: game mode, time, give, summon, kill, teleport.
//!
//! Everything that changes the world or hands out items is operator-only;
//! `/kill` and `/spawn` affect only whoever typed them.

use aether_api::Vector3;

use super::{entities, interact, mobs};
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::{GameMode, ServerEvent};
use crate::session::DemoWorld;

/// Handle `cmd` if it is a gameplay command. `None` if it is not one.
pub fn dispatch(
    cmd: &str,
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Option<Vec<String>> {
    let op = crate::is_operator(&handle.name);
    let deny = || Some(vec!["You are not an operator.".to_string()]);
    Some(match cmd {
        "gamemode" | "gm" => {
            if !op {
                return deny();
            }
            let Some(mode) = args.first().and_then(|a| parse_mode(a)) else {
                return Some(vec!["/gamemode <survival|creative> [player]".into()]);
            };
            let target = match args.get(1) {
                Some(name) => match registry
                    .snapshot()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
                {
                    Some(p) => p,
                    None => return Some(vec![format!("{name} is not online")]),
                },
                None => match registry.by_entity(handle.entity_id) {
                    Some(p) => p,
                    None => return Some(Vec::new()),
                },
            };
            interact::set_mode(&target, world, mode);
            vec![format!("{} is now in {:?} mode", target.name, mode)]
        }
        "time" => {
            if !op {
                return deny();
            }
            let t = match (args.first().copied(), args.get(1).copied()) {
                (Some("set"), Some(v)) | (Some(v), None) => match v {
                    "day" => 1000,
                    "noon" => 6000,
                    "night" => 13000,
                    "midnight" => 18000,
                    n => match n.parse::<i64>() {
                        Ok(n) => n,
                        Err(_) => {
                            return Some(vec!["/time set <day|noon|night|midnight|ticks>".into()])
                        }
                    },
                },
                _ => {
                    return Some(vec![format!(
                        "It is {} (day {})",
                        super::time_of_day(),
                        super::now() / 24000
                    )])
                }
            };
            super::set_time_of_day(t);
            registry.broadcast(&super::time_event(), world);
            vec![format!("Time set to {t}")]
        }
        "give" => {
            if !op {
                return deny();
            }
            let Some(raw) = args.first() else {
                return Some(vec!["/give <item> [count]".into()]);
            };
            let item = if raw.contains(':') {
                raw.to_string()
            } else {
                format!("minecraft:{raw}")
            };
            if !super::tables::is_item(&item) {
                return Some(vec![format!("No such item: {item}")]);
            }
            let count: u64 = args
                .get(1)
                .and_then(|c| c.parse().ok())
                .unwrap_or(1)
                .min(64 * 36);
            crate::ground::give_or_drop(handle, registry, world, &item, count);
            vec![format!("Gave {count} {item}")]
        }
        "summon" => {
            if !op {
                return deny();
            }
            let Some(def) = args.first().and_then(|n| mobs::by_name(n)) else {
                let names: Vec<&str> = mobs::ALL
                    .iter()
                    .map(|d| d.kind.trim_start_matches("minecraft:"))
                    .collect();
                return Some(vec![format!("/summon <{}>", names.join("|"))]);
            };
            let p = handle.pos();
            let (yaw, pitch) = (p.yaw.to_radians() as f64, 0.0f64);
            let at = Vector3::new(
                p.x - yaw.sin() * 3.0 * pitch.cos(),
                p.y,
                p.z + yaw.cos() * 3.0,
            );
            entities::spawn_mob(def, at, p.yaw + 180.0);
            vec![format!("Summoned {}", def.kind)]
        }
        "killall" => {
            if !op {
                return deny();
            }
            vec![format!("Removed {} mobs", entities::kill_all_mobs())]
        }
        "kill" => {
            {
                let mut st = handle.game();
                if st.dead {
                    return Some(Vec::new());
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
        "spawn" => {
            let s = handle.game().spawn;
            teleport(handle, registry, world, s.0, s.1, s.2);
            vec!["Teleported to spawn".into()]
        }
        "tp" => {
            if !op {
                return deny();
            }
            match args {
                [x, y, z] => match (x.parse::<f64>(), y.parse::<f64>(), z.parse::<f64>()) {
                    (Ok(x), Ok(y), Ok(z)) => {
                        teleport(handle, registry, world, x, y, z);
                        vec![format!("Teleported to {x} {y} {z}")]
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
                        vec![format!("Teleported to {}", p.name)]
                    }
                    None => vec![format!("{name} is not online")],
                },
                _ => vec!["/tp <x> <y> <z> | /tp <player>".into()],
            }
        }
        "block" => {
            let parsed: Vec<i32> = args.iter().filter_map(|a| a.parse().ok()).collect();
            let [x, y, z] = parsed[..] else {
                return Some(vec!["/block <x> <y> <z>".into()]);
            };
            let b = world.get_block(x, y, z);
            vec![format!(
                "{x} {y} {z}: {}",
                world.block_name_of(b).unwrap_or_default()
            )]
        }
        "heal" => {
            if !op {
                return deny();
            }
            let mut st = handle.game();
            st.health = 20.0;
            st.food = 20;
            st.saturation = 5.0;
            vec!["Healed".into()]
        }
        _ => return None,
    })
}

fn parse_mode(s: &str) -> Option<GameMode> {
    match s {
        "s" | "0" => Some(GameMode::Survival),
        "c" | "1" => Some(GameMode::Creative),
        other => other.parse().ok(),
    }
}

/// Move a player, telling their client and everyone watching.
pub fn teleport(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: f64,
    y: f64,
    z: f64,
) {
    let mut pos = handle.pos();
    pos.x = x;
    pos.y = y;
    pos.z = z;
    handle.set_pos(pos);
    handle.game().fall_peak = None;
    handle.emit(
        &ServerEvent::Teleport {
            x,
            y,
            z,
            yaw: pos.yaw,
            pitch: pos.pitch,
        },
        world,
    );
    registry.broadcast_except(handle.entity_id, &ServerEvent::EntityMove(handle), world);
}
