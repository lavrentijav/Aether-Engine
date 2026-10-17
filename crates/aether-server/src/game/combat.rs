//! Damage, death and respawn, attacks and explosions.

use aether_api::{block_ids, JournalActor, Vector3};

use super::entities;
use super::mobs;
use super::player::HURT_COOLDOWN;
use super::tables;
use crate::inventory::Stack;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

/// Damage sources armour does nothing against.
fn bypasses_armor(source: &str) -> bool {
    matches!(
        source,
        "minecraft:fall"
            | "minecraft:out_of_world"
            | "minecraft:starve"
            | "minecraft:drown"
            | "minecraft:generic_kill"
    )
}

/// Damage sources fire resistance turns away.
fn is_fire(source: &str) -> bool {
    matches!(
        source,
        "minecraft:in_fire"
            | "minecraft:on_fire"
            | "minecraft:lava"
            | "minecraft:hot_floor"
            | "minecraft:campfire"
    )
}

/// Armour points and toughness a player is wearing.
fn worn_armor(handle: &PlayerHandle) -> (f32, f32) {
    let inv = handle.inventory();
    (5..=8)
        .filter_map(|i| inv.slot(i))
        .map(|s| tables::armor_points_id(s.item))
        .fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1))
}

/// Wear the armour a hit landed on.
fn wear_armor(handle: &PlayerHandle, damage: f32) {
    let wear = (damage / 4.0).floor().max(1.0) as u16;
    let mut inv = handle.inventory();
    for i in 5..=8 {
        if let Some(slot) = inv.slot_mut(i) {
            if let Some(s) = slot {
                let max = tables::max_durability_id(s.item);
                if max > 0 {
                    s.damage += wear;
                    if s.damage >= max {
                        *slot = None;
                    }
                }
            }
        }
    }
}

/// The damage that reaches health after resistance and absorption, spending
/// absorption as it soaks.
fn after_effects(dealt: f32, resistance: f32, absorption: &mut f32) -> f32 {
    let mut damage = dealt * (1.0 - resistance).max(0.0);
    if *absorption > 0.0 {
        let soaked = damage.min(*absorption);
        *absorption -= soaked;
        damage -= soaked;
    }
    damage
}

