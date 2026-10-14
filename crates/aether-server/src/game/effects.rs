//! Status effects: the model, where they come from, and the rules the server
//! applies while one is running.
//!
//! An effect is one entry on a player or a mob: an id from the
//! `minecraft:mob_effect` registry, an amplifier (`0` is level I), a
//! duration, and the three display flags the protocol carries. The model
//! itself is version-neutral; [`super::effects::give`] renders it through the
//! client's codec as Update Mob Effect / Remove Mob Effect.
//!
//! Which of them the server acts on is deliberately a short list. Regeneration
//! heals, poison and wither hurt (poison never below one heart), hunger burns
//! saturation, absorption soaks damage, resistance reduces it, fire resistance
//! turns fire and lava away, and speed and slowness move the entity's
//! `minecraft:movement_speed` attribute — which is sent to the client so its
//! own prediction agrees. Everything else is carried and displayed, which is
//! all the rest of the effect list does without potions or beacons.

use super::tables::Rng;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

// The `minecraft:mob_effect` registry, whose order is fixed and shared by
// every version this server speaks (new effects append, so the old ids do
// not move). Only the ones the server acts on or that a food applies are
// named.
pub const SPEED: u8 = 0;
pub const SLOWNESS: u8 = 1;
pub const STRENGTH: u8 = 4;
/// Instant health and instant damage are applied the moment they arrive and
/// are never stored on the entity — see [`is_instant`].
pub const INSTANT_HEALTH: u8 = 5;
pub const INSTANT_DAMAGE: u8 = 6;
pub const JUMP_BOOST: u8 = 7;
pub const NAUSEA: u8 = 8;
pub const REGENERATION: u8 = 9;
pub const RESISTANCE: u8 = 10;
pub const FIRE_RESISTANCE: u8 = 11;
pub const WATER_BREATHING: u8 = 12;
pub const INVISIBILITY: u8 = 13;
pub const BLINDNESS: u8 = 14;
pub const NIGHT_VISION: u8 = 15;
pub const HUNGER: u8 = 16;
pub const WEAKNESS: u8 = 17;
pub const POISON: u8 = 18;
pub const WITHER: u8 = 19;
pub const ABSORPTION: u8 = 21;
pub const SATURATION: u8 = 22;
pub const LUCK: u8 = 25;
pub const SLOW_FALLING: u8 = 27;
pub const WIND_CHARGED: u8 = 35;
pub const WEAVING: u8 = 36;
pub const OOZING: u8 = 37;
pub const INFESTED: u8 = 38;

/// Whether an effect takes hold immediately rather than over time. Vanilla's
/// `InstantenousMobEffect`: instant health heals and instant damage hurts, and
/// the effect itself is never added to the entity.
pub fn is_instant(id: u8) -> bool {
    matches!(id, INSTANT_HEALTH | INSTANT_DAMAGE)
}

/// The immediate heal of an instant-health effect at `amplifier`, scaled by a
/// splash's distance factor — vanilla's `HealOrHarmMobEffect`, `(int)(d * (4
/// << amplifier) + 0.5)`.
pub fn instant_heal(amplifier: u8, factor: f64) -> f32 {
    (factor * (4i64 << amplifier.min(30)) as f64 + 0.5) as i64 as f32
}

/// The immediate damage of an instant-damage effect, vanilla's
/// `(int)(d * (6 << amplifier) + 0.5)`.
pub fn instant_damage(amplifier: u8, factor: f64) -> f32 {
    (factor * (6i64 << amplifier.min(30)) as f64 + 0.5) as i64 as f32
}

/// The `minecraft:attribute` registry id of `movement_speed`. The registry
/// is alphabetical in the data dump but registered in vanilla's own order on
/// the wire; this is the wire id in every version this codec speaks.
pub const MOVEMENT_SPEED_ATTRIBUTE: i32 = 20;
/// `AttributeModifier.Operation.ADD_MULTIPLIED_TOTAL`.
const OP_ADD_MULTIPLIED_TOTAL: i8 = 2;
/// A player's base movement speed, the value the client expects in Update
/// Attributes.
const PLAYER_BASE_MOVEMENT_SPEED: f64 = 0.1;

