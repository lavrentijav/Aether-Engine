//! One player's survival state: health, hunger, the window they have open
//! and the stack on their cursor.

use super::effects::{Effect, Effects};
use crate::inventory::Stack;
use crate::protocol::GameMode;

/// Ticks a hit leaves a player immune to the next, vanilla's half second.
pub const HURT_COOLDOWN: u32 = 10;
/// Ticks eating takes.
pub const EAT_TICKS: u64 = 32;

/// A game window the player has open.
#[derive(Debug, Clone)]
pub struct OpenWindow {
    /// The id the client knows it by, `2..=100`; `1` is the stash and `0` the
    /// player's own inventory.
    pub id: u8,
    pub kind: WindowKind,
}

/// What kind of window, and what it is attached to.
#[derive(Debug, Clone)]
pub enum WindowKind {
    /// A crafting table. The grid belongs to the window, not the table, and
    /// is handed back when it closes.
    Crafting { grid: Vec<Option<Stack>> },
    /// A chest at a position — a double chest's left half (slots 0–26) and,
    /// when doubled, its right.
    Chest {
        left: (i32, i32, i32),
        right: Option<(i32, i32, i32)>,
    },
    /// A furnace at a position.
    Furnace { pos: (i32, i32, i32) },
    /// A hopper at a position.
    Hopper { pos: (i32, i32, i32) },
    /// A dispenser or dropper at a position.
    Dispenser { pos: (i32, i32, i32) },
    /// A brewing stand at a position.
    BrewingStand { pos: (i32, i32, i32) },
}

/// A drag in progress (click mode 5).
#[derive(Debug, Clone, Default)]
pub struct Drag {
    /// 0 left (split evenly), 1 right (one each), 2 middle (creative fill).
    pub kind: u8,
    pub slots: Vec<i16>,
}

/// Survival state for one player.
#[derive(Debug, Clone)]
pub struct PlayerState {
    pub mode: GameMode,
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    pub dead: bool,
    /// The status effects running on this player.
    pub effects: Effects,
    /// Absorption hearts left, spent before health. Driven by the absorption
    /// effect; the client draws its own from that effect's icon.
    pub absorption: f32,
    /// The highest point since last on the ground, for fall damage.
    pub fall_peak: Option<f64>,
    pub hurt_cooldown: u32,
    /// Ticks towards the next regeneration or starvation step.
    pub food_timer: u32,
    /// When eating started (tick) and with which hand.
    pub eating: Option<(u64, u8)>,
    /// When drawing a bow started.
    pub drawing: Option<u64>,
    /// When charging a spear started, for its right-click lunge.
    pub charging: Option<u64>,
    /// The block being mined and the tick it started.
    pub digging: Option<((i32, i32, i32), u64)>,
    /// The tick of the last attack, for the 1.9 cooldown.
    pub last_attack: u64,
    pub sneaking: bool,
    pub sprinting: bool,
    pub window: Option<OpenWindow>,
    /// The last window id handed out.
    pub window_counter: u8,
    /// Bumped on every window update the server sends.
    pub state_id: i32,
    pub cursor: Option<Stack>,
    /// The item a creative client is carrying between slots, shadowed from
    /// its slot reports so a substituted item moved through the cursor keeps
    /// its real identity. See `session::record_slot_change`.
    pub creative_carry: Option<aether_world::ItemTypeId>,
    pub drag: Option<Drag>,
    pub xp_total: i32,
    /// Where they come back after dying.
    pub spawn: (f64, f64, f64),
    /// Ticks spent in the void, lava or fire, for damage pacing.
    pub env_timer: u32,
    /// Health, food and saturation as last sent, so changes are noticed.
    pub sent_health: (f32, i32, f32),
}

impl PlayerState {
    pub fn new(mode: GameMode) -> Self {
        Self {
            mode,
            health: 20.0,
            food: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            dead: false,
            effects: Effects::default(),
            absorption: 0.0,
            fall_peak: None,
            hurt_cooldown: 0,
            food_timer: 0,
            eating: None,
            drawing: None,
            charging: None,
            digging: None,
            last_attack: 0,
            sneaking: false,
            sprinting: false,
            window: None,
            window_counter: 1,
            state_id: 1,
            cursor: None,
            creative_carry: None,
            drag: None,
            xp_total: 0,
            spawn: (8.5, 80.0, 8.5),
            env_timer: 0,
            sent_health: (-1.0, -1, -1.0),
        }
    }

    /// Whether the server is in charge of this player's survival.
    pub fn survival(&self) -> bool {
        self.mode == GameMode::Survival
    }

