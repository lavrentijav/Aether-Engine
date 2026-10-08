//! Every entity that is not a player: items on the ground, mobs, arrows.
//!
//! One list, stepped twenty times a second by [`super::tick`]. Clients learn
//! about an entity through the **tracker**: each player has the set of
//! entity ids their client has spawned, and every tick that set is brought in
//! line with what is within range — spawning what came into view, destroying
//! what left it. Movement and status updates go only to the players whose
//! set holds the entity, so a mob a kilometre away costs nobody bandwidth,
//! and a column that unloads and reloads gets its entities back.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use aether_api::{Body, Vector3};

use super::mobs::{self, Mob};
use super::tables::Rng;
use crate::inventory::Stack;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::{MetaValue, ServerEvent};
use crate::session::DemoWorld;

/// How far, in blocks, a client is told about an entity.
pub const TRACK_RANGE: f64 = 72.0;
/// Ticks an item lies on the ground before it disappears (vanilla's 5 min).
pub const ITEM_LIFETIME: u32 = 6000;
/// `ItemEntity`'s own physics: gravity 0.04, drag 0.98, and ground
/// friction of a block's 0.6 slipperiness × 0.98. The client simulates a
/// thrown item with exactly these, so the server has to as well.
const ITEM_PHYSICS: aether_api::PhysicsParams = aether_api::PhysicsParams {
    gravity: 0.04,
    vertical_drag: 0.98,
    ground_friction: 0.588,
    air_friction: 0.98,
};

/// Ticks between position syncs for items and arrows, which the client
/// simulates itself from their velocity (vanilla's tracking interval for
/// items is 20).
const PROJECTILE_SYNC: u64 = 20;

/// Ticks an arrow stuck in a block lasts.
const ARROW_LIFETIME: u32 = 1200;

/// A stack on the ground.
#[derive(Debug, Clone)]
pub struct ItemEnt {
    pub stack: Stack,
    /// Only this player may pick it up — the remainder of a transaction,
    /// not loot. `None` for ordinary drops.
    pub owner: Option<u128>,
    pub pickup_delay: u32,
}

/// An arrow in flight or stuck in a block.
#[derive(Debug, Clone)]
pub struct Arrow {
    pub shooter: Option<i32>,
    pub from_player: bool,
    pub damage: f32,
    pub stuck: bool,
    pub stuck_ticks: u32,
}

/// What an entity is.
#[derive(Debug, Clone)]
pub enum Kind {
    Item(ItemEnt),
    Mob(Mob),
    Arrow(Arrow),
}

/// One entity.
#[derive(Debug, Clone)]
pub struct Entity {
    pub id: i32,
    pub uuid: u128,
    pub body: Body,
    pub yaw: f32,
    pub pitch: f32,
    pub age: u32,
    pub kind: Kind,
    /// Position and yaw as last sent to clients.
    sent: (f64, f64, f64, f32),
    /// Removed at the end of this tick.
    pub dead: bool,
    /// At rest as of the last tick, so coming to rest is noticed once.
    rested: bool,
}

impl Entity {
    pub fn pos(&self) -> Vector3 {
        self.body.feet()
    }

