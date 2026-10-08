//! Mobs: what each kind is, how it moves and fights, and where it spawns.
//!
//! The behaviour is the shape of vanilla's, not its letter: animals wander
//! and flee when hit, zombies and spiders chase and strike, skeletons keep
//! their distance and shoot, creepers hiss and blow up, and the undead burn
//! in daylight. Pathfinding is a straight line with a jump over one-block
//! steps and a refusal to walk off a drop — enough to be a real threat on
//! open ground, and an honest gap underground (see `KNOWN_ISSUES`).

use aether_api::{Body, Vector3};

use super::entities::{Action, PlayerView};
use super::tables::Rng;
use crate::inventory::Stack;
use crate::protocol::MetaValue;
use crate::session::DemoWorld;

/// One kind of mob.
#[derive(Debug)]
pub struct MobDef {
    pub kind: &'static str,
    pub health: f32,
    /// Walking speed, blocks per tick.
    pub speed: f64,
    /// Melee damage, `0` for none.
    pub damage: f32,
    pub hostile: bool,
    /// Burns in daylight.
    pub undead: bool,
    /// Experience for killing it.
    pub xp: i32,
}

pub static COW: MobDef = MobDef {
    kind: "minecraft:cow",
    health: 10.0,
    speed: 0.07,
    damage: 0.0,
    hostile: false,
    undead: false,
    xp: 2,
};
pub static PIG: MobDef = MobDef {
    kind: "minecraft:pig",
    health: 10.0,
    speed: 0.07,
    damage: 0.0,
    hostile: false,
    undead: false,
    xp: 2,
};
pub static SHEEP: MobDef = MobDef {
    kind: "minecraft:sheep",
    health: 8.0,
    speed: 0.07,
    damage: 0.0,
    hostile: false,
    undead: false,
    xp: 2,
};
pub static CHICKEN: MobDef = MobDef {
    kind: "minecraft:chicken",
    health: 4.0,
    speed: 0.06,
    damage: 0.0,
    hostile: false,
    undead: false,
    xp: 2,
};
pub static ZOMBIE: MobDef = MobDef {
    kind: "minecraft:zombie",
    health: 20.0,
    speed: 0.115,
    damage: 3.0,
    hostile: true,
    undead: true,
    xp: 5,
};
pub static SKELETON: MobDef = MobDef {
    kind: "minecraft:skeleton",
    health: 20.0,
    speed: 0.12,
    damage: 0.0,
    hostile: true,
    undead: true,
    xp: 5,
};
pub static CREEPER: MobDef = MobDef {
    kind: "minecraft:creeper",
    health: 20.0,
    speed: 0.11,
    damage: 0.0,
    hostile: true,
    undead: false,
    xp: 5,
};
pub static SPIDER: MobDef = MobDef {
    kind: "minecraft:spider",
    health: 16.0,
    speed: 0.14,
    damage: 2.0,
    hostile: true,
    undead: false,
    xp: 5,
};

/// Every mob this server knows.
pub static ALL: [&MobDef; 8] = [
    &COW, &PIG, &SHEEP, &CHICKEN, &ZOMBIE, &SKELETON, &CREEPER, &SPIDER,
];

/// The mob kind called `name` (with or without `minecraft:`).
pub fn by_name(name: &str) -> Option<&'static MobDef> {
    let full = if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    };
    ALL.iter().copied().find(|d| d.kind == full)
}

/// The sixteen dye colours, in wool-metadata order.
const WOOL: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// The wool colour name for a sheep colour index.
pub fn wool_name(i: u8) -> &'static str {
    WOOL[(i & 15) as usize]
}

/// One live mob.
#[derive(Debug, Clone)]
pub struct Mob {
    pub def: &'static MobDef,
    pub health: f32,
    pub hurt_cd: u32,
    pub attack_cd: u32,
    /// The player entity it is after.
    pub target: Option<i32>,
    pub wander: Option<(f64, f64)>,
    pub idle: u32,
    pub panic: u32,
    /// Creeper fuse, `0..=30`.
    pub fuse: i32,
    /// Ticks since death, while the death animation plays.
    pub death: Option<u32>,
    /// Ticks of knockback left, during which it does not steer.
    pub knock: u32,
    pub burning: u32,
    /// Blocked last tick: jump this one.
    pub jump: bool,
    pub wool: u8,
    pub sheared: bool,
    pub meta_dirty: bool,
}