/// Hurt a player. Returns whether the hit landed.
pub fn hurt_player(
    target: &PlayerHandle,
    amount: f32,
    source: &'static str,
    attacker: Option<(i32, Vector3)>,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> bool {
    if amount <= 0.0 {
        return false;
    }
    // Vanilla's friendly-fire rule: a player on the same team as the victim
    // deals no damage while that team's friendly fire is off.
    if let Some((attacker_id, _)) = attacker {
        if let Some(attacker) = registry.by_entity(attacker_id) {
            if attacker.entity_id != target.entity_id
                && !crate::team::allows_damage(world, &attacker.name, &target.name)
            {
                return false;
            }
        }
    }
    let armor = if bypasses_armor(source) {
        (0.0, 0.0)
    } else {
        worn_armor(target)
    };
    let dealt = tables::after_armor(amount, armor.0, armor.1);
    let (health, food, sat, died) = {
        let mut st = target.game();
        if !st.survival() || st.dead || (st.hurt_cooldown > 0 && source != "minecraft:out_of_world")
        {
            return false;
        }
        // Fire resistance: the blow never lands at all.
        if is_fire(source) && st.effects.fire_resistant() {
            return false;
        }
        // Resistance turns a fraction of the blow away, then absorption
        // soaks what is left before health.
        let dealt = after_effects(dealt, st.effects.resistance_fraction(), &mut st.absorption);
        st.hurt_cooldown = HURT_COOLDOWN;
        st.health -= dealt;
        st.exhaust(0.1);
        st.eating = None;
        let died = st.health <= 0.0;
        if died {
            st.health = 0.0;
            st.dead = true;
        }
        (st.health, st.food, st.saturation, died)
    };
    if armor.0 > 0.0 {
        wear_armor(target, amount);
        target.sync_inventory(world);
    }
    let ev = ServerEvent::Damage {
        entity_id: target.entity_id,
        source,
        attacker: attacker.map(|a| a.0),
    };
    target.emit(&ev, world);
    registry.broadcast_except(target.entity_id, &ev, world);
    target.emit(
        &ServerEvent::Health {
            health,
            food,
            saturation: sat,
        },
        world,
    );
    crate::plugins::after_damage(target.entity_id as u64, dealt as f64, source);
    if let Some((_, from)) = attacker {
        let p = target.pos();
        let (dx, dz) = (p.x - from.x, p.z - from.z);
        let len = (dx * dx + dz * dz).sqrt().max(1e-4);
        target.emit(
            &ServerEvent::EntityVelocity {
                entity_id: target.entity_id,
                velocity: (dx / len * 0.4, 0.36, dz / len * 0.4),
            },
            world,
        );
    }
    if died {
        die(target, source, attacker.map(|a| a.0), registry, world);
    }
    true
}

fn death_message(
    name: &str,
    source: &str,
    attacker: Option<i32>,
    registry: &SharedRegistry,
) -> String {
    let killer = attacker.and_then(|a| {
        registry
            .by_entity(a)
            .map(|p| p.name.clone())
            .or_else(|| super::entities::kind_of(a).map(pretty))
    });
    match (source, killer) {
        ("minecraft:fall", _) => format!("{name} hit the ground too hard"),
        ("minecraft:out_of_world", _) => format!("{name} fell out of the world"),
        ("minecraft:lava", _) => format!("{name} tried to swim in lava"),
        ("minecraft:starve", _) => format!("{name} starved to death"),
        ("minecraft:drown", _) => format!("{name} drowned"),
        ("minecraft:explosion" | "minecraft:player_explosion", _) => format!("{name} blew up"),
        ("minecraft:arrow", Some(k)) => format!("{name} was shot by {k}"),
        (_, Some(k)) => format!("{name} was slain by {k}"),
        _ => format!("{name} died"),
    }
}

/// `minecraft:cave_spider` → `Cave Spider`.
fn pretty(kind: &str) -> String {
    kind.strip_prefix("minecraft:")
        .unwrap_or(kind)
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A player died: death screen, message, and everything they carried on the
/// floor.
pub fn die(
    target: &PlayerHandle,
    source: &str,
    attacker: Option<i32>,
    registry: &SharedRegistry,
    world: &DemoWorld,
) {
    let text = death_message(&target.name, source, attacker, registry);
    crate::log::info(&format!("[death] {text}"));
    crate::plugins::after_entity_death(target.entity_id as u64, attacker.map(|a| a as u64));
    // Death clears every effect; tell the client before the respawn screen
    // takes over.
    super::effects::clear(target, registry, world);
    // Everything the player held goes to the ground, each stack keeping its
    // identity; the scope journals the moves when it ends.
    let _scope = super::provenance::Scope::begin(target, world);
    let items: Vec<Stack> = {
        let mut st = target.game();
        st.window = None;
        st.absorption = 0.0;
        // The inventory is gone, so the creative cursor shadow must go too: a
        // stand-in reported after death must not resolve to the dropped item.
        st.clear_creative_shadow();
        let mut items: Vec<Stack> = st.cursor.take().into_iter().collect();
        st.xp_total = 0;
        let mut inv = target.inventory();
        for i in 0..crate::inventory::SLOTS {
            if let Some(s) = inv.take_slot(i) {
                items.push(s);
            }
        }
        items
    };
    let p = target.pos();
    for s in items {
        let (vx, vz) = entities::with_rng(|r| (r.f64() * 0.4 - 0.2, r.f64() * 0.4 - 0.2));
        let at = Vector3::new(p.x, p.y + 1.0, p.z);
        super::provenance::note_thrown(
            world,
            aether_world::journal::ActorId(target.uuid),
            &s,
            super::provenance::ground_place(at.x, at.y, at.z),
        );
        entities::spawn_item(at, s, Vector3::new(vx, 0.2, vz), 40, None);
    }
    crate::session::save_inventory(target, world);
    target.emit(
        &ServerEvent::DeathMessage {
            entity_id: target.entity_id,
            text: text.clone(),
        },
        world,
    );
    let status = ServerEvent::EntityStatus {
        entity_id: target.entity_id,
        status: 3,
    };
    registry.broadcast_except(target.entity_id, &status, world);
    registry.broadcast(&ServerEvent::Chat(text), world);
    target.sync_inventory(world);
}

/// The attack cooldown scale, `0.2..=1.0`, vanilla 1.9.
fn cooldown_scale(ticks_since: u64, attacks_per_second: f32) -> f32 {
    let period = 20.0 / attacks_per_second;
    let charge = ((ticks_since as f32 + 0.5) / period).clamp(0.0, 1.0);
    0.2 + charge * charge * 0.8
}

/// A player hits entity `target`.
pub fn player_attack(
    attacker: &PlayerHandle,
    target: i32,
    registry: &SharedRegistry,
    world: &DemoWorld,
    now: u64,
) {
    if target == attacker.entity_id {
        return;
    }
    let held = attacker.held_item();
    let (base, speed) = tables::weapon_id(held);
    let pos = attacker.pos();
    let from = Vector3::new(pos.x, pos.y, pos.z);
    let (scale, crit) = {
        let mut st = attacker.game();
        if st.dead {
            return;
        }
        let since = now.saturating_sub(st.last_attack);
        st.last_attack = now;
        st.exhaust(0.1);
        let scale = cooldown_scale(since, speed);
        let crit = scale > 0.9 && !pos.on_ground && !st.sprinting;
        (scale, crit)
    };
    let mut damage = base * scale;
    if crit {
        damage *= 1.5;
    }
    let creative = !attacker.game().survival();

    if let Some(victim) = registry.by_entity(target) {
        // Reach check: vanilla allows about three blocks, plus slack for lag.
        let v = victim.pos();
        if (v.x - pos.x).powi(2) + (v.y - pos.y).powi(2) + (v.z - pos.z).powi(2) > 6.0 * 6.0 {
            return;
        }
        if hurt_player(
            &victim,
            damage,
            "minecraft:player_attack",
            Some((attacker.entity_id, from)),
            registry,
            world,
        ) {
            if crit {
                registry.broadcast(
                    &ServerEvent::EntityAnimation {
                        entity_id: target,
                        animation: 4,
                    },
                    world,
                );
            }
            wear_held(attacker, world, 1);
        }
        return;
    }
    let Some(tpos) = entities::position(target) else {
        return;
    };
    if (tpos.x - pos.x).powi(2) + (tpos.y - pos.y).powi(2) + (tpos.z - pos.z).powi(2) > 6.0 * 6.0 {
        return;
    }
    if let Some(hit) = entities::hurt_mob(target, damage, Some(from), Some(attacker.entity_id)) {
        on_mob_hurt(
            target,
            &hit,
            Some(attacker),
            registry,
            world,
            "minecraft:player_attack",
        );
        if crit {
            registry.broadcast_tracking(
                target,
                &ServerEvent::EntityAnimation {
                    entity_id: target,
                    animation: 4,
                },
                world,
            );
        }
        if !creative {
            wear_held(attacker, world, 1);
        }
    }
}

/// Everything that follows a mob taking damage: the flash, and on death the
/// animation, the loot and the experience.
pub fn on_mob_hurt(
    id: i32,
    hit: &entities::MobHit,
    killer: Option<&PlayerHandle>,
    registry: &SharedRegistry,
    world: &DemoWorld,
    source: &'static str,
) {
    registry.broadcast_tracking(
        id,
        &ServerEvent::Damage {
            entity_id: id,
            source,
            attacker: killer.map(|k| k.entity_id),
        },
        world,
    );
    crate::plugins::after_damage(id as u64, hit.damage as f64, source);
    if !hit.killed {
        return;
    }
    registry.broadcast_tracking(
        id,
        &ServerEvent::EntityStatus {
            entity_id: id,
            status: 3,
        },
        world,
    );
    let ctx = super::loot::LootContext {
        player_kill: killer.is_some(),
        entity_kind: Some(hit.kind.to_owned()),
        entity_on_fire: hit.on_fire,
        entity_components: if hit.kind == "minecraft:sheep" {
            vec![(
                "minecraft:sheep/color".to_owned(),
                mobs::wool_name(hit.wool).to_owned(),
            )]
        } else {
            Vec::new()
        },
        entity_type_specific: if hit.kind == "minecraft:sheep" {
            Some((
                "minecraft:sheep".to_owned(),
                vec![("sheared".to_owned(), hit.sheared as i64)],
            ))
        } else {
            None
        },
        ..Default::default()
    };
    let drops = entities::with_rng(|r| tables::mob_drops_ctx(hit.kind, &ctx, r));
    let stacks: Vec<Stack> = drops
        .into_iter()
        .map(|(i, n)| Stack::named(&i, n))
        .collect();
    entities::drop_at_block(
        hit.pos.x.floor() as i32,
        hit.pos.y.floor() as i32,
        hit.pos.z.floor() as i32,
        stacks,
        killer
            .map(|k| aether_world::journal::ActorId(k.uuid))
            .unwrap_or(aether_world::journal::ActorId::SERVER),
        None,
    );
    // Experience is left on the ground for whoever is near, not credited to
    // the killer: `LivingEntity.dropExperience`. Mobs only pay it out when a
    // player did the kill (`lastHurtByPlayer`), so an arrow from one mob into
    // another drops none.
    if killer.is_some() {
        entities::award_xp(hit.pos, hit.xp);
    }
    crate::plugins::after_entity_death(id as u64, killer.map(|k| k.entity_id as u64));
}

/// Add experience to a player.
pub fn give_xp(handle: &PlayerHandle, world: &DemoWorld, amount: i32) {
    let (bar, level, total) = {
        let mut st = handle.game();
        st.xp_total += amount;
        let (level, bar) = st.xp_level();
        (bar, level, st.xp_total)
    };
    handle.emit(&ServerEvent::Experience { bar, level, total }, world);
}

/// Take `n` points of wear off the held tool, breaking it at the end.
pub fn wear_held(handle: &PlayerHandle, world: &DemoWorld, n: u16) {
    let broke = {
        let mut inv = handle.inventory();
        let i = inv.held_index();
        let Some(slot) = inv.slot_mut(i) else { return };
        let Some(s) = slot else { return };
        let max = tables::max_durability_id(s.item);
        if max == 0 {
            return;
        }
        s.damage += n;
        if s.damage >= max {
            *slot = None;
            true
        } else {
            false
        }
    };
    if broke {
        let p = handle.pos();
        handle.emit(
            &ServerEvent::Sound {
                name: "minecraft:entity.item.break".into(),
                category: 7,
                x: p.x,
                y: p.y,
                z: p.z,
                volume: 0.8,
                pitch: 1.0,
            },
            world,
        );
    }
    handle.sync_inventory(world);
}

/// Bring a dead player back at their spawn point.
///
/// The connection thread does the rest — resending columns and entities —
/// because it owns the column window.
pub fn respawn(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Option<(f64, f64, f64)> {
    let (mode, spawn) = {
        let mut st = handle.game();
        if !st.dead {
            return None;
        }
        st.dead = false;
        st.health = 20.0;
        st.food = 20;
        st.saturation = 5.0;
        st.exhaustion = 0.0;
        st.fall_peak = None;
        st.hurt_cooldown = 60;
        st.eating = None;
        st.digging = None;
        st.clear_creative_shadow();
        (st.mode, st.spawn)
    };
    entities::reset_tracking(handle);
    handle.emit(&ServerEvent::Respawn { game_mode: mode }, world);
    let mut pos = handle.pos();
    pos.x = spawn.0;
    pos.y = spawn.1;
    pos.z = spawn.2;
    pos.on_ground = false;
    handle.set_pos(pos);
    registry.broadcast_except(
        handle.entity_id,
        &ServerEvent::DespawnEntity(handle.entity_id),
        world,
    );
    Some(spawn)
}

/// Blow up at `pos`: break blocks, hurt and push everyone nearby.
///
/// The region `explosion` flag is consulted per block: a protected block is
/// skipped while the rest of the blast still breaks. Entity damage is not
/// region-gated (the brief scopes the flag to block damage).
pub fn explode(pos: Vector3, radius: f32, registry: &SharedRegistry, world: &DemoWorld) {
    // One snapshot for the whole blast; the check itself is lock-free and
    // allocates nothing. Protected blocks are skipped, unprotected ones in
    // the same blast still break.
    let regions = crate::region::store().snapshot();
    let r = radius as f64;
    let ri = radius.ceil() as i32;
    let (cx, cy, cz) = (
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );
    let mut broken = Vec::new();
    for dy in -ri..=ri {
        for dz in -ri..=ri {
            for dx in -ri..=ri {
                let d = ((dx * dx + dy * dy + dz * dz) as f64).sqrt();
                let jitter = entities::with_rng(|g| g.f64() * 0.6);
                if d > r - jitter {
                    continue;
                }
                let (x, y, z) = (cx + dx, cy + dy, cz + dz);
                let verdict = regions.check(
                    crate::region::OVERWORLD,
                    crate::region::BlockPos::new(x, y, z),
                    crate::region::Action::Explosion,
                    &crate::region::Actor::server(),
                );
                if !verdict.allows() {
                    continue;
                }
                let b = world.get_block(x, y, z);
                if b == block_ids::AIR {
                    continue;
                }
                let Some(name) = crate::game::block_name(world, b) else {
                    continue;
                };
                let hard = tables::block_info(&name).map(|i| i.hardness).unwrap_or(0.0);
                if !(0.0..50.0).contains(&hard)
                    || name == "minecraft:water"
                    || name == "minecraft:lava"
                {
                    continue;
                }
                broken.push((x, y, z, name));
            }
        }
    }
    let actor = JournalActor(0);
    for (x, y, z, name) in &broken {
        let seq = world.set_block_by(
            actor,
            *x,
            *y,
            *z,
            block_ids::AIR,
            world.props_of(block_ids::AIR),
        );
        crate::visibility::invalidate_block(*x, *y, *z);
        super::fluids::notify(world, *x, *y, *z);
        super::blockticks::notify(world, *x, *y, *z);
        registry.broadcast(
            &ServerEvent::BlockChange {
                x: *x,
                y: *y,
                z: *z,
                block: block_ids::AIR,
            },
            world,
        );
        if let Some(kind) = super::containers::ContainerKind::of_block(name) {
            super::window::close_viewers((*x, *y, *z), world);
            let items = super::containers::remove(world, (*x, *y, *z), kind);
            entities::spill(*x, *y, *z, items, actor);
        }
        // Vanilla drops each block with a chance of 1 / radius.
        if entities::with_rng(|g| g.chance(1.0 / radius)) {
            let drops = entities::with_rng(|g| {
                tables::block_drops(name, Some("minecraft:netherite_pickaxe"), g)
            });
            entities::drop_at_block(
                *x,
                *y,
                *z,
                drops
                    .into_iter()
                    .map(|(i, n)| Stack::named(&i, n))
                    .collect(),
                actor,
                seq,
            );
        }
    }
    for p in registry.snapshot() {
        let pp = p.pos();
        let (dx, dy, dz) = (pp.x - pos.x, pp.y + 0.9 - pos.y, pp.z - pos.z);
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        let reach = r * 2.0;
        let push = if dist < reach {
            (1.0 - dist / reach).max(0.0)
        } else {
            0.0
        };
        let knock = (push > 0.0).then(|| {
            let len = dist.max(1e-3);
            (dx / len * push, dy / len * push, dz / len * push)
        });
        if dist < 64.0 {
            p.emit(
                &ServerEvent::Explosion {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    radius,
                    knockback: knock,
                },
                world,
            );
        }
        if push > 0.0 {
            let damage = ((push * push + push) / 2.0 * 7.0 * reach + 1.0) as f32;
            hurt_player(&p, damage, "minecraft:explosion", None, registry, world);
        }
    }
    // Mobs caught in the blast.
    for (id, mp) in entities::mobs_in(pos, r * 2.0) {
        let dist =
            ((mp.x - pos.x).powi(2) + (mp.y - pos.y).powi(2) + (mp.z - pos.z).powi(2)).sqrt();
        let push = (1.0 - dist / (r * 2.0)).max(0.0);
        let damage = ((push * push + push) / 2.0 * 7.0 * r * 2.0 + 1.0) as f32;
        if let Some(hit) = entities::hurt_mob(id, damage, Some(pos), None) {
            on_mob_hurt(id, &hit, None, registry, world, "minecraft:explosion");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_world(seed: u64) -> DemoWorld {
        use aether_api::{FjallStore, World};
        let dir = std::env::temp_dir().join(format!("aether-region-{}-{seed}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", seed).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, seed, revision, &cfg);
        World::new(store, generator)
    }

    #[test]
    fn an_explosion_skips_protected_blocks_and_breaks_the_rest() {
        let regions = crate::region::install_test_store(crate::region::RegionStore::new());
        let region = crate::region::Region::new(
            crate::region::RegionId(1),
            "overworld",
            crate::region::BlockPos::new(0, 0, 0),
            crate::region::BlockPos::new(7, 255, 7),
        )
        .with_flag("explosion", false);
        regions.replace_all([region.handle()]);

        let world = test_world(29);
        let registry: SharedRegistry = std::sync::Arc::new(crate::players::Registry::default());
        world.set_block(4, 200, 2, "minecraft:stone");
        world.set_block(12, 200, 2, "minecraft:stone");
        explode(Vector3::new(8.0, 200.0, 2.0), 6.0, &registry, &world);
        assert_ne!(
            world.get_block(4, 200, 2),
            block_ids::AIR,
            "the protected block survived"
        );
        assert_eq!(
            world.get_block(12, 200, 2),
            block_ids::AIR,
            "the unprotected block in the same blast broke"
        );
    }

    #[test]
    fn a_full_charge_hits_full_and_spam_hits_a_fifth() {
        assert!((cooldown_scale(100, 1.6) - 1.0).abs() < 1e-6);
        assert!(cooldown_scale(0, 1.6) < 0.25);
    }

    #[test]
    fn mob_names_read_as_names() {
        assert_eq!(pretty("minecraft:cave_spider"), "Cave Spider");
        assert_eq!(pretty("minecraft:zombie"), "Zombie");
    }

    #[test]
    fn resistance_and_absorption_cut_the_blow_before_health() {
        // Resistance I turns a fifth away.
        let mut absorb = 0.0;
        assert!((after_effects(10.0, 0.2, &mut absorb) - 8.0).abs() < 1e-6);
        // Absorption soaks what is left and is spent.
        let mut absorb = 20.0;
        assert_eq!(after_effects(10.0, 0.0, &mut absorb), 0.0);
        assert_eq!(absorb, 10.0);
        // A blow larger than the absorption leaves the rest for health.
        let mut absorb = 3.0;
        assert_eq!(after_effects(10.0, 0.0, &mut absorb), 7.0);
        assert_eq!(absorb, 0.0);
        // Full resistance takes nothing.
        let mut absorb = 0.0;
        assert_eq!(after_effects(10.0, 1.0, &mut absorb), 0.0);
    }

    #[test]
    fn fire_sources_are_recognised() {
        assert!(is_fire("minecraft:lava"));
        assert!(is_fire("minecraft:on_fire"));
        assert!(!is_fire("minecraft:player_attack"));
        assert!(!is_fire("minecraft:fall"));
    }
}
