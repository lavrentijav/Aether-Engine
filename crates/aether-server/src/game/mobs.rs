//! Mobs: what each kind is, how it moves and fights, and where it spawns.
//!
//! The behaviour is the shape of vanilla's, not its letter: animals wander
//! and flee when hit, zombies and spiders chase and strike, skeletons keep
//! their distance and shoot, creepers hiss and blow up, and the undead burn
//! in daylight.
//!
//! Movement is A\* over the block grid (`aether_world::pathfind`, a
//! simplified `WalkNodeEvaluator`): a mob follows a cached path to its goal,
//! re-solving when the target moves far enough, the next waypoint is walled
//! off, the path runs out, or the mob stops making progress (a block dropped
//! onto the path). The search is charged against a per-tick node budget shared
//! by every mob, so a crowd cannot stall the tick; a mob that cannot afford a
//! path falls back to straight-line steering, guarded against steps the
//! pathfinder itself would refuse.

use std::cell::Cell;

use aether_api::{Body, Vector3};
use aether_world::pathfind::{self, Nav};
use aether_world::registry::shapes::Box6;

use super::entities::{Action, PlayerView};
use super::tables::Rng;
use crate::inventory::Stack;
use crate::protocol::MetaValue;
use crate::session::DemoWorld;

/// Node expansions every mob shares per tick. Once it is spent, no mob
/// searches again until the next tick and they steer straight.
const NAV_NODE_BUDGET: usize = 4096;
/// Most nodes a single search may expand.
const MAX_PATH_NODES: usize = 512;
/// A fresh path is solved when the goal has moved this far from the cached
/// path's goal, or the path was blocked.
const REPATH_DISTANCE: f64 = 2.0;
/// Ticks a mob waits before it solves another path.
const REPATH_COOLDOWN: u32 = 10;
/// A path counts as reaching the goal within this many horizontal blocks.
const GOAL_RANGE: f64 = 1.2;
/// How far up or down the goal's column a standable landing is sought.
const GOAL_RESOLVE_DY: i32 = 4;
/// Consecutive no-progress ticks before the cached path is thrown away and a
/// fresh one is solved — how a block dropped onto the path is noticed.
const STUCK_TICKS: u32 = 10;

/// The per-tick pathfinding budget shared by every mob.
///
/// `Cell` because the tick is single-threaded and the `Ctx` handed to each
/// mob is shared immutably.
pub struct NavBudget {
    remaining: Cell<usize>,
}

impl NavBudget {
    pub fn new(n: usize) -> Self {
        Self {
            remaining: Cell::new(n),
        }
    }
    /// Nodes still available this tick.
    pub fn remaining(&self) -> usize {
        self.remaining.get()
    }
    /// Charge `n` expansions against the budget.
    pub fn spend(&self, n: usize) {
        self.remaining.set(self.remaining.get().saturating_sub(n));
    }
}

impl Default for NavBudget {
    fn default() -> Self {
        Self::new(NAV_NODE_BUDGET)
    }
}

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
    /// Ticks the current target has been in view (negative: out of view).
    pub see_time: i32,
    /// Status effects running on this mob.
    pub effects: super::effects::Effects,
    /// Cached A\* path, nearest waypoint first, as block cells.
    pub path: Vec<[i32; 3]>,
    /// The waypoint in [`Mob::path`] the mob is walking to.
    pub path_at: usize,
    /// The goal the cached path was solved for.
    pub path_goal: Option<(f64, f64, f64)>,
    /// Ticks before a fresh path may be solved.
    pub repath: u32,
    /// Consecutive ticks the mob wanted to move but barely did, so a block
    /// dropped in its way can force a fresh path.
    pub stuck: u32,
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
            see_time: 0,
            effects: super::effects::Effects::default(),
            path: Vec::new(),
            path_at: 0,
            path_goal: None,
            repath: 0,
            stuck: 0,
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
                crate::protocol::META_AGEABLE,
                MetaValue::Byte((self.wool | if self.sheared { 0x10 } else { 0 }) as i8),
            )),
            _ => {}
        }
        v
    }

    /// What it holds and wears.
    pub fn equipment(&self) -> Vec<(u8, Option<Stack>)> {
        match self.def.kind {
            "minecraft:skeleton" => vec![(0, Some(Stack::named("minecraft:bow", 1)))],
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
    /// The pathfinding budget shared by every mob this tick.
    pub nav: &'a NavBudget,
}

/// Where a mob is heading: a point, and how fast.
struct Goal {
    x: f64,
    y: f64,
    z: f64,
    speed: f64,
}

/// A [`pathfind::Nav`] view of the server world, reading the per-state
/// collision shapes and the names of the few hazards that matter.
struct WorldNav<'a> {
    world: &'a DemoWorld,
}