impl Mob {
    pub fn new(def: &'static MobDef, rng: &mut Rng) -> Self {
        // Natural sheep colours: mostly white, some greys, black and brown.
        let roll = rng.f32();
        let wool = if roll < 0.8164 {
            0
        } else if roll < 0.8664 {
            7
        } else if roll < 0.9164 {
            8
        } else if roll < 0.9664 {
            15
        } else if roll < 0.9964 {
            12
        } else {
            6
        };
        Self {
            def,
            health: def.health,
            hurt_cd: 0,
            attack_cd: 20,
            target: None,
            wander: None,
            idle: rng.range(20, 120) as u32,
            panic: 0,
            fuse: 0,
            death: None,
            knock: 0,
            burning: 0,
            jump: false,
            wool,
            sheared: false,
            meta_dirty: false,
        }
    }

    /// Entity metadata describing this mob's visible state.
    pub fn metadata(&self) -> Vec<(u8, MetaValue)> {
        let flags: i8 = if self.burning > 0 { 0x01 } else { 0 };
        let mut v = vec![
            (0, MetaValue::Byte(flags)),
            (9, MetaValue::Float(self.health.max(0.0))),
        ];
        if self.death.is_some() {
            v.push((6, MetaValue::Pose(7)));
        }
        match self.def.kind {
            "minecraft:creeper" => {
                v.push((16, MetaValue::VarInt(if self.fuse > 0 { 1 } else { -1 })))
            }
            "minecraft:sheep" => v.push((
                17,
                MetaValue::Byte((self.wool | if self.sheared { 0x10 } else { 0 }) as i8),
            )),
            _ => {}
        }
        v
    }

    /// What it holds and wears.
    pub fn equipment(&self) -> Vec<(u8, Option<Stack>)> {
        match self.def.kind {
            "minecraft:skeleton" => vec![(0, Some(Stack::new("minecraft:bow", 1)))],
            _ => Vec::new(),
        }
    }
}

/// What the mob tick may look at.
pub struct Ctx<'a> {
    pub world: &'a DemoWorld,
    pub players: &'a [PlayerView],
    pub now: u64,
    pub day: bool,
}

fn yaw_to(dx: f64, dz: f64) -> f32 {
    (-dx).atan2(dz).to_degrees() as f32
}

fn solid(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    world.props_of(world.get_block(x, y, z)).collision
}

fn is_water(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    crate::game::block_name(world, world.get_block(x, y, z))
        .is_some_and(|n| n == "minecraft:water" || n == "minecraft:lava")
}

/// Whether nothing but air is above `(x, y, z)` for a good while.
pub fn sky_above(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    (y..(y + 48).min(384)).all(|yy| world.get_block(x, yy, z) == aether_api::block_ids::AIR)
}

/// Whether there is ground within three blocks below `(x, y, z)`.
fn ground_below(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    (1..=3).any(|d| solid(world, x, y - d, z) || is_water(world, x, y - d, z))
}

