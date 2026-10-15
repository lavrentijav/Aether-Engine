//! The world tick: twenty times a second, everything that happens without a
//! player asking — time passing, mobs moving, food running down, furnaces
//! burning, items falling.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aether_api::Vector3;

use super::effects::{self, Effect, TickEvent};
use super::entities::{self, Action, PlayerView};
use super::{combat, containers, mobs, tables, window};
use crate::inventory::Stack;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

/// One tick, vanilla's 50 ms.
pub const TICK: Duration = Duration::from_millis(50);

/// KV key for the world clock.
const TIME_KEY: &[u8] = aether_api::keys::WORLD_TIME;

/// Start the game: install the shared handles and run the tick forever.
pub fn start(registry: SharedRegistry, world: Arc<DemoWorld>) {
    if let Ok(Some(b)) = world.get_meta(TIME_KEY) {
        if b.len() == 16 {
            let age = u64::from_be_bytes(b[..8].try_into().unwrap());
            let tod = i64::from_be_bytes(b[8..].try_into().unwrap());
            super::AGE.store(age, Ordering::Relaxed);
            super::set_time_of_day(tod);
        }
    }
    let _ = super::REGISTRY.set(Arc::clone(&registry));
    let _ = super::WORLD.set(Arc::clone(&world));
    std::thread::Builder::new()
        .name("game-tick".into())
        .spawn(move || {
            let mut rng = tables::Rng::from_time();
            loop {
                let start = Instant::now();
                tick_once(&registry, &world, &mut rng);
                let took = start.elapsed();
                super::SLOWEST_TICK_US.fetch_max(took.as_micros() as u64, Ordering::Relaxed);
                crate::web::record_tick(took.as_micros() as u64);
                std::thread::sleep(TICK.saturating_sub(took));
            }
        })
        .expect("failed to start the game tick");
}

fn tick_once(registry: &SharedRegistry, world: &DemoWorld, rng: &mut tables::Rng) {
    let now = super::AGE.fetch_add(1, Ordering::Relaxed) + 1;
    super::set_time_of_day(super::time_of_day() + 1);
    let night = super::is_night();

    let actions = entities::tick(world, registry, now, !night);
    for a in actions {
        apply(a, registry, world);
    }
    entities::update_tracking(registry, world);
    super::fluids::tick(world, registry);
    // Falling sand, gravel and concrete powder, leaf distance, crop growth:
    // the scheduled queue and the random-tick pass.
    super::blockticks::tick(world, registry, rng);

    let players = registry.snapshot();
    // Anti-xray: recompute a player's reachable sections where needed and
    // re-send (reveal) newly visible columns, capped per tick. A no-op unless
    // `[visibility] enabled`.
    crate::visibility::tick(&players, world);
    for p in &players {
        // Eating finishes here, in the tick: journal what it used up. Only
        // while eating, so a capture is not taken every tick for nothing.
        let _scope = (p.full() && p.game().eating.is_some())
            .then(|| super::provenance::Scope::begin(p, world));
        survival(p, registry, world, now);
    }

    // Plugins get one tick hook a frame, at a safe point: no world or player
    // lock is held here. The host applies each plugin's own period and budget.
    crate::plugins::on_tick(now);

    if now % 2 == 0 {
        broadcast_equipment(&players, registry, world);
    }
    if now % 5 == 0 {
        super::interact::dig_progress(registry, world);
    }

    let mut changed = containers::tick_furnaces(world);
    changed.extend(super::blockticks::hopper::tick(world));
    if now % 10 == 0 {
        for pos in changed {
            window::refresh_viewers(pos, world, None);
        }
    }

    if now % 20 == 0 {
        let ev = super::time_event();
        registry.broadcast(&ev, world);
    }

    if now % 40 == 0 && !players.is_empty() {
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
        for (def, at) in mobs::spawn_round(world, &views, night, rng) {
            let yaw = rng.f32() * 360.0;
            entities::spawn_mob(def, at, yaw);
        }
    }

    if now % 600 == 0 {
        save_time(world);
        // Bound what a crash loses of the pending scheduled ticks, the same
        // thirty-second horizon the world clock gets.
        super::blockticks::save(world);
        super::fluids::save(world);
    }
}

/// Persist the world clock.
pub fn save_time(world: &DemoWorld) {
    let mut b = Vec::with_capacity(16);
    b.extend_from_slice(&super::now().to_be_bytes());
    b.extend_from_slice(&super::time_of_day().to_be_bytes());
    let _ = world.put_meta(TIME_KEY, &b);
}