impl Nav for WorldNav<'_> {
    fn boxes(&self, x: i32, y: i32, z: i32) -> &'static [Box6] {
        aether_world::registry::shapes::collision_boxes(self.world.get_block(x, y, z).raw())
    }
    fn hazard(&self, x: i32, y: i32, z: i32) -> bool {
        is_hazard(self.world.get_block(x, y, z))
    }
}

/// Whether a block state is one a walking mob must not path onto. Read from
/// the block registry's `&'static str` name rather than `block_name`, which
/// allocates a `String` — `hazard` is on the per-node hot path.
fn is_hazard(state: aether_world::BlockStateId) -> bool {
    matches!(
        aether_world::registry::blocks::block_of_state(state).map(|(_, n)| n),
        Some("minecraft:lava")
            | Some("minecraft:fire")
            | Some("minecraft:soul_fire")
            | Some("minecraft:cactus")
            | Some("minecraft:magma_block")
    )
}

/// Whether cell `w` is one a mob can stand in.
fn standable_at(world: &DemoWorld, w: [i32; 3]) -> bool {
    pathfind::standable(&WorldNav { world }, w[0], w[1], w[2])
}

/// Whether the straight-line fallback may step toward `(dx, dz)` from cell
/// `from`: some cell directly ahead — same level, one up, or a drop within
/// [`pathfind::MAX_DROP`] — must be standable. A cached path needs no such
/// check, because the search already refused those drops and hazards.
fn can_advance<N: pathfind::Nav + ?Sized>(nav: &N, from: [i32; 3], dx: f64, dz: f64) -> bool {
    let sx = if dx > 1e-6 {
        1
    } else if dx < -1e-6 {
        -1
    } else {
        0
    };
    let sz = if dz > 1e-6 {
        1
    } else if dz < -1e-6 {
        -1
    } else {
        0
    };
    if sx == 0 && sz == 0 {
        return true;
    }
    if pathfind::step(nav, from, sx, sz).is_some() {
        return true;
    }
    if sx != 0 && sz != 0 {
        // A diagonal may be walled one way but open the other; try each axis.
        return pathfind::step(nav, from, sx, 0).is_some()
            || pathfind::step(nav, from, 0, sz).is_some();
    }
    false
}

/// Solve a path from `pos` to a point, charged against the tick budget.
fn find_path(ctx: &Ctx, pos: Vector3, goal: (f64, f64, f64)) -> Option<Vec<[i32; 3]>> {
    let remaining = ctx.nav.remaining();
    if remaining == 0 {
        return None;
    }
    let nav = WorldNav { world: ctx.world };
    let start = pathfind::resolve_start(
        &nav,
        [
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        ],
    )?;
    let raw = [
        goal.0.floor() as i32,
        goal.1.floor() as i32,
        goal.2.floor() as i32,
    ];
    // The goal is a point, not a node: settle it onto the surface in its own
    // column so a wander pick on a slope, or a jumping player, is reachable.
    let target = pathfind::resolve_goal(&nav, raw, GOAL_RESOLVE_DY).unwrap_or(raw);
    let budget = remaining.min(MAX_PATH_NODES);
    let result = pathfind::search(&nav, start, target, GOAL_RANGE, budget);
    ctx.nav.spend(result.expanded);
    result.reached.then_some(result.path)
}