/// Step one mob. Returns `true` when it should be removed.
#[allow(clippy::too_many_arguments)]
pub fn tick(
    id: i32,
    m: &mut Mob,
    body: &mut Body,
    yaw: &mut f32,
    pitch: &mut f32,
    ctx: &Ctx,
    rng: &mut Rng,
    actions: &mut Vec<Action>,
) -> bool {
    let world = ctx.world;
    let pos = body.feet();
    if let Some(t) = &mut m.death {
        *t += 1;
        world.step_body(body);
        return *t >= 20;
    }
    m.hurt_cd = m.hurt_cd.saturating_sub(1);
    m.attack_cd = m.attack_cd.saturating_sub(1);
    let (bx, by, bz) = (
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );

    // Despawn hostiles nobody is near.
    let nearest = ctx
        .players
        .iter()
        .map(|p| ((p.pos.x - pos.x).powi(2) + (p.pos.z - pos.z).powi(2)).sqrt())
        .fold(f64::INFINITY, f64::min);
    if m.def.hostile && (nearest > 128.0 || (nearest > 32.0 && rng.chance(1.0 / 800.0))) {
        return true;
    }

    // Daylight burns the undead under open sky.
    if m.def.undead && ctx.day && ctx.now % 20 == (id as u64 % 20) {
        let burning = !is_water(world, bx, by, bz) && sky_above(world, bx, by + 2, bz);
        if burning != (m.burning > 0) {
            m.meta_dirty = true;
        }
        m.burning = if burning { 40 } else { 0 };
    } else if !ctx.day && m.burning > 0 {
        m.burning = 0;
        m.meta_dirty = true;
    }
    if m.burning > 0 && ctx.now % 20 == 0 {
        m.health -= 1.0;
        m.meta_dirty = true;
        actions.push(Action::Tracked(
            id,
            crate::protocol::ServerEvent::Damage {
                entity_id: id,
                source: "minecraft:on_fire",
                attacker: None,
            },
        ));
        if m.health <= 0.0 {
            m.death = Some(0);
            actions.push(Action::Tracked(
                id,
                crate::protocol::ServerEvent::EntityStatus {
                    entity_id: id,
                    status: 3,
                },
            ));
        }
    }

    // Choose where to go.
    let mut goal: Option<(f64, f64, f64)> = None; // x, z, speed
    if m.def.hostile {
        let valid = m
            .target
            .and_then(|t| ctx.players.iter().find(|p| p.eid == t && p.huntable));
        let target = match valid {
            Some(p) if (p.pos.x - pos.x).abs() < 32.0 && (p.pos.z - pos.z).abs() < 32.0 => Some(p),
            _ => {
                // Spiders are only hostile in the dark.
                let range = if m.def.kind == "minecraft:spider" && ctx.day {
                    0.0
                } else {
                    16.0
                };
                ctx.players
                    .iter()
                    .filter(|p| p.huntable)
                    .map(|p| {
                        (
                            p,
                            (p.pos.x - pos.x).powi(2)
                                + (p.pos.y - pos.y).powi(2)
                                + (p.pos.z - pos.z).powi(2),
                        )
                    })
                    .filter(|(_, d)| *d < range * range)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(p, _)| p)
            }
        };
        m.target = target.map(|p| p.eid);
        if let Some(p) = target {
            let (dx, dy, dz) = (p.pos.x - pos.x, p.pos.y - pos.y, p.pos.z - pos.z);
            let dist_h = (dx * dx + dz * dz).sqrt();
            let dist = (dist_h * dist_h + dy * dy).sqrt();
            *yaw = yaw_to(dx, dz);
            *pitch = (-(dy + 1.0)).atan2(dist_h).to_degrees() as f32;
            match m.def.kind {
                "minecraft:skeleton" => {
                    if dist > 10.0 {
                        goal = Some((p.pos.x, p.pos.z, m.def.speed));
                    } else if dist < 4.0 {
                        goal = Some((pos.x - dx, pos.z - dz, m.def.speed));
                    }
                    if dist < 15.0 && m.attack_cd == 0 {
                        m.attack_cd = 40 + rng.range(0, 20) as u32;
                        let from = Vector3::new(pos.x, pos.y + 1.5, pos.z);
                        let ty = p.pos.y + 1.1 - from.y;
                        let speed = 1.6;
                        let h = dist_h.max(0.1);
                        // Lead the shot against gravity (0.05/tick²): t = h / speed.
                        let vy = ty / h * speed + 0.025 * h / speed;
                        let vel = Vector3::new(dx / h * speed, vy.clamp(-2.0, 2.0), dz / h * speed);
                        actions.push(Action::Shoot {
                            from,
                            vel,
                            shooter: id,
                        });
                    }
                }
                "minecraft:creeper" => {
                    if dist < 3.0 {
                        if m.fuse == 0 {
                            m.meta_dirty = true;
                            actions.push(Action::Tracked(
                                id,
                                crate::protocol::ServerEvent::Sound {
                                    name: "minecraft:entity.creeper.primed",
                                    category: 5,
                                    x: pos.x,
                                    y: pos.y,
                                    z: pos.z,
                                    volume: 1.0,
                                    pitch: 0.5,
                                },
                            ));
                        }
                        m.fuse += 1;
                    } else {
                        if m.fuse > 0 {
                            m.fuse -= 1;
                            if m.fuse == 0 {
                                m.meta_dirty = true;
                            }
                        }
                        goal = Some((p.pos.x, p.pos.z, m.def.speed));
                    }
                    if m.fuse >= 30 {
                        actions.push(Action::Explode {
                            pos: Vector3::new(pos.x, pos.y + 0.8, pos.z),
                            radius: 3.0,
                        });
                        return true;
                    }
                }
                _ => {
                    goal = Some((p.pos.x, p.pos.z, m.def.speed));
                    let reach = 1.2 + body_half_width(body);
                    if dist_h < reach && dy.abs() < 2.0 && m.attack_cd == 0 {
                        m.attack_cd = 20;
                        actions.push(Action::Attack {
                            mob: id,
                            target: p.eid,
                            damage: m.def.damage,
                            from: pos,
                        });
                        actions.push(Action::Tracked(
                            id,
                            crate::protocol::ServerEvent::EntityAnimation {
                                entity_id: id,
                                animation: 0,
                            },
                        ));
                    }
                }
            }
        }
    }
    if goal.is_none() {
        if m.panic > 0 {
            m.panic -= 1;
            if m.wander.is_none() {
                let a = rng.f64() * std::f64::consts::TAU;
                m.wander = Some((pos.x + a.cos() * 8.0, pos.z + a.sin() * 8.0));
            }
        } else if m.wander.is_none() {
            m.idle = m.idle.saturating_sub(1);
            if m.idle == 0 {
                m.idle = rng.range(80, 240) as u32;
                if rng.chance(0.6) {
                    let a = rng.f64() * std::f64::consts::TAU;
                    let r = 2.0 + rng.f64() * 6.0;
                    m.wander = Some((pos.x + a.cos() * r, pos.z + a.sin() * r));
                }
            }
        }
        if let Some((wx, wz)) = m.wander {
            let speed = if m.panic > 0 {
                m.def.speed * 2.0
            } else {
                m.def.speed
            };
            if (wx - pos.x).powi(2) + (wz - pos.z).powi(2) < 0.5 {
                m.wander = None;
            } else {
                goal = Some((wx, wz, speed));
            }
        }
    }

    // Steer.
    let mut want = (0.0, 0.0);
    if let Some((gx, gz, speed)) = goal {
        let (dx, dz) = (gx - pos.x, gz - pos.z);
        let len = (dx * dx + dz * dz).sqrt();
        if len > 0.3 {
            want = (dx / len * speed, dz / len * speed);
            if m.target.is_none() {
                *yaw = yaw_to(dx, dz);
                *pitch = 0.0;
            }
            // Do not walk off a drop the mob cannot climb back up.
            let ahead_x = (pos.x + dx / len * 0.8).floor() as i32;
            let ahead_z = (pos.z + dz / len * 0.8).floor() as i32;
            if !solid(world, ahead_x, by, ahead_z)
                && !ground_below(world, ahead_x, by + 1, ahead_z)
                && m.target.is_none()
            {
                want = (0.0, 0.0);
                m.wander = None;
            }
        }
    }
    let swimming =
        is_water(world, bx, by, bz) || is_water(world, bx, (pos.y + 0.6).floor() as i32, bz);
    if m.knock > 0 {
        m.knock -= 1;
    } else {
        body.velocity.x = want.0;
        body.velocity.z = want.1;
    }
    if swimming {
        // Buoyancy that beats the physics step's 0.08 of gravity: rise while
        // the head is under, bob at the surface once it is out.
        let head_under = is_water(
            world,
            bx,
            (pos.y + body_height(body) * 0.8).floor() as i32,
            bz,
        );
        body.velocity.y = if head_under { 0.12 } else { 0.08 };
    } else if m.jump && body.on_ground {
        body.velocity.y = 0.42;
    }
    m.jump = false;
    let before = body.feet();
    world.step_body(body);
    let after = body.feet();
    let moved = ((after.x - before.x).powi(2) + (after.z - before.z).powi(2)).sqrt();
    let intended = (want.0 * want.0 + want.1 * want.1).sqrt();
    if intended > 0.01 && moved < intended * 0.4 {
        m.jump = true;
    }
    false
}