fn apply(a: Action, registry: &SharedRegistry, world: &DemoWorld) {
    match a {
        Action::Attack {
            mob,
            target,
            damage,
            from,
        } => {
            if let Some(p) = registry.by_entity(target) {
                combat::hurt_player(
                    &p,
                    damage,
                    "minecraft:mob_attack",
                    Some((mob, from)),
                    registry,
                    world,
                );
            }
        }
        Action::Shot {
            shooter,
            target,
            damage,
            from,
        } => {
            if let Some(p) = registry.by_entity(target) {
                combat::hurt_player(
                    &p,
                    damage,
                    "minecraft:arrow",
                    shooter.map(|s| (s, from)),
                    registry,
                    world,
                );
            }
        }
        Action::HitMob {
            shooter,
            target,
            damage,
            from,
        } => {
            if let Some(hit) = entities::hurt_mob(target, damage, Some(from), shooter) {
                let killer = shooter.and_then(|s| registry.by_entity(s));
                combat::on_mob_hurt(
                    target,
                    &hit,
                    killer.as_deref(),
                    registry,
                    world,
                    "minecraft:arrow",
                );
            }
        }
        Action::Explode { pos, radius } => combat::explode(pos, radius, registry, world),
        Action::Shoot { from, vel, shooter } => {
            entities::spawn_arrow(from, vel, Some(shooter), false, 2.0);
            registry.broadcast_tracking(
                shooter,
                &ServerEvent::Sound {
                    name: "minecraft:entity.skeleton.shoot",
                    category: 5,
                    x: from.x,
                    y: from.y,
                    z: from.z,
                    volume: 1.0,
                    pitch: 1.0,
                },
                world,
            );
        }
        Action::Picked {
            item,
            collector,
            count,
            gone,
        } => {
            let ev = ServerEvent::Collect {
                item,
                collector,
                count,
            };
            let picker = registry.by_entity(collector);
            for p in registry.snapshot() {
                if p.tracked().contains(&item) || p.entity_id == collector {
                    p.emit(&ev, world);
                }
            }
            if gone {
                for p in registry.snapshot() {
                    if p.tracked().remove(&item) {
                        p.emit(&ServerEvent::DespawnEntity(item), world);
                    }
                }
            }
            if let Some(p) = picker {
                p.sync_inventory(world);
                crate::session::save_inventory(&p, world);
            }
        }
        Action::XpPicked {
            orb,
            collector,
            value,
        } => {
            // Vanilla answers an orb pickup with a TakeItemEntity of amount 0;
            // the client has usually predicted it already.
            let ev = ServerEvent::Collect {
                item: orb,
                collector,
                count: 0,
            };
            for p in registry.snapshot() {
                if p.entity_id == collector || p.tracked().contains(&orb) {
                    p.emit(&ev, world);
                }
                if p.tracked().remove(&orb) {
                    p.emit(&ServerEvent::DespawnEntity(orb), world);
                }
            }
            if let Some(p) = registry.by_entity(collector) {
                combat::give_xp(&p, world, value);
            }
        }
        Action::PotionImpact {
            pos,
            potion,
            lingering,
            shooter,
        } => entities::potion_impact(registry, world, pos, potion, lingering, shooter),
        Action::CloudPulse {
            pos,
            potion,
            radius,
        } => entities::cloud_pulse(registry, world, pos, potion, radius),
        Action::Tracked(id, ev) => registry.broadcast_tracking(id, &ev, world),
    }
}