/// Follow the cached path toward `goal`, solving a fresh one when it is stale,
/// blocked or missing. Returns the waypoint to steer for, or `None` to head
/// straight at `goal`.
fn follow_path(m: &mut Mob, pos: Vector3, goal: (f64, f64, f64), ctx: &Ctx) -> Option<(f64, f64)> {
    m.repath = m.repath.saturating_sub(1);
    // Stale if the goal wandered away, or the path ran out.
    let moved = m.path_goal.map_or(true, |(px, py, pz)| {
        (px - goal.0).powi(2) + (py - goal.1).powi(2) + (pz - goal.2).powi(2)
            > REPATH_DISTANCE * REPATH_DISTANCE
    });
    if moved || m.path_at >= m.path.len() {
        m.path.clear();
        m.path_at = 0;
    }
    // Blocked if the next waypoint is no longer standable.
    if let Some(w) = m.path.get(m.path_at) {
        if !standable_at(ctx.world, *w) {
            m.path.clear();
            m.path_at = 0;
        }
    }
    if m.path.is_empty() && m.repath == 0 {
        match find_path(ctx, pos, goal) {
            Some(path) => {
                m.path = path;
                m.path_at = 0;
                m.path_goal = Some(goal);
                m.repath = REPATH_COOLDOWN;
            }
            None => {
                m.repath = REPATH_COOLDOWN;
                // An animal whose pick is unreachable drops it and chooses
                // again; a hunter keeps straight-line steering as a fallback.
                if m.target.is_none() {
                    m.wander = None;
                }
                return None;
            }
        }
    }
    // Advance past waypoints already reached.
    while m.path_at < m.path.len() {
        let w = m.path[m.path_at];
        let (wx, wz) = (w[0] as f64 + 0.5, w[2] as f64 + 0.5);
        if (wx - pos.x).powi(2) + (wz - pos.z).powi(2) < 0.6 * 0.6 {
            m.path_at += 1;
        } else {
            break;
        }
    }
    if m.path_at >= m.path.len() {
        m.path.clear();
        m.path_at = 0;
        return None;
    }
    let w = m.path[m.path_at];
    Some((w[0] as f64 + 0.5, w[2] as f64 + 0.5))
}

/// Note this tick's intended vs actual movement. A mob that wants to move but
/// barely does — a block placed between it and the next waypoint while the
/// waypoint cell itself stays standable — counts up; after [`STUCK_TICKS`] in
/// a row the cached path is dropped and the cooldown cleared, so the next
/// `follow_path` re-solves around the obstruction. Returns whether the path
/// was thrown away.
fn note_progress(m: &mut Mob, intended: f64, moved: f64) -> bool {
    if intended > 0.01 && moved < intended * 0.4 {
        m.stuck += 1;
    } else {
        m.stuck = 0;
    }
    if m.stuck >= STUCK_TICKS {
        m.stuck = 0;
        if !m.path.is_empty() {
            m.path.clear();
            m.path_at = 0;
            m.repath = 0;
            return true;
        }
    }
    false
}

fn yaw_to(dx: f64, dz: f64) -> f32 {
    (-dx).atan2(dz).to_degrees() as f32
}

fn is_water(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    crate::game::block_name(world, world.get_block(x, y, z))
        .is_some_and(|n| n == "minecraft:water" || n == "minecraft:lava")
}