    /// The protocol entity type.
    pub fn type_name(&self) -> &'static str {
        match &self.kind {
            Kind::Item(_) => "minecraft:item",
            Kind::Mob(m) => m.def.kind,
            Kind::Arrow(_) => "minecraft:arrow",
        }
    }

    /// The packets that make this entity appear for a client.
    pub fn spawn_events(&self) -> Vec<ServerEvent<'static>> {
        let p = self.pos();
        let v = self.body.velocity;
        let data = match &self.kind {
            Kind::Arrow(a) => a.shooter.map(|s| s + 1).unwrap_or(0),
            _ => 0,
        };
        let mut out = vec![ServerEvent::SpawnEntity {
            entity_id: self.id,
            uuid: self.uuid,
            kind: self.type_name(),
            x: p.x,
            y: p.y,
            z: p.z,
            yaw: self.yaw,
            pitch: self.pitch,
            velocity: (v.x, v.y, v.z),
            data,
        }];
        match &self.kind {
            Kind::Item(it) => out.push(ServerEvent::EntityMeta {
                entity_id: self.id,
                entries: vec![(8, MetaValue::Item(Some(it.stack.clone())))],
            }),
            Kind::Mob(m) => {
                out.push(ServerEvent::EntityMeta {
                    entity_id: self.id,
                    entries: m.metadata(),
                });
                let eq = m.equipment();
                if !eq.is_empty() {
                    out.push(ServerEvent::Equipment {
                        entity_id: self.id,
                        slots: eq,
                    });
                }
            }
            Kind::Arrow(_) => {}
        }
        out
    }
}

fn entities() -> &'static Mutex<Vec<Entity>> {
    static E: OnceLock<Mutex<Vec<Entity>>> = OnceLock::new();
    E.get_or_init(|| Mutex::new(Vec::new()))
}

/// Entity ids for everything that is not a player.
///
/// Counted up from a long way above the player range, which counts up from
/// one, so the two can never meet.
fn next_id() -> i32 {
    static NEXT: AtomicI32 = AtomicI32::new(1_000_000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn uuid_for(id: i32) -> u128 {
    0xae7e_0000_0000_4000_8000_0000_0000_0000u128 | (id as u32 as u128)
}

fn rng() -> &'static Mutex<Rng> {
    static R: OnceLock<Mutex<Rng>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Rng::from_time()))
}

/// Roll something random, from the shared RNG.
pub fn with_rng<R>(f: impl FnOnce(&mut Rng) -> R) -> R {
    f(&mut rng().lock().unwrap())
}

fn push(body: Body, yaw: f32, kind: Kind) -> i32 {
    let id = next_id();
    let p = body.feet();
    entities().lock().unwrap().push(Entity {
        id,
        uuid: uuid_for(id),
        body,
        yaw,
        pitch: 0.0,
        age: 0,
        kind,
        sent: (p.x, p.y, p.z, yaw),
        dead: false,
        rested: false,
    });
    id
}

/// Put a stack on the ground at `pos`, moving at `vel` blocks/tick.
pub fn spawn_item(
    pos: Vector3,
    stack: Stack,
    vel: Vector3,
    pickup_delay: u32,
    owner: Option<u128>,
) -> i32 {
    let mut body = Body::sized(pos, 0.25, 0.25);
    body.velocity = vel;
    push(
        body,
        0.0,
        Kind::Item(ItemEnt {
            stack,
            owner,
            pickup_delay,
        }),
    )
}

/// Throw a stack from a player's hand, the way pressing Q does.
pub fn throw_from(handle: &PlayerHandle, stack: Stack) {
    let p = handle.pos();
    let (yaw, pitch) = (p.yaw.to_radians() as f64, p.pitch.to_radians() as f64);
    let speed = 0.3;
    let vel = Vector3::new(
        -yaw.sin() * pitch.cos() * speed,
        -pitch.sin() * speed + 0.1,
        yaw.cos() * pitch.cos() * speed,
    );
    spawn_item(Vector3::new(p.x, p.y + 1.32, p.z), stack, vel, 40, None);
}

/// Scatter what a broken block dropped around its centre.
pub fn drop_at_block(x: i32, y: i32, z: i32, items: Vec<Stack>) {
    for s in items {
        let (dx, dz, vx, vz) = with_rng(|r| {
            (
                r.f64() * 0.5 - 0.25,
                r.f64() * 0.5 - 0.25,
                r.f64() * 0.2 - 0.1,
                r.f64() * 0.2 - 0.1,
            )
        });
        spawn_item(
            Vector3::new(x as f64 + 0.5 + dx, y as f64 + 0.25, z as f64 + 0.5 + dz),
            s,
            Vector3::new(vx, 0.2, vz),
            10,
            None,
        );
    }
}