/// One player's tick: eating, hunger, regeneration, the void.
fn survival(p: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld, now: u64) {
    let pos = p.pos();
    let mut hurts: Vec<(f32, &'static str)> = Vec::new();
    let mut ate = false;
    // Effects to apply and to take away, and a milk bucket that clears them
    // all; all three are sent after the game lock is dropped.
    let mut applied: Vec<Effect> = Vec::new();
    let mut removed: Vec<u8> = Vec::new();
    let mut milk = false;
    let health_msg;
    {
        let mut st = p.game();
        st.hurt_cooldown = st.hurt_cooldown.saturating_sub(1);
        if st.dead {
            return;
        }
        if let Some((start, hand)) = st.eating {
            if now.saturating_sub(start) >= super::player::EAT_TICKS {
                st.eating = None;
                let mut inv = p.inventory();
                let slot = if hand == 1 {
                    crate::inventory::OFFHAND
                } else {
                    inv.held_index()
                };
                if let Some(food) = inv.slot(slot).cloned() {
                    if food
                        .item
                        .vanilla_name()
                        .is_some_and(super::brewing::is_drinkable)
                    {
                        // A drinkable potion applies its effects and leaves a
                        // glass bottle. Instant effects take hold at once, the
                        // rest wait in the effect list above.
                        let potion = food.potion.unwrap_or(super::brewing::WATER);
                        for e in super::brewing::effects_of(potion) {
                            if effects::is_instant(e.id) {
                                if e.id == effects::INSTANT_HEALTH {
                                    if st.survival() {
                                        st.health = (st.health
                                            + effects::instant_heal(e.amplifier, 1.0))
                                        .min(20.0);
                                    }
                                } else if st.survival() {
                                    hurts.push((
                                        effects::instant_damage(e.amplifier, 1.0),
                                        "minecraft:magic",
                                    ));
                                }
                            } else {
                                applied.push(e);
                            }
                        }
                        consume_one(&mut inv, slot, food.item, st.survival());
                        ate = true;
                    } else if let Some(f) = tables::food_id(food.item) {
                        st.food = (st.food + f.nutrition as i32).min(20);
                        st.saturation = (st.saturation + f.saturation).min(st.food as f32);
                        consume_one(&mut inv, slot, food.item, st.survival());
                        applied = food.item.vanilla_name().map_or_else(Vec::new, |n| {
                            entities::with_rng(|r| effects::food_effects(n, r))
                        });
                        ate = true;
                    } else if food.item == crate::inventory::item("minecraft:milk_bucket") {
                        // Milk cures every effect, as vanilla's does.
                        if st.survival() {
                            inv.put_slot(slot, Some(Stack::named("minecraft:bucket", 1)));
                        }
                        milk = true;
                        ate = true;
                    }
                }
            }
        }
        if st.survival() {
            st.food_timer += 1;
            if st.food >= 20 && st.saturation > 0.0 && st.health < 20.0 {
                if st.food_timer >= 10 {
                    let heal = st.saturation.min(6.0) / 6.0;
                    st.health = (st.health + heal).min(20.0);
                    st.exhaust(heal * 6.0);
                    st.food_timer = 0;
                }
            } else if st.food >= 18 && st.health < 20.0 {
                if st.food_timer >= 80 {
                    st.health = (st.health + 1.0).min(20.0);
                    st.exhaust(6.0);
                    st.food_timer = 0;
                }
            } else if st.food == 0 {
                if st.food_timer >= 80 {
                    st.food_timer = 0;
                    if st.health > 1.0 {
                        hurts.push((1.0, "minecraft:starve"));
                    }
                }
            } else {
                st.food_timer = st.food_timer.min(80);
            }

            st.env_timer += 1;
            if st.env_timer >= 10 {
                st.env_timer = 0;
                if pos.y < -128.0 {
                    hurts.push((4.0, "minecraft:out_of_world"));
                } else {
                    let feet = world.get_block(
                        pos.x.floor() as i32,
                        pos.y.floor() as i32,
                        pos.z.floor() as i32,
                    );
                    if crate::game::block_name(world, feet).is_some_and(|n| n == "minecraft:lava") {
                        hurts.push((4.0, "minecraft:lava"));
                    }
                }
            }
        }
        // Status effects advance for every player, so they still run out in
        // creative; only a survival player feels them.
        for ev in st.effects.tick() {
            match ev {
                TickEvent::Heal(h) => {
                    if st.survival() && st.health < 20.0 {
                        st.health = (st.health + h).min(20.0);
                    }
                }
                TickEvent::Damage { amount, lethal } => {
                    if st.survival() {
                        let damage = effects::tick_damage(st.health, amount, lethal);
                        if damage > 0.0 {
                            hurts.push((
                                damage,
                                if lethal {
                                    "minecraft:wither"
                                } else {
                                    "minecraft:magic"
                                },
                            ));
                        }
                    }
                }
                TickEvent::Exhaust(x) => {
                    if st.survival() {
                        st.exhaust(x);
                    }
                }
                TickEvent::Removed(id) => {
                    if id == effects::ABSORPTION {
                        st.absorption = 0.0;
                    }
                    removed.push(id);
                }
            }
        }
        let now_h = (st.health, st.food, st.saturation);
        health_msg = (now_h != st.sent_health).then(|| {
            st.sent_health = now_h;
            now_h
        });
    }
    if ate {
        p.emit(
            &ServerEvent::EntityStatus {
                entity_id: p.entity_id,
                status: 9,
            },
            world,
        );
        p.sync_inventory(world);
        crate::session::save_inventory(p, world);
    }
    if let Some((health, food, saturation)) = health_msg {
        p.emit(
            &ServerEvent::Health {
                health,
                food,
                saturation,
            },
            world,
        );
    }
    // Milk first, then what the food left: a cured player is not re-poisoned
    // by the same bite.
    if milk {
        effects::clear(p, registry, world);
    }
    effects::give_all(p, registry, world, &applied);
    for id in removed {
        let ev = ServerEvent::RemoveMobEffect {
            entity_id: p.entity_id,
            effect: id,
        };
        p.emit(&ev, world);
        registry.broadcast_except(p.entity_id, &ev, world);
        // A speed or slowness effect that ran out has to take its movement
        // modifier off the client too.
        if id == effects::SPEED || id == effects::SLOWNESS {
            effects::sync_movement(p, registry, world);
        }
    }
    for (amount, source) in hurts {
        combat::hurt_player(p, amount, source, None, registry, world);
    }
}

/// Use up one of `item` from `slot`, leaving a bowl or bottle where vanilla
/// does.
fn consume_one(
    inv: &mut crate::inventory::Inventory,
    slot: usize,
    item: aether_world::ItemTypeId,
    survival: bool,
) {
    if !survival {
        return;
    }
    // A destroy here is an item being eaten or drunk; the scope that wraps
    // this tick reads the reason when it journals.
    super::provenance::note_destroy_reason(aether_world::journal::DestroyReason::Consumed);
    let remainder = if item == crate::inventory::item("minecraft:mushroom_stew")
        || item == crate::inventory::item("minecraft:rabbit_stew")
        || item == crate::inventory::item("minecraft:beetroot_soup")
        || item == crate::inventory::item("minecraft:suspicious_stew")
    {
        Some("minecraft:bowl")
    } else if item == crate::inventory::item("minecraft:honey_bottle")
        || item
            .vanilla_name()
            .is_some_and(super::brewing::is_potion_item)
    {
        Some("minecraft:glass_bottle")
    } else {
        None
    };
    if let Some(Some(s)) = inv.slot_mut(slot) {
        s.count -= 1;
        if s.count == 0 {
            inv.put_slot(slot, remainder.map(|r| Stack::named(r, 1)));
            return;
        }
    }
    if let Some(r) = remainder {
        if let Some(left) = inv.insert(Stack::named(r, 1)) {
            let _ = left;
        }
    }
}

type Gear = Vec<(u8, Option<(aether_world::ItemTypeId, u16)>)>;

fn worn(p: &PlayerHandle) -> Gear {
    let inv = p.inventory();
    let at = |i: usize| inv.slot(i).map(|s| (s.item, s.damage));
    vec![
        (0, at(inv.held_index())),
        (1, at(crate::inventory::OFFHAND)),
        (2, at(8)),
        (3, at(7)),
        (4, at(6)),
        (5, at(5)),
    ]
}

/// Tell everyone else what each player holds and wears, when it changes.
fn broadcast_equipment(
    players: &[Arc<PlayerHandle>],
    registry: &SharedRegistry,
    world: &DemoWorld,
) {
    static LAST: OnceLock<Mutex<HashMap<i32, Gear>>> = OnceLock::new();
    let last = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut last = last.lock().unwrap();
    last.retain(|id, _| players.iter().any(|p| p.entity_id == *id));
    for p in players {
        let gear = worn(p);
        if last.get(&p.entity_id) == Some(&gear) {
            continue;
        }
        last.insert(p.entity_id, gear);
        registry.broadcast_except(p.entity_id, &equipment_event(p), world);
    }
}

/// The equipment packet describing `p`.
pub fn equipment_event(p: &PlayerHandle) -> ServerEvent<'static> {
    let inv = p.inventory();
    let at = |i: usize| inv.slot(i).cloned();
    ServerEvent::Equipment {
        entity_id: p.entity_id,
        slots: vec![
            (0, at(inv.held_index())),
            (1, at(crate::inventory::OFFHAND)),
            (2, at(8)),
            (3, at(7)),
            (4, at(6)),
            (5, at(5)),
        ],
    }
}