/// Whether nothing but air is above `(x, y, z)` for a good while.
pub fn sky_above(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    (y..(y + 48).min(384)).all(|yy| world.get_block(x, yy, z) == aether_api::block_ids::AIR)
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
    // Status effects: poison and wither hurt the way a hit does, and can
    // kill; regeneration heals back up to this kind's full health.
    for ev in m.effects.tick() {
        match ev {
            crate::game::effects::TickEvent::Heal(h) => {
                m.health = (m.health + h).min(m.def.health);
                m.meta_dirty = true;
            }
            crate::game::effects::TickEvent::Damage { amount, lethal } => {
                let damage = crate::game::effects::tick_damage(m.health, amount, lethal);
                if damage > 0.0 {
                    m.health -= damage;
                    m.meta_dirty = true;
                    if m.health <= 0.0 {
                        m.health = 0.0;
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
            }
            crate::game::effects::TickEvent::Exhaust(_)
            | crate::game::effects::TickEvent::Removed(_) => {}
        }
    }
    m.hurt_cd = m.hurt_cd.saturating_sub(1);
    m.attack_cd = m.attack_cd.saturating_sub(1);
    // Speed and slowness move the mob at the same pace the movement-speed
    // attribute would.
    let move_speed = m.def.speed * m.effects.movement_multiplier();
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
    let mut goal: Option<Goal> = None;
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
                    // Vanilla's target goals must *see* a player to pick
                    // them; a monster behind a wall does not know you are
                    // there.
                    .filter(|(p, _)| {
                        line_of_sight(
                            world,
                            eye(pos, body),
                            Vector3::new(p.pos.x, p.pos.y + 1.62, p.pos.z),
                        )
                    })
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
                    // Vanilla's RangedBowAttackGoal: close in until within
                    // 15 blocks with a clear view, hold position while the
                    // bow is drawn, and loose only after seeing the target
                    // for a second — then wait out the interval.
                    let from = eye(pos, body);
                    let sees =
                        line_of_sight(world, from, Vector3::new(p.pos.x, p.pos.y + 1.62, p.pos.z));
                    m.see_time = if sees {
                        m.see_time.max(0) + 1
                    } else {
                        m.see_time.min(0) - 1
                    };
                    if dist > 15.0 || !sees {
                        goal = Some(Goal {
                            x: p.pos.x,
                            y: p.pos.y,
                            z: p.pos.z,
                            speed: move_speed,
                        });
                    } else if dist < 4.0 {
                        goal = Some(Goal {
                            x: pos.x - dx,
                            y: pos.y,
                            z: pos.z - dz,
                            speed: move_speed,
                        });
                    }
                    if sees && dist < 15.0 && m.see_time >= 20 && m.attack_cd == 0 {
                        m.attack_cd = SKELETON_INTERVAL;
                        // `AbstractSkeleton.performRangedAttack`: aim at a
                        // third of the target's height, lift by a fifth of
                        // the horizontal distance, then `shoot` at 1.6 with
                        // inaccuracy 14 - 4 × difficulty (normal: 6).
                        let (ax, ay, az) =
                            (p.pos.x - from.x, p.pos.y + 0.6 - from.y, p.pos.z - from.z);
                        let h = (ax * ax + az * az).sqrt();
                        let vel = shoot_vector(ax, ay + h * 0.2, az, 1.6, SKELETON_INACCURACY, rng);
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
                                    name: "minecraft:entity.creeper.primed".into(),
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
                        goal = Some(Goal {
                            x: p.pos.x,
                            y: p.pos.y,
                            z: p.pos.z,
                            speed: move_speed,
                        });
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
                    goal = Some(Goal {
                        x: p.pos.x,
                        y: p.pos.y,
                        z: p.pos.z,
                        speed: move_speed,
                    });
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
                move_speed * 2.0
            } else {
                move_speed
            };
            if (wx - pos.x).powi(2) + (wz - pos.z).powi(2) < 0.5 {
                m.wander = None;
            } else {
                goal = Some(Goal {
                    x: wx,
                    y: pos.y,
                    z: wz,
                    speed,
                });
            }
        }
    }

    // Whether the mob is at a water surface: there it swims, so the walking
    // pathfinder and its drop guard do not apply.
    let height = body_height(body);
    let feet_wet = is_water(world, bx, (pos.y + 0.1).floor() as i32, bz);

    // Steer. When a path is being followed it supplies the waypoint; the
    // straight line to the goal is the fallback when no path was found.
    let mut want = (0.0, 0.0);
    if let Some(g) = &goal {
        let waypoint = follow_path(m, pos, (g.x, g.y, g.z), ctx);
        let (tx, tz) = waypoint.unwrap_or((g.x, g.z));
        let (dx, dz) = (tx - pos.x, tz - pos.z);
        let len = (dx * dx + dz * dz).sqrt();
        if len > 0.3 {
            want = (dx / len * g.speed, dz / len * g.speed);
            if m.target.is_none() {
                *yaw = yaw_to(dx, dz);
                *pitch = 0.0;
            }
            // Only the straight-line fallback needs the drop guard, and it
            // applies to every mob: a hostile whose search failed must not
            // walk off a cliff either. A mob with a path can trust it, since
            // the search used the same drop limit, and a swimming mob is not
            // walking at all.
            if !feet_wet
                && waypoint.is_none()
                && !can_advance(&WorldNav { world }, [bx, by, bz], dx, dz)
            {
                want = (0.0, 0.0);
                if m.target.is_none() {
                    m.wander = None;
                }
            }
        }
    }
    // In water a mob floats with half its body under and swims at half
    // pace, the way vanilla's `travel` (drag 0.8, a quarter of gravity) and
    // `FloatGoal` (rise while more than 0.4 deep) settle it. Holding exactly
    // against gravity at the surface, as this did, stood mobs *on* the water.
    let middle_wet = is_water(world, bx, (pos.y + height * 0.5).floor() as i32, bz);
    let swimming = feet_wet || middle_wet;
    let pace = if swimming { 0.5 } else { 1.0 };
    if m.knock > 0 {
        m.knock -= 1;
    } else {
        body.velocity.x = want.0 * pace;
        body.velocity.z = want.1 * pace;
    }
    if swimming {
        // The physics step then takes 0.08 of gravity off: deep, the net is
        // +0.04 a tick, a gentle rise; shallow, −0.02, a slow settle. The
        // two meet with the waterline at mid-body.
        let lift = if middle_wet { 0.12 } else { 0.06 };
        body.velocity.y = body.velocity.y * 0.8 + lift;
        body.velocity.y = body.velocity.y.clamp(-0.3, 0.12);
    } else if m.jump && body.on_ground {
        body.velocity.y = 0.42;
    }
    m.jump = false;
    // Flowing water carries a swimming mob downstream, on top of its own
    // steering.
    crate::game::fluids::push_body(world, body);
    let before = body.feet();
    world.step_body(body);
    let after = body.feet();
    let moved = ((after.x - before.x).powi(2) + (after.z - before.z).powi(2)).sqrt();
    let intended = (want.0 * want.0 + want.1 * want.1).sqrt();
    if intended > 0.01 && moved < intended * 0.4 {
        m.jump = true;
    }
    note_progress(m, intended, moved);
    false
}