/// Spawn a mob of `def` at `pos`.
pub fn spawn_mob(def: &'static mobs::MobDef, pos: Vector3, yaw: f32) -> i32 {
    let (_, w, h) = super::tables::entity_type(def.kind).unwrap_or((0, 0.6, 1.8));
    let body = Body::sized(pos, w as f64, h as f64);
    let mob = with_rng(|r| Mob::new(def, r));
    push(body, yaw, Kind::Mob(mob))
}

/// Fire an arrow.
pub fn spawn_arrow(
    pos: Vector3,
    vel: Vector3,
    shooter: Option<i32>,
    from_player: bool,
    damage: f32,
) -> i32 {
    let mut body = Body::sized(pos, 0.5, 0.5);
    body.velocity = vel;
    let yaw = (-vel.x).atan2(vel.z).to_degrees() as f32;
    push(
        body,
        yaw,
        Kind::Arrow(Arrow {
            shooter,
            from_player,
            damage,
            stuck: false,
            stuck_ticks: 0,
        }),
    )
}

/// How many mobs there are, and how many of them hostile, near `pos`.
pub fn mobs_near(pos: Vector3, radius: f64) -> (usize, usize) {
    let all = entities().lock().unwrap();
    let mut n = 0;
    let mut hostile = 0;
    for e in all.iter() {
        if let Kind::Mob(m) = &e.kind {
            let p = e.pos();
            if (p.x - pos.x).abs() < radius && (p.z - pos.z).abs() < radius {
                n += 1;
                if m.def.hostile {
                    hostile += 1;
                }
            }
        }
    }
    (n, hostile)
}

/// Total mob count, for the global cap.
pub fn mob_count() -> usize {
    entities()
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, Kind::Mob(_)))
        .count()
}

/// Items on the ground owned by `owner`, for `/inv`: `(item, count, pos,
/// seconds left)`.
pub fn owned_items(owner: u128) -> Vec<(String, u8, Vector3, u32)> {
    entities()
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Item(it) if it.owner == Some(owner) => Some((
                it.stack.item.clone(),
                it.stack.count,
                e.pos(),
                ITEM_LIFETIME.saturating_sub(e.age) / 20,
            )),
            _ => None,
        })
        .collect()
}

/// What hurting a mob did.
pub struct MobHit {
    pub killed: bool,
    pub kind: &'static str,
    pub xp: i32,
    pub pos: Vector3,
}

/// Hurt the mob `id` by `amount`, knocked back away from `from`.
///
/// `None` if there is no such mob, it is already dying, or it was hit less
/// than half a second ago.
pub fn hurt_mob(
    id: i32,
    amount: f32,
    from: Option<Vector3>,
    attacker: Option<i32>,
) -> Option<MobHit> {
    let mut all = entities().lock().unwrap();
    let e = all.iter_mut().find(|e| e.id == id && !e.dead)?;
    let pos = e.pos();
    let Kind::Mob(m) = &mut e.kind else {
        return None;
    };
    if m.death.is_some() || m.hurt_cd > 0 {
        return None;
    }
    m.health -= amount;
    m.hurt_cd = 10;
    m.meta_dirty = true;
    if m.def.hostile {
        if let Some(a) = attacker {
            if a < 1_000_000 {
                m.target = Some(a);
            }
        }
    } else {
        m.panic = 60;
        m.wander = None;
    }
    if let Some(f) = from {
        let (dx, dz) = (pos.x - f.x, pos.z - f.z);
        let len = (dx * dx + dz * dz).sqrt().max(1e-4);
        e.body.velocity.x = dx / len * 0.4;
        e.body.velocity.z = dz / len * 0.4;
        if e.body.on_ground {
            e.body.velocity.y = 0.36;
        }
        m.knock = 8;
    }
    let killed = m.health <= 0.0;
    if killed {
        m.death = Some(0);
    }
    Some(MobHit {
        killed,
        kind: m.def.kind,
        xp: m.def.xp,
        pos,
    })
}

