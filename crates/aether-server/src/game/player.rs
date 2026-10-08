//! One player's survival state: health, hunger, the window they have open
//! and the stack on their cursor.

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
    /// A chest at a position.
    Chest { pos: (i32, i32, i32) },
    /// A furnace at a position.
    Furnace { pos: (i32, i32, i32) },
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
    /// The highest point since last on the ground, for fall damage.
    pub fall_peak: Option<f64>,
    pub hurt_cooldown: u32,
    /// Ticks towards the next regeneration or starvation step.
    pub food_timer: u32,
    /// When eating started (tick) and with which hand.
    pub eating: Option<(u64, u8)>,
    /// When drawing a bow started.
    pub drawing: Option<u64>,
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
            fall_peak: None,
            hurt_cooldown: 0,
            food_timer: 0,
            eating: None,
            drawing: None,
            digging: None,
            last_attack: 0,
            sneaking: false,
            sprinting: false,
            window: None,
            window_counter: 1,
            state_id: 1,
            cursor: None,
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
}

const SAVE_VERSION: u8 = 1;

/// KV key for a player's saved state.
pub fn key(uuid: u128) -> [u8; 17] {
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
    out
}

pub fn decode(b: &[u8]) -> Option<Saved> {
    if b.len() != 1 + 6 * 8 + 4 * 4 + 4 + 4 + 1 || b[0] != SAVE_VERSION {
        return None;
    }
    let f64_at = |i: usize| f64::from_be_bytes(b[1 + i * 8..9 + i * 8].try_into().unwrap());
    let base = 1 + 48;
    let f32_at =
        |i: usize| f32::from_be_bytes(b[base + i * 4..base + 4 + i * 4].try_into().unwrap());
    let i32_at = |o: usize| i32::from_be_bytes(b[o..o + 4].try_into().unwrap());
    let mode = match b[b.len() - 1] {
        1 => GameMode::Creative,
        _ => GameMode::Survival,
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
        };
        assert_eq!(decode(&encode(&s)), Some(s));
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
}