/// One active effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Effect {
    pub id: u8,
    /// `0` is level I.
    pub amplifier: u8,
    /// Ticks left; `-1` never runs out.
    pub duration: i32,
    /// From a beacon or conduit: fewer particles.
    pub ambient: bool,
    /// Show swirl particles.
    pub visible: bool,
    /// Show the HUD icon.
    pub show_icon: bool,
}

impl Effect {
    /// An effect with the usual display flags.
    pub fn new(id: u8, amplifier: u8, duration: i32) -> Self {
        Self {
            id,
            amplifier,
            duration,
            ambient: false,
            visible: true,
            show_icon: true,
        }
    }

    /// The same effect, hidden from the HUD.
    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self.show_icon = false;
        self
    }
}

/// Everything running on one entity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects(Vec<Effect>);

impl Effects {
    pub fn all(&self) -> &[Effect] {
        &self.0
    }

    /// Build from a persisted list, dropping duplicates (the last wins).
    pub fn from_list(list: Vec<Effect>) -> Self {
        let mut out = Effects::default();
        for e in list {
            out.0.retain(|x| x.id != e.id);
            out.0.push(e);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Effect> {
        self.0.iter()
    }

    pub fn get(&self, id: u8) -> Option<&Effect> {
        self.0.iter().find(|e| e.id == id)
    }

    pub fn active(&self, id: u8) -> bool {
        self.get(id).is_some()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Apply `e`; returns the effect now stored when it is new or replaces a
    /// weaker one, and `None` when the running effect was already at least as
    /// strong. Vanilla's rule: a higher amplifier wins outright, an equal
    /// amplifier takes the longer duration, and an infinite effect is never
    /// cut short.
    pub fn apply(&mut self, e: Effect) -> Option<Effect> {
        match self.0.iter_mut().find(|x| x.id == e.id) {
            Some(slot) => {
                let stronger = if slot.duration < 0 && e.duration >= 0 {
                    false
                } else if e.duration < 0 || e.amplifier > slot.amplifier {
                    true
                } else {
                    e.amplifier == slot.amplifier && e.duration > slot.duration
                };
                if stronger {
                    *slot = e;
                    Some(e)
                } else {
                    None
                }
            }
            None => {
                self.0.push(e);
                Some(e)
            }
        }
    }

    /// Remove one effect, returning what was running.
    pub fn remove(&mut self, id: u8) -> Option<Effect> {
        let i = self.0.iter().position(|e| e.id == id)?;
        Some(self.0.remove(i))
    }

    /// How much of incoming damage absorption still has to give.
    pub fn absorption_amount(&self) -> f32 {
        match self.get(ABSORPTION) {
            Some(e) => 4.0 * (e.amplifier as f32 + 1.0),
            None => 0.0,
        }
    }

    /// The fraction of damage resistance turns away, `0.0..=1.0`.
    pub fn resistance_fraction(&self) -> f32 {
        match self.get(RESISTANCE) {
            Some(e) => (0.2 * (e.amplifier as f32 + 1.0)).min(1.0),
            None => 0.0,
        }
    }

    /// Whether fire, lava and burning are turned away.
    pub fn fire_resistant(&self) -> bool {
        self.active(FIRE_RESISTANCE)
    }

    /// The multiplier speed and slowness put on movement: `+20 %` per level
    /// of Speed and `-15 %` per level of Slowness, applied to the total the
    /// way vanilla's `ADD_MULTIPLIED_TOTAL` modifiers are.
    pub fn movement_multiplier(&self) -> f64 {
        let mut m = 1.0;
        if let Some(e) = self.get(SPEED) {
            m += 0.2 * (e.amplifier as f64 + 1.0);
        }
        if let Some(e) = self.get(SLOWNESS) {
            m -= 0.15 * (e.amplifier as f64 + 1.0);
        }
        m.max(0.0)
    }

    /// Advance every effect one tick, returning what the entity should feel.
    ///
    /// Durations already at zero are not decremented, so an infinite (`-1`)
    /// effect stays.
    pub fn tick(&mut self) -> Vec<TickEvent> {
        let mut out = Vec::new();
        let mut expired: Vec<u8> = Vec::new();
        for e in self.0.iter_mut() {
            let d = if e.duration < 0 { i32::MAX } else { e.duration };
            match e.id {
                REGENERATION => {
                    // `50 >> amplifier` ticks between each heart, never less
                    // than one.
                    let interval = (50u32 >> e.amplifier.min(6)).max(1) as i32;
                    if d % interval == 0 {
                        out.push(TickEvent::Heal(1.0));
                    }
                }
                POISON => {
                    let interval = (25u32 >> e.amplifier.min(6)).max(1) as i32;
                    if d % interval == 0 {
                        out.push(TickEvent::Damage {
                            amount: 1.0,
                            lethal: false,
                        });
                    }
                }
                WITHER => {
                    let interval = (40u32 >> e.amplifier.min(6)).max(1) as i32;
                    if d % interval == 0 {
                        out.push(TickEvent::Damage {
                            amount: 1.0,
                            lethal: true,
                        });
                    }
                }
                HUNGER => {
                    // `addExhaustion(0.005 * (amplifier + 1))` every tick.
                    out.push(TickEvent::Exhaust(0.005 * (e.amplifier as f32 + 1.0)));
                }
                _ => {}
            }
            if e.duration > 0 {
                e.duration -= 1;
                if e.duration == 0 {
                    expired.push(e.id);
                }
            }
        }
        for id in expired {
            self.0.retain(|e| e.id != id);
            out.push(TickEvent::Removed(id));
        }
        out
    }
}

/// What a tick of an effect does to the entity carrying it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TickEvent {
    /// Regeneration: heal this much, up to the maximum.
    Heal(f32),
    /// Poison or wither: deal this much, but a non-lethal hit stops at one
    /// heart (see [`tick_damage`]).
    Damage { amount: f32, lethal: bool },
    /// Hunger: spend this much exhaustion.
    Exhaust(f32),
    /// The effect ran out; its icon and particles go away.
    Removed(u8),
}

/// The damage a `lethal == false` tick actually deals to `health`, honouring
/// poison's floor of one heart.
pub fn tick_damage(health: f32, amount: f32, lethal: bool) -> f32 {
    if lethal {
        amount
    } else {
        amount.min((health - 1.0).max(0.0))
    }
}

// ---------------------------------------------------------------------------
// Sources: what eating each food applies.
// ---------------------------------------------------------------------------

/// The effects eating `item` puts on. Chance-based foods roll `rng`.
///
/// Suspicious stew's effect is really stored in the bowl from the flower it
/// was made with; this server keeps no item components, so it rolls one of
/// the flowers' effects instead, with vanilla's durations.
pub fn food_effects(item: &str, rng: &mut Rng) -> Vec<Effect> {
    match item {
        "minecraft:golden_apple" => vec![
            Effect::new(REGENERATION, 1, 100),
            Effect::new(ABSORPTION, 0, 2400),
        ],
        "minecraft:enchanted_golden_apple" => vec![
            Effect::new(REGENERATION, 1, 100),
            Effect::new(RESISTANCE, 0, 100),
            Effect::new(FIRE_RESISTANCE, 0, 100),
            Effect::new(ABSORPTION, 3, 2400),
        ],
        // 60 % chance, four seconds.
        "minecraft:poisonous_potato" => {
            if rng.chance(0.6) {
                vec![Effect::new(POISON, 0, 80)]
            } else {
                Vec::new()
            }
        }
        "minecraft:spider_eye" => vec![Effect::new(POISON, 0, 100)],
        // Poison IV a minute, Hunger III and Nausea II fifteen seconds.
        "minecraft:pufferfish" => vec![
            Effect::new(POISON, 3, 1200),
            Effect::new(HUNGER, 2, 300),
            Effect::new(NAUSEA, 1, 300),
        ],
        // 80 % chance, thirty seconds.
        "minecraft:rotten_flesh" => {
            if rng.chance(0.8) {
                vec![Effect::new(HUNGER, 0, 600)]
            } else {
                Vec::new()
            }
        }
        "minecraft:suspicious_stew" => vec![match rng.range(0, 9) {
            0 => Effect::new(NIGHT_VISION, 0, 100),   // poppy
            1 => Effect::new(JUMP_BOOST, 0, 120),     // cornflower
            2 => Effect::new(WEAKNESS, 0, 140),       // tulip
            3 => Effect::new(BLINDNESS, 0, 160),      // azure bluet
            4 => Effect::new(POISON, 0, 240),         // lily of the valley
            5 => Effect::new(SATURATION, 0, 7),       // dandelion / blue orchid
            6 => Effect::new(FIRE_RESISTANCE, 0, 80), // allium
            7 => Effect::new(WITHER, 0, 160),         // wither rose
            _ => Effect::new(REGENERATION, 0, 160),   // oxeye daisy
        }],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Applying to a live entity.
// ---------------------------------------------------------------------------

/// The Update Mob Effect event for `e` on `entity_id`.
pub fn event(entity_id: i32, e: Effect) -> ServerEvent<'static> {
    ServerEvent::UpdateMobEffect {
        entity_id,
        effect: e.id,
        amplifier: e.amplifier,
        duration: e.duration,
        ambient: e.ambient,
        visible: e.visible,
        show_icon: e.show_icon,
    }
}

/// Give `handle` the effect `e`, telling their client and everyone watching.
pub fn give(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld, e: Effect) {
    if !handle.full() {
        return;
    }
    let shown = {
        let mut st = handle.game();
        let shown = st.effects.apply(e);
        // Applying absorption tops up to the new amount; it never refills a
        // partly spent shield, as vanilla's `setAbsorptionAmount(max(...))`.
        if shown.is_some() && e.id == ABSORPTION {
            st.absorption = st.absorption.max(st.effects.absorption_amount());
        }
        shown
    };
    if let Some(shown) = shown {
        let ev = event(handle.entity_id, shown);
        handle.emit(&ev, world);
        registry.broadcast_except(handle.entity_id, &ev, world);
        if shown.id == SPEED || shown.id == SLOWNESS {
            sync_movement(handle, registry, world);
        }
    }
}

/// Give a player several effects in one go.
pub fn give_all(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    list: &[Effect],
) {
    for e in list {
        give(handle, registry, world, *e);
    }
}

/// Remove one effect from `handle`.
pub fn remove(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld, id: u8) {
    if !handle.full() {
        return;
    }
    let removed = {
        let mut st = handle.game();
        let removed = st.effects.remove(id);
        if id == ABSORPTION {
            st.absorption = st.effects.absorption_amount();
        }
        removed
    };
    if removed.is_some() {
        let ev = ServerEvent::RemoveMobEffect {
            entity_id: handle.entity_id,
            effect: id,
        };
        handle.emit(&ev, world);
        registry.broadcast_except(handle.entity_id, &ev, world);
        if id == SPEED || id == SLOWNESS {
            sync_movement(handle, registry, world);
        }
    }
}

/// Remove every effect (milk, death, respawn).
pub fn clear(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld) {
    if !handle.full() {
        return;
    }
    let ids: Vec<u8> = handle.game().effects.iter().map(|e| e.id).collect();
    for id in ids {
        remove(handle, registry, world, id);
    }
}

/// The movement-speed modifiers the running effects put on.
pub fn movement_modifiers(effects: &Effects) -> Vec<(String, f64, i8)> {
    let mut out = Vec::new();
    if let Some(e) = effects.get(SPEED) {
        out.push((
            "minecraft:effect.speed".to_owned(),
            0.2 * (e.amplifier as f64 + 1.0),
            OP_ADD_MULTIPLIED_TOTAL,
        ));
    }
    if let Some(e) = effects.get(SLOWNESS) {
        out.push((
            "minecraft:effect.slowness".to_owned(),
            -0.15 * (e.amplifier as f64 + 1.0),
            OP_ADD_MULTIPLIED_TOTAL,
        ));
    }
    out
}

/// Tell `handle` (and watchers) the movement-speed attribute after a
/// speed/slowness change.
pub fn sync_movement(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld) {
    if !handle.full() {
        return;
    }
    let modifiers = movement_modifiers(&handle.game().effects);
    let ev = ServerEvent::UpdateAttributes {
        entity_id: handle.entity_id,
        properties: vec![(
            MOVEMENT_SPEED_ATTRIBUTE,
            PLAYER_BASE_MOVEMENT_SPEED,
            modifiers,
        )],
    };
    handle.emit(&ev, world);
    registry.broadcast_except(handle.entity_id, &ev, world);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng() -> Rng {
        Rng::new(0x1234_5678)
    }

    #[test]
    fn a_stronger_effect_replaces_and_a_weaker_one_does_not() {
        let mut e = Effects::default();
        assert_eq!(
            e.apply(Effect::new(SPEED, 0, 100)),
            Some(Effect::new(SPEED, 0, 100))
        );
        // Same amplifier, longer: replace.
        assert_eq!(
            e.apply(Effect::new(SPEED, 0, 200)),
            Some(Effect::new(SPEED, 0, 200))
        );
        // Same amplifier, shorter: keep.
        assert_eq!(e.apply(Effect::new(SPEED, 0, 50)), None);
        // Higher amplifier: replace.
        assert_eq!(
            e.apply(Effect::new(SPEED, 1, 10)),
            Some(Effect::new(SPEED, 1, 10))
        );
        assert_eq!(e.get(SPEED).unwrap().amplifier, 1);
    }

    #[test]
    fn an_infinite_effect_is_never_cut_short() {
        let mut e = Effects::default();
        e.apply(Effect::new(SPEED, 0, -1));
        assert_eq!(e.apply(Effect::new(SPEED, 0, 100)), None);
        assert_eq!(e.get(SPEED).unwrap().duration, -1);
    }

    #[test]
    fn effects_expire_and_report_it_once() {
        let mut e = Effects::default();
        e.apply(Effect::new(SPEED, 0, 2));
        let ticks: Vec<TickEvent> = (0..3).flat_map(|_| e.tick()).collect();
        assert_eq!(ticks, vec![TickEvent::Removed(SPEED)]);
        assert!(e.is_empty());
    }

    #[test]
    fn poison_stops_at_one_heart_and_wither_does_not() {
        assert_eq!(tick_damage(1.5, 1.0, false), 0.5);
        assert_eq!(tick_damage(1.0, 1.0, false), 0.0);
        assert_eq!(tick_damage(0.5, 1.0, false), 0.0);
        assert_eq!(tick_damage(0.5, 1.0, true), 1.0);
    }

    #[test]
    fn regeneration_and_poison_run_on_their_vanilla_intervals() {
        // Regeneration I heals every 50 ticks.
        let mut e = Effects::default();
        e.apply(Effect::new(REGENERATION, 0, 100));
        let mut heals = 0;
        for _ in 0..100 {
            heals += e
                .tick()
                .iter()
                .filter(|t| matches!(t, TickEvent::Heal(_)))
                .count();
        }
        assert_eq!(heals, 2, "at 100 and 50 ticks");
        // Poison IV (amplifier 3) every 3 ticks.
        let mut e = Effects::default();
        e.apply(Effect::new(POISON, 3, 9));
        let mut hits = 0;
        for _ in 0..9 {
            hits += e
                .tick()
                .iter()
                .filter(|t| matches!(t, TickEvent::Damage { .. }))
                .count();
        }
        assert_eq!(hits, 3);
    }

    #[test]
    fn wither_hurts_on_its_own_slower_interval() {
        // Wither I every 40 ticks.
        let mut e = Effects::default();
        e.apply(Effect::new(WITHER, 0, 80));
        let mut hits = 0;
        for _ in 0..80 {
            hits += e
                .tick()
                .iter()
                .filter(|t| matches!(t, TickEvent::Damage { lethal: true, .. }))
                .count();
        }
        assert_eq!(hits, 2);
    }

    #[test]
    fn fire_resistance_is_read_from_the_effect() {
        let mut e = Effects::default();
        assert!(!e.fire_resistant());
        e.apply(Effect::new(FIRE_RESISTANCE, 0, 100));
        assert!(e.fire_resistant());
    }

    #[test]
    fn hunger_burns_exhaustion_every_tick() {
        let mut e = Effects::default();
        e.apply(Effect::new(HUNGER, 1, 5));
        let ex: f32 = e
            .tick()
            .iter()
            .filter_map(|t| match t {
                TickEvent::Exhaust(x) => Some(*x),
                _ => None,
            })
            .sum();
        assert!((ex - 0.01).abs() < 1e-6, "0.005 x 2");
    }

    #[test]
    fn absorption_and_resistance_read_their_amplifiers() {
        let mut e = Effects::default();
        e.apply(Effect::new(ABSORPTION, 2, 100));
        assert_eq!(e.absorption_amount(), 12.0);
        e.apply(Effect::new(RESISTANCE, 3, 100));
        assert!((e.resistance_fraction() - 0.8).abs() < 1e-6);
        e.apply(Effect::new(RESISTANCE, 9, 100));
        assert_eq!(e.resistance_fraction(), 1.0);
    }

    #[test]
    fn speed_and_slowness_move_the_multiplier() {
        let mut e = Effects::default();
        e.apply(Effect::new(SPEED, 0, 100));
        assert!((e.movement_multiplier() - 1.2).abs() < 1e-9);
        e.apply(Effect::new(SLOWNESS, 1, 100));
        assert!((e.movement_multiplier() - (1.2 - 0.3)).abs() < 1e-9);
        let mods = movement_modifiers(&e);
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].0, "minecraft:effect.speed");
        assert_eq!(mods[1].0, "minecraft:effect.slowness");
    }

    #[test]
    fn each_named_food_carries_its_vanilla_effects() {
        let mut r = rng();
        let ids = |item: &str, r: &mut Rng| -> Vec<u8> {
            food_effects(item, r).iter().map(|e| e.id).collect()
        };
        assert_eq!(
            ids("minecraft:golden_apple", &mut r),
            vec![REGENERATION, ABSORPTION]
        );
        assert_eq!(
            ids("minecraft:enchanted_golden_apple", &mut r),
            vec![REGENERATION, RESISTANCE, FIRE_RESISTANCE, ABSORPTION]
        );
        assert_eq!(ids("minecraft:spider_eye", &mut r), vec![POISON]);
        assert_eq!(
            ids("minecraft:pufferfish", &mut r),
            vec![POISON, HUNGER, NAUSEA]
        );
        // Chance foods: a big enough sample must include both outcomes.
        let mut poison = false;
        let mut clean = false;
        for _ in 0..200 {
            let got = food_effects("minecraft:poisonous_potato", &mut r);
            match got.first().map(|e| e.id) {
                Some(POISON) => poison = true,
                _ => clean = true,
            }
        }
        assert!(poison && clean, "the 60 % roll fires both ways");
        assert!(food_effects("minecraft:suspicious_stew", &mut r).len() == 1);
        assert!(food_effects("minecraft:bread", &mut r).is_empty());
    }
}