/// Use (right-click) an entity with `item`. Returns what the player gets in
/// exchange for what they held, if anything happened.
pub fn interact(id: i32, item: Option<&str>) -> Option<Interaction> {
    let mut all = entities().lock().unwrap();
    let e = all.iter_mut().find(|e| e.id == id && !e.dead)?;
    let pos = e.pos();
    let Kind::Mob(m) = &mut e.kind else {
        return None;
    };
    match (m.def.kind, item) {
        ("minecraft:cow", Some("minecraft:bucket")) => {
            Some(Interaction::Replace("minecraft:milk_bucket"))
        }
        ("minecraft:sheep", Some("minecraft:shears")) if !m.sheared => {
            m.sheared = true;
            m.meta_dirty = true;
            let n = with_rng(|r| r.range(1, 3)) as u8;
            let wool = format!("minecraft:{}_wool", mobs::wool_name(m.wool));
            drop(all);
            drop_at_block(
                pos.x.floor() as i32,
                pos.y.floor() as i32 + 1,
                pos.z.floor() as i32,
                vec![Stack::new(&wool, n)],
            );
            Some(Interaction::Wear)
        }
        _ => None,
    }
}

/// The outcome of using an entity.
pub enum Interaction {
    /// Swap the held item for this one (a bucket of milk).
    Replace(&'static str),
    /// The held tool takes a point of wear.
    Wear,
}

/// The entity type of `id`, if it exists.
pub fn kind_of(id: i32) -> Option<&'static str> {
    entities()
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.type_name())
}

/// Living mobs within `radius` of `pos`, with their positions.
pub fn mobs_in(pos: Vector3, radius: f64) -> Vec<(i32, Vector3)> {
    entities()
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.death.is_none()))
        .map(|e| (e.id, e.pos()))
        .filter(|(_, p)| {
            (p.x - pos.x).powi(2) + (p.y - pos.y).powi(2) + (p.z - pos.z).powi(2) < radius * radius
        })
        .collect()
}

/// The position of entity `id`, if it exists.
pub fn position(id: i32) -> Option<Vector3> {
    entities()
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.pos())
}

/// Remove every hostile mob (peaceful command, tests).
pub fn kill_all_mobs() -> usize {
    let mut all = entities().lock().unwrap();
    let mut n = 0;
    for e in all.iter_mut() {
        if matches!(e.kind, Kind::Mob(_)) {
            e.dead = true;
            n += 1;
        }
    }
    n
}