/// Ticks between a skeleton's shots on normal difficulty.
const SKELETON_INTERVAL: u32 = 40;
/// `14 - 4 × difficulty`, normal difficulty.
const SKELETON_INACCURACY: f64 = 6.0;

/// Where a mob looks from: 85 % of its height, vanilla's default eye.
fn eye(feet: Vector3, body: &Body) -> Vector3 {
    Vector3::new(feet.x, feet.y + body_height(body) * 0.85, feet.z)
}

/// Whether nothing solid lies between `a` and `b`, stepping a fifth of a
/// block at a time.
pub fn line_of_sight(world: &DemoWorld, a: Vector3, b: Vector3) -> bool {
    let (dx, dy, dz) = (b.x - a.x, b.y - a.y, b.z - a.z);
    let len = (dx * dx + dy * dy + dz * dz).sqrt();
    let steps = (len / 0.2).ceil().max(1.0) as i32;
    let mut last = (i32::MIN, i32::MIN, i32::MIN);
    for i in 1..steps {
        let t = i as f64 / steps as f64;
        let c = (
            (a.x + dx * t).floor() as i32,
            (a.y + dy * t).floor() as i32,
            (a.z + dz * t).floor() as i32,
        );
        if c != last {
            last = c;
            let props = world.props_of(world.get_block(c.0, c.1, c.2));
            // Glass and leaves block movement but not sight.
            if props.collision && props.light_opacity > 1 {
                return false;
            }
        }
    }
    true
}