    /// Forget the item shadowed on the creative cursor.
    ///
    /// Called wherever the cursor is emptied behind the client's back — death
    /// drops the inventory, respawn and closing a window hand the cursor back —
    /// so a stand-in conjured after the item is gone cannot be resolved to the
    /// real item it once stood for. See `session::record_slot_change`.
    pub fn clear_creative_shadow(&mut self) {
        self.creative_carry = None;
    }

    /// A fresh window id.
    pub fn next_window_id(&mut self) -> u8 {
        self.window_counter = if self.window_counter >= 100 {
            2
        } else {
            self.window_counter + 1
        };
        self.window_counter
    }

    /// Bump and return the window state id.
    pub fn next_state(&mut self) -> i32 {
        self.state_id = self.state_id.wrapping_add(1) & 0x7FFF;
        self.state_id
    }

    /// Spend `amount` of exhaustion, vanilla's food model.
    pub fn exhaust(&mut self, amount: f32) {
        if !self.survival() {
            return;
        }
        self.exhaustion += amount;
        while self.exhaustion >= 4.0 {
            self.exhaustion -= 4.0;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else {
                self.food = (self.food - 1).max(0);
            }
        }
    }

    /// Level and progress for a total experience count, vanilla's curve.
    pub fn xp_level(&self) -> (i32, f32) {
        let mut level = 0;
        let mut left = self.xp_total;
        loop {
            let need = if level >= 30 {
                112 + (level - 30) * 9
            } else if level >= 15 {
                37 + (level - 15) * 5
            } else {
                7 + level * 2
            };
            if left < need {
                return (level, left as f32 / need as f32);
            }
            left -= need;
            level += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence: position, health, hunger, experience and game mode.
// ---------------------------------------------------------------------------

/// What is saved about a player besides their inventory.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub pos: (f64, f64, f64),
    pub yaw: f32,
    pub pitch: f32,
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub xp_total: i32,
    pub mode: GameMode,
    pub spawn: (f64, f64, f64),
    /// Status effects still running when the player left.
    pub effects: Vec<Effect>,
}

/// The version 1 layout, without effects. Still read, so a player who last
/// logged in before effects existed is not locked out of their save.
const LEGACY_LEN: usize = 1 + 6 * 8 + 4 * 4 + 4 + 4 + 1;

/// 2 added the active effects.
const SAVE_VERSION: u8 = 2;

/// KV key for a player's saved state.
///
/// Seventeen bytes under the reserved `D` prefix (see
/// [`aether_api::keys`]). It used to be `P`, which the journal's actor index
/// also claims: `/lookup player:` scans `P` + a 16-byte actor, so a saved
/// player and an actor index entry shared a prefix. [`legacy_key`] still reads
/// the old key so a player who saved before the rename keeps their state.
pub fn key(uuid: u128) -> [u8; 17] {
    let mut k = [0u8; 17];
    k[0] = aether_api::keys::PLAYER_SAVE[0];
    k[1..].copy_from_slice(&uuid.to_be_bytes());
    k
}

/// The pre-rename key (`P`). Read fallback only; nothing is ever written here.
pub fn legacy_key(uuid: u128) -> [u8; 17] {
    let mut k = [0u8; 17];
    k[0] = b'P';
    k[1..].copy_from_slice(&uuid.to_be_bytes());
    k
}

pub fn encode(s: &Saved) -> Vec<u8> {
    let mut out = vec![SAVE_VERSION];
    for v in [s.pos.0, s.pos.1, s.pos.2, s.spawn.0, s.spawn.1, s.spawn.2] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    for v in [s.yaw, s.pitch, s.health, s.saturation] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&s.food.to_be_bytes());
    out.extend_from_slice(&s.xp_total.to_be_bytes());
    out.push(s.mode.wire());
    out.push(s.effects.len().min(255) as u8);
    for e in s.effects.iter().take(255) {
        out.push(e.id);
        out.push(e.amplifier);
        out.extend_from_slice(&e.duration.to_be_bytes());
        let flags = (e.ambient as u8) | ((e.visible as u8) << 1) | ((e.show_icon as u8) << 2);
        out.push(flags);
    }
    out
}

pub fn decode(b: &[u8]) -> Option<Saved> {
    if b.len() < LEGACY_LEN {
        return None;
    }
    match b[0] {
        1 | 2 => {}
        _ => return None,
    }
    let f64_at = |i: usize| f64::from_be_bytes(b[1 + i * 8..9 + i * 8].try_into().unwrap());
    let base = 1 + 48;
    let f32_at =
        |i: usize| f32::from_be_bytes(b[base + i * 4..base + 4 + i * 4].try_into().unwrap());
    let i32_at = |o: usize| i32::from_be_bytes(b[o..o + 4].try_into().unwrap());
    let mode = match b[LEGACY_LEN - 1] {
        1 => GameMode::Creative,
        _ => GameMode::Survival,
    };
    let effects = if b[0] == 2 {
        let mut at = LEGACY_LEN;
        let n = *b.get(at)? as usize;
        at += 1;
        let mut list = Vec::with_capacity(n);
        for _ in 0..n {
            let id = *b.get(at)?;
            let amplifier = *b.get(at + 1)?;
            let duration = i32::from_be_bytes(b.get(at + 2..at + 6)?.try_into().ok()?);
            let flags = *b.get(at + 6)?;
            at += 7;
            list.push(Effect {
                id,
                amplifier,
                duration,
                ambient: flags & 1 != 0,
                visible: flags & 2 != 0,
                show_icon: flags & 4 != 0,
            });
        }
        if at != b.len() {
            return None;
        }
        list
    } else {
        if b.len() != LEGACY_LEN {
            return None;
        }
        Vec::new()
    };
    Some(Saved {
        pos: (f64_at(0), f64_at(1), f64_at(2)),
        spawn: (f64_at(3), f64_at(4), f64_at(5)),
        yaw: f32_at(0),
        pitch: f32_at(1),
        health: f32_at(2),
        saturation: f32_at(3),
        food: i32_at(base + 16),
        xp_total: i32_at(base + 20),
        mode,
        effects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_state_round_trips() {
        let s = Saved {
            pos: (1.5, 64.0, -3.25),
            yaw: 90.0,
            pitch: -10.0,
            health: 13.5,
            food: 17,
            saturation: 2.5,
            xp_total: 321,
            mode: GameMode::Creative,
            spawn: (8.5, 70.0, 8.5),
            effects: vec![
                Effect::new(crate::game::effects::SPEED, 1, 200),
                Effect::new(crate::game::effects::ABSORPTION, 0, 2400).hidden(),
            ],
        };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn a_version_1_save_still_loads_without_effects() {
        let legacy = {
            let s = Saved {
                pos: (0.0, 64.0, 0.0),
                yaw: 0.0,
                pitch: 0.0,
                health: 20.0,
                food: 20,
                saturation: 5.0,
                xp_total: 0,
                mode: GameMode::Survival,
                spawn: (8.5, 80.0, 8.5),
                effects: Vec::new(),
            };
            let mut out = vec![1u8];
            out.extend_from_slice(&encode(&s)[1..LEGACY_LEN]);
            out
        };
        let decoded = decode(&legacy).expect("a version 1 save loads");
        assert!(decoded.effects.is_empty());
        assert_eq!(decoded.food, 20);
    }

    #[test]
    fn exhaustion_eats_saturation_then_food() {
        let mut p = PlayerState::new(GameMode::Survival);
        p.saturation = 1.0;
        p.exhaust(4.0);
        assert_eq!((p.food, p.saturation), (20, 0.0));
        p.exhaust(4.0);
        assert_eq!(p.food, 19);
    }

    #[test]
    fn levels_follow_the_vanilla_curve() {
        let mut p = PlayerState::new(GameMode::Survival);
        p.xp_total = 7;
        assert_eq!(p.xp_level().0, 1);
        p.xp_total = 352; // level 15 starts at 315 + 37 = 352 total → level 16
        assert_eq!(p.xp_level().0, 16);
    }

    #[test]
    fn death_clears_the_creative_shadow_so_a_stand_in_is_not_conjured() {
        // F3: the shadow is only set by a slot report, so death — which drops
        // the whole inventory — must clear it, or a stand-in reported after
        // respawn would resolve to the real item that no longer exists.
        // `combat::die`, `combat::respawn` and `game::window::close` all call
        // `clear_creative_shadow`, which is the step this test drives.
        use crate::protocol::compat::items::{table_for, Resolution};
        let spear = aether_world::ItemTypeId::vanilla("minecraft:iron_spear").unwrap();
        let trident = aether_world::ItemTypeId::vanilla("minecraft:trident").unwrap();
        let table = table_for(767).unwrap(); // 1.21.1
        let shown = table.engine(table.wire(spear).unwrap().id).unwrap();
        assert_eq!(shown, trident, "the spear is shown as a trident");

        let mut p = PlayerState::new(GameMode::Creative);
        // Pick the spear up: the shadow is set.
        let (_, carry) = table.resolve_change(None, Some(spear), p.creative_carry);
        p.creative_carry = carry;
        assert_eq!(p.creative_carry, Some(spear));
        // Die: the inventory is dropped and the shadow with it.
        p.clear_creative_shadow();
        assert_eq!(p.creative_carry, None);
        // Respawn, report a trident into an empty slot: a genuine creation,
        // not the spear conjured from the dropped one.
        let (res, _) = table.resolve_change(Some(shown), None, p.creative_carry);
        assert_eq!(res, Resolution::Apply(Some(trident)));
    }
}