/// Something a tick decided that has to happen outside the entity lock.
pub enum Action {
    /// A mob hits a player.
    Attack {
        mob: i32,
        target: i32,
        damage: f32,
        from: Vector3,
    },
    /// A projectile hits a player.
    Shot {
        shooter: Option<i32>,
        target: i32,
        damage: f32,
        from: Vector3,
    },
    /// A projectile hits a mob.
    HitMob {
        shooter: Option<i32>,
        target: i32,
        damage: f32,
        from: Vector3,
    },
    /// A creeper blows up.
    Explode { pos: Vector3, radius: f32 },
    /// A skeleton looses an arrow.
    Shoot {
        from: Vector3,
        vel: Vector3,
        shooter: i32,
    },
    /// A player picked up `count` from item entity `item`.
    Picked {
        item: i32,
        collector: i32,
        count: u8,
        gone: bool,
    },
    /// An event for everyone tracking an entity.
    Tracked(i32, ServerEvent<'static>),
}

/// A player as the entity tick sees one.
#[derive(Debug, Clone)]
pub struct PlayerView {
    pub eid: i32,
    pub uuid: u128,
    pub pos: Vector3,
    /// Survival and alive: something a mob may hunt.
    pub huntable: bool,
    pub alive: bool,
}

/// Step every entity one tick.
pub fn tick(world: &DemoWorld, registry: &SharedRegistry, now: u64, day: bool) -> Vec<Action> {
    let players: Vec<std::sync::Arc<PlayerHandle>> = registry.snapshot();
    let views: Vec<PlayerView> = players
        .iter()
        .map(|p| {
            let pos = p.pos();
            let st = p.game();
            PlayerView {
                eid: p.entity_id,
                uuid: p.uuid,
                pos: Vector3::new(pos.x, pos.y, pos.z),
                huntable: st.survival() && !st.dead,
                alive: !st.dead,
            }
        })
        .collect();

    let mut actions = Vec::new();
    let mut gone: Vec<i32> = Vec::new();
    {
        let mut all = entities().lock().unwrap();
        // Boxes of the living mobs, for arrows to hit.
        let mob_boxes: Vec<(i32, aether_api::Aabb)> = all
            .iter()
            .filter_map(|e| match &e.kind {
                Kind::Mob(m) if m.death.is_none() => Some((e.id, e.body.aabb)),
                _ => None,
            })
            .collect();
        let mut rng = rng().lock().unwrap();
        for e in all.iter_mut() {
            e.age += 1;
            let id = e.id;
            let pos = e.pos();
            match &mut e.kind {
                Kind::Item(it) => {
                    if e.age >= ITEM_LIFETIME {
                        e.dead = true;
                        continue;
                    }
                    // Items bob at the waterline, as vanilla's do: lifted
                    // while their centre is under, left to settle once it is
                    // not — never held above the surface.
                    if in_water(world, Vector3::new(pos.x, pos.y + 0.05, pos.z)) {
                        let v = &mut e.body.velocity;
                        v.x *= 0.95;
                        v.z *= 0.95;
                        v.y = (v.y * 0.9 + 0.09).min(0.06);
                    }
                    world.step_body_with(&mut e.body, ITEM_PHYSICS);
                    if it.pickup_delay > 0 {
                        it.pickup_delay -= 1;
                    }
                }
                Kind::Arrow(a) => {
                    tick_arrow(
                        world,
                        e.id,
                        &mut e.body,
                        &mut e.yaw,
                        &mut e.pitch,
                        a,
                        &views,
                        &mob_boxes,
                        &mut actions,
                    );
                    if a.stuck {
                        a.stuck_ticks += 1;
                        if a.stuck_ticks > ARROW_LIFETIME {
                            e.dead = true;
                        }
                    }
                    if e.body.feet().y < -128.0 {
                        e.dead = true;
                    }
                }
                Kind::Mob(m) => {
                    let ctx = mobs::Ctx {
                        world,
                        players: &views,
                        now,
                        day,
                    };
                    if mobs::tick(
                        id,
                        m,
                        &mut e.body,
                        &mut e.yaw,
                        &mut e.pitch,
                        &ctx,
                        &mut rng,
                        &mut actions,
                    ) {
                        e.dead = true;
                    }
                    if m.meta_dirty {
                        m.meta_dirty = false;
                        actions.push(Action::Tracked(
                            id,
                            ServerEvent::EntityMeta {
                                entity_id: id,
                                entries: m.metadata(),
                            },
                        ));
                    }
                }
            }
            if e.body.feet().y < -128.0 {
                e.dead = true;
            }
            // Position updates. Mobs every other tick, as their steering
            // changes direction at will. Items and arrows follow ballistic
            // paths the client simulates from their velocity, so they get a
            // sync — position *and* velocity — only every second, or the
            // moment they stop; positions alone every other tick made a
            // thrown item stutter through interpolation steps that fought
            // the client's own simulation.
            if !e.dead {
                let p = e.pos();
                let (sx, sy, sz, syaw) = e.sent;
                let moved = (p.x - sx).abs() + (p.y - sy).abs() + (p.z - sz).abs();
                let is_mob = matches!(e.kind, Kind::Mob(_));
                let v = e.body.velocity;
                let resting = v.x.abs() + v.y.abs() + v.z.abs() < 1e-3;
                let due = if is_mob {
                    now % 2 == 0 && (moved > 0.01 || (e.yaw - syaw).abs() > 2.0)
                } else {
                    moved > 0.01 && (e.age as u64 % PROJECTILE_SYNC == 0 || (resting && !e.rested))
                };
                e.rested = resting;
                if due {
                    e.sent = (p.x, p.y, p.z, e.yaw);
                    actions.push(Action::Tracked(
                        id,
                        ServerEvent::EntityPos {
                            entity_id: id,
                            x: p.x,
                            y: p.y,
                            z: p.z,
                            yaw: e.yaw,
                            pitch: e.pitch,
                            on_ground: e.body.on_ground,
                        },
                    ));
                    if is_mob {
                        actions.push(Action::Tracked(
                            id,
                            ServerEvent::EntityHead {
                                entity_id: id,
                                yaw: e.yaw,
                            },
                        ));
                    } else {
                        actions.push(Action::Tracked(
                            id,
                            ServerEvent::EntityVelocity {
                                entity_id: id,
                                velocity: (v.x, v.y, v.z),
                            },
                        ));
                    }
                }
            }
        }

        // Pickups.
        for e in all.iter_mut() {
            if e.dead {
                continue;
            }
            let id = e.id;
            let ib = e.body.aabb;
            match &mut e.kind {
                Kind::Item(it) if it.pickup_delay == 0 => {
                    for (view, handle) in views.iter().zip(players.iter()) {
                        if !view.alive || it.owner.is_some_and(|o| o != view.uuid) {
                            continue;
                        }
                        if !near_player(view.pos, ib, 1.0, 0.5) {
                            continue;
                        }
                        let before = it.stack.count;
                        let rest = handle.inventory().insert(it.stack.clone());
                        let left = rest.map(|r| r.count).unwrap_or(0);
                        if left == before {
                            continue;
                        }
                        it.stack.count = left;
                        let gone_now = left == 0;
                        actions.push(Action::Picked {
                            item: id,
                            collector: view.eid,
                            count: before - left,
                            gone: gone_now,
                        });
                        if gone_now {
                            e.dead = true;
                            break;
                        }
                        actions.push(Action::Tracked(
                            id,
                            ServerEvent::EntityMeta {
                                entity_id: id,
                                entries: vec![(8, MetaValue::Item(Some(it.stack.clone())))],
                            },
                        ));
                    }
                }
                Kind::Arrow(a) if a.stuck && a.from_player => {
                    for (view, handle) in views.iter().zip(players.iter()) {
                        if view.alive
                            && view.huntable
                            && near_player(view.pos, ib, 1.0, 0.5)
                            && handle
                                .inventory()
                                .insert(Stack::new("minecraft:arrow", 1))
                                .is_none()
                        {
                            {
                                actions.push(Action::Picked {
                                    item: id,
                                    collector: view.eid,
                                    count: 1,
                                    gone: true,
                                });
                                e.dead = true;
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Merge item stacks lying next to each other, a second at a time.
        if now % 20 == 0 {
            merge_items(&mut all, &mut actions);
        }

        all.retain(|e| {
            if e.dead {
                gone.push(e.id);
            }
            !e.dead
        });
    }
    for id in gone {
        for p in &players {
            if p.tracked().remove(&id) {
                p.emit(&ServerEvent::DespawnEntity(id), world);
            }
        }
    }
    actions
}

fn merge_items(all: &mut [Entity], actions: &mut Vec<Action>) {
    for i in 0..all.len() {
        if all[i].dead {
            continue;
        }
        let Kind::Item(a) = &all[i].kind else {
            continue;
        };
        if a.owner.is_some() || a.stack.count >= a.stack.max_stack() {
            continue;
        }
        let pa = all[i].pos();
        for j in i + 1..all.len() {
            if all[j].dead {
                continue;
            }
            let pb = all[j].pos();
            if (pa.x - pb.x).abs() > 0.75 || (pa.y - pb.y).abs() > 0.5 || (pa.z - pb.z).abs() > 0.75
            {
                continue;
            }
            let (left, right) = all.split_at_mut(j);
            let (Kind::Item(a), Kind::Item(b)) = (&mut left[i].kind, &mut right[0].kind) else {
                continue;
            };
            if b.owner.is_some() || !a.stack.stacks_with(&b.stack) {
                continue;
            }
            let room = a.stack.max_stack().saturating_sub(a.stack.count);
            let n = room.min(b.stack.count);
            if n == 0 {
                break;
            }
            a.stack.count += n;
            b.stack.count -= n;
            let id_a = left[i].id;
            actions.push(Action::Tracked(
                id_a,
                ServerEvent::EntityMeta {
                    entity_id: id_a,
                    entries: vec![(8, MetaValue::Item(Some(a.stack.clone())))],
                },
            ));
            if b.stack.count == 0 {
                right[0].dead = true;
            }
        }
    }
}

/// Whether a player standing at `feet` reaches the box `b`, with vanilla's
/// pickup inflation of the player's box.
fn near_player(feet: Vector3, b: aether_api::Aabb, grow_h: f64, grow_v: f64) -> bool {
    // `getBoundingBox().inflate(grow_h, grow_v, grow_h)`: grown on every
    // side, so `grow_v` below the feet as well as above the head.
    let pb = aether_api::Aabb::from_base(feet, 0.6 + 2.0 * grow_h, 1.8 + 2.0 * grow_v)
        .offset(Vector3::new(0.0, -grow_v, 0.0));
    pb.intersects(b)
}

/// Whether the block at the feet of `pos` is water.
pub fn in_water(world: &DemoWorld, pos: Vector3) -> bool {
    let b = world.get_block(
        pos.x.floor() as i32,
        (pos.y + 0.1).floor() as i32,
        pos.z.floor() as i32,
    );
    crate::game::block_name(world, b).is_some_and(|n| n == "minecraft:water")
}

fn solid_at(world: &DemoWorld, x: f64, y: f64, z: f64) -> bool {
    let b = world.get_block(x.floor() as i32, y.floor() as i32, z.floor() as i32);
    world.props_of(b).collision
}

#[allow(clippy::too_many_arguments)]
fn tick_arrow(
    world: &DemoWorld,
    id: i32,
    body: &mut Body,
    yaw: &mut f32,
    pitch: &mut f32,
    a: &mut Arrow,
    players: &[PlayerView],
    mobs: &[(i32, aether_api::Aabb)],
    actions: &mut Vec<Action>,
) {
    if a.stuck {
        return;
    }
    let v = body.velocity;
    let steps = 4;
    for _ in 0..steps {
        let d = Vector3::new(v.x / steps as f64, v.y / steps as f64, v.z / steps as f64);
        body.aabb = body.aabb.offset(d);
        let c = body.feet();
        let centre = Vector3::new(c.x, c.y + 0.25, c.z);
        if solid_at(world, centre.x, centre.y, centre.z) {
            a.stuck = true;
            body.velocity = Vector3::new(0.0, 0.0, 0.0);
            actions.push(Action::Tracked(
                id,
                ServerEvent::EntityPos {
                    entity_id: id,
                    x: c.x,
                    y: c.y,
                    z: c.z,
                    yaw: *yaw,
                    pitch: *pitch,
                    on_ground: true,
                },
            ));
            return;
        }
        let speed = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
        let damage = (a.damage as f64 * speed).ceil() as f32;
        if a.from_player {
            for (mid, b) in mobs {
                if b.intersects(body.aabb) {
                    actions.push(Action::HitMob {
                        shooter: a.shooter,
                        target: *mid,
                        damage,
                        from: c,
                    });
                    a.stuck = true;
                    body.velocity = Vector3::new(0.0, 0.0, 0.0);
                    a.stuck_ticks = ARROW_LIFETIME; // gone next tick
                    return;
                }
            }
        }
        for p in players {
            if !p.huntable || Some(p.eid) == a.shooter {
                continue;
            }
            if aether_api::Aabb::from_base(p.pos, 0.6, 1.8).intersects(body.aabb) {
                actions.push(Action::Shot {
                    shooter: a.shooter,
                    target: p.eid,
                    damage,
                    from: c,
                });
                a.stuck = true;
                body.velocity = Vector3::new(0.0, 0.0, 0.0);
                a.stuck_ticks = ARROW_LIFETIME;
                return;
            }
        }
    }
    body.velocity = Vector3::new(v.x * 0.99, v.y * 0.99 - 0.05, v.z * 0.99);
    let h = (v.x * v.x + v.z * v.z).sqrt();
    *yaw = (-v.x).atan2(v.z).to_degrees() as f32;
    *pitch = (-v.y).atan2(h).to_degrees() as f32;
}

/// Bring every player's tracked set in line with what is in range.
pub fn update_tracking(registry: &SharedRegistry, world: &DemoWorld) {
    let players = registry.snapshot();
    if players.is_empty() {
        return;
    }
    let all = entities().lock().unwrap();
    let mut out: Vec<(std::sync::Arc<PlayerHandle>, Vec<ServerEvent<'static>>)> = Vec::new();
    for p in &players {
        let pos = p.pos();
        let mut evs = Vec::new();
        let mut tracked = p.tracked();
        for e in all.iter() {
            let ep = e.pos();
            let near = (ep.x - pos.x).abs() <= TRACK_RANGE && (ep.z - pos.z).abs() <= TRACK_RANGE;
            let has = tracked.contains(&e.id);
            if near && !has && !e.dead {
                tracked.insert(e.id);
                if p.full() {
                    evs.extend(e.spawn_events());
                } else if let Kind::Item(it) = &e.kind {
                    // Older codecs know dropped items by their own event.
                    evs.push(ServerEvent::DropItem {
                        entity_id: e.id,
                        x: ep.x,
                        y: ep.y,
                        z: ep.z,
                        item: it.stack.item.clone(),
                        count: it.stack.count as u64,
                    });
                }
            } else if !near && has {
                tracked.remove(&e.id);
                evs.push(ServerEvent::DespawnEntity(e.id));
            }
        }
        if !evs.is_empty() {
            out.push((std::sync::Arc::clone(p), evs));
        }
    }
    drop(all);
    for (p, evs) in out {
        for ev in &evs {
            p.emit(ev, world);
        }
    }
}

/// Forget what a client has spawned, so the tracker sends it all again —
/// after a respawn, whose new level starts empty.
pub fn reset_tracking(handle: &PlayerHandle) {
    handle.tracked().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_ids_stay_clear_of_the_player_range() {
        let a = next_id();
        let b = next_id();
        assert!(a >= 1_000_000 && b > a);
    }

    #[test]
    fn pickup_reach_matches_vanilla() {
        let feet = Vector3::new(0.0, 64.0, 0.0);
        let item =
            |x: f64, y: f64| aether_api::Aabb::from_base(Vector3::new(x, y, 0.0), 0.25, 0.25);
        assert!(near_player(feet, item(1.2, 64.0), 1.0, 0.5));
        assert!(!near_player(feet, item(1.6, 64.0), 1.0, 0.5));
        assert!(near_player(feet, item(0.0, 63.6), 1.0, 0.5));
        assert!(!near_player(feet, item(0.0, 63.2), 1.0, 0.5));
    }
}