fn body_height(b: &Body) -> f64 {
    b.aabb.max.y - b.aabb.min.y
}

fn body_half_width(b: &Body) -> f64 {
    (b.aabb.max.x - b.aabb.min.x) / 2.0
}

/// Try to spawn mobs around each player.
///
/// Called every couple of seconds. Animals appear on grass in daylight-ish
/// places up to a small cap per player; monsters in the dark — at night
/// under open sky, or any time underground — up to a larger one.
pub fn spawn_round(
    world: &DemoWorld,
    players: &[PlayerView],
    night: bool,
    rng: &mut Rng,
) -> Vec<(&'static MobDef, Vector3)> {
    let mut out = Vec::new();
    if super::entities::mob_count() > 300 {
        return out;
    }
    for p in players {
        if !p.alive {
            continue;
        }
        let (n, hostile) = super::entities::mobs_near(p.pos, 64.0);
        let passive = n - hostile;
        let attempts = if night { 8 } else { 4 };
        for _ in 0..attempts {
            let a = rng.f64() * std::f64::consts::TAU;
            let r = 24.0 + rng.f64() * 30.0;
            let x = (p.pos.x + a.cos() * r).floor() as i32;
            let z = (p.pos.z + a.sin() * r).floor() as i32;
            let Some(y) = spawn_height(world, x, p.pos.y.floor() as i32, z) else {
                continue;
            };
            let too_close = players.iter().any(|q| {
                (q.pos.x - x as f64).powi(2)
                    + (q.pos.y - y as f64).powi(2)
                    + (q.pos.z - z as f64).powi(2)
                    < 24.0 * 24.0
            });
            if too_close {
                continue;
            }
            let ground =
                crate::game::block_name(world, world.get_block(x, y - 1, z)).unwrap_or_default();
            let open_sky = sky_above(world, x, y + 2, z);
            let dark = !open_sky || night;
            let at = Vector3::new(x as f64 + 0.5, y as f64, z as f64 + 0.5);
            if dark && hostile < 12 && ground != "minecraft:water" && rng.chance(0.7) {
                let def = match rng.range(0, 99) {
                    0..=39 => &ZOMBIE,
                    40..=64 => &SKELETON,
                    65..=84 => &CREEPER,
                    _ => &SPIDER,
                };
                out.push((def, at));
            } else if open_sky
                && !night
                && passive < 8
                && ground == "minecraft:grass_block"
                && rng.chance(0.3)
            {
                let def = match rng.range(0, 3) {
                    0 => &COW,
                    1 => &PIG,
                    2 => &SHEEP,
                    _ => &CHICKEN,
                };
                // Animals come in small groups.
                for i in 0..rng.range(2, 4) {
                    out.push((
                        def,
                        Vector3::new(at.x + (i % 2) as f64, at.y, at.z + (i / 2) as f64),
                    ));
                }
            }
        }
    }
    out
}