/// `Projectile.shoot`: normalise, add Gaussian spread scaled by
/// `0.0075 × inaccuracy` per axis, then scale to `speed`.
fn shoot_vector(x: f64, y: f64, z: f64, speed: f64, inaccuracy: f64, rng: &mut Rng) -> Vector3 {
    let len = (x * x + y * y + z * z).sqrt().max(1e-6);
    let spread = 0.0075 * inaccuracy;
    let (x, y, z) = (
        x / len + rng.gaussian() * spread,
        y / len + rng.gaussian() * spread,
        z / len + rng.gaussian() * spread,
    );
    Vector3::new(x * speed, y * speed, z * speed)
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
    use std::collections::HashSet;

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

    #[test]
    fn hazards_are_recognised_from_the_registry() {
        let h = |n: &str| is_hazard(aether_world::registry::blocks::default_state(n).unwrap());
        for name in [
            "minecraft:lava",
            "minecraft:fire",
            "minecraft:soul_fire",
            "minecraft:cactus",
            "minecraft:magma_block",
        ] {
            assert!(h(name), "{name} is a hazard");
        }
        for name in ["minecraft:stone", "minecraft:water", "minecraft:air"] {
            assert!(!h(name), "{name} is not a hazard");
        }
    }

    fn mob_with_a_path() -> Mob {
        let mut m = Mob::new(&ZOMBIE, &mut Rng::new(1));
        m.path = vec![[1, 64, 0], [2, 64, 0]];
        m.path_at = 0;
        m.path_goal = Some((2.5, 64.0, 0.5));
        m.repath = REPATH_COOLDOWN;
        m
    }

    #[test]
    fn a_wedged_mob_throws_its_path_away() {
        // A block placed between the mob and its waypoint leaves the waypoint
        // cell standable, so only the lack of progress can notice.
        let mut m = mob_with_a_path();
        for _ in 0..STUCK_TICKS {
            note_progress(&mut m, 0.1, 0.0);
        }
        assert!(m.path.is_empty(), "the wedged path is dropped");
        assert_eq!(m.repath, 0, "a fresh path may be solved at once");
    }

    #[test]
    fn movement_resets_the_stuck_counter() {
        let mut m = mob_with_a_path();
        let mut forced = false;
        for _ in 0..(STUCK_TICKS - 1) {
            forced |= note_progress(&mut m, 0.1, 0.0);
        }
        assert!(!forced, "not yet");
        // A single tick of real movement clears the count, so it never
        // false-positives on a mob that is making its way.
        assert!(!note_progress(&mut m, 0.1, 0.09));
        assert_eq!(m.stuck, 0);
        assert!(!m.path.is_empty(), "the path survives a moving mob");
    }

    /// A plain grid of full cubes, for the fallback guard.
    struct BlockGrid(HashSet<[i32; 3]>);

    impl BlockGrid {
        fn new() -> Self {
            Self(HashSet::new())
        }
        fn put(mut self, x: i32, y: i32, z: i32) -> Self {
            self.0.insert([x, y, z]);
            self
        }
    }

    impl pathfind::Nav for BlockGrid {
        fn boxes(&self, x: i32, y: i32, z: i32) -> &'static [Box6] {
            const FULL: [Box6; 1] = [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
            const NONE: [Box6; 0] = [];
            if self.0.contains(&[x, y, z]) {
                &FULL
            } else {
                &NONE
            }
        }
        fn hazard(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }
    }

    #[test]
    fn the_fallback_guard_follows_the_pathfinder_drop_limit() {
        // Floor at y = 0 up to x = 0, then a shelf two lower.
        let shallow = BlockGrid::new().put(0, 0, 0).put(1, -2, 0);
        assert!(
            can_advance(&shallow, [0, 1, 0], 1.0, 0.0),
            "a two-block drop is within MAX_DROP and must be walked"
        );
        // The same, four lower: past MAX_DROP, so the fallback refuses.
        let deep = BlockGrid::new().put(0, 0, 0).put(1, -4, 0);
        assert!(
            !can_advance(&deep, [0, 1, 0], 1.0, 0.0),
            "a four-block drop must be refused"
        );
    }
}