/// The Y of the first standable spot in column `(x, z)` within reach of
/// `near_y`: solid ground with two air blocks above it.
fn spawn_height(world: &DemoWorld, x: i32, near_y: i32, z: i32) -> Option<i32> {
    let top = (near_y + 24).min(380);
    let bottom = (near_y - 32).max(-63);
    let mut air = 0;
    for y in (bottom..=top).rev() {
        let b = world.get_block(x, y, z);
        let props = world.props_of(b);
        if props.collision {
            if air >= 2 && props.solid {
                return Some(y + 1);
            }
            air = 0;
        } else if b == aether_api::block_ids::AIR {
            air += 1;
        } else {
            air = 0;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mob_is_a_real_entity_type() {
        for d in ALL {
            assert!(
                super::super::tables::entity_type(d.kind).is_some(),
                "{}",
                d.kind
            );
        }
        assert_eq!(by_name("zombie").map(|d| d.kind), Some("minecraft:zombie"));
    }

    #[test]
    fn yaw_faces_the_target() {
        // Vanilla: yaw 0 faces +Z, 90 faces -X.
        assert!((yaw_to(0.0, 1.0) - 0.0).abs() < 1e-3);
        assert!((yaw_to(-1.0, 0.0) - 90.0).abs() < 1e-3);
    }
}
