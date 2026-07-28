//! The player entity.
//!
//! A minimal, Data-Oriented-friendly player: an id, identity, a physics
//! [`Body`] and a game mode. Networking (`aether-server`) owns one per
//! connection and steps its body against the [`crate::World`] each tick.

use aether_core::math::Vec3;
use aether_physics::Body;

/// Minecraft game modes (values match the vanilla protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameMode {
    /// Survival.
    Survival = 0,
    /// Creative (flying, no fall damage) — the demo default.
    Creative = 1,
    /// Adventure.
    Adventure = 2,
    /// Spectator.
    Spectator = 3,
}

impl GameMode {
    /// The protocol byte for this mode.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// A player in the world.
#[derive(Debug, Clone)]
pub struct Player {
    /// Server-assigned entity id.
    pub entity_id: i32,
    /// Display name.
    pub name: String,
    /// 128-bit account UUID (offline mode derives it from the name).
    pub uuid: u128,
    /// Physics body (collision box + velocity + ground state).
    pub body: Body,
    /// Current game mode.
    pub game_mode: GameMode,
    /// Facing yaw (degrees).
    pub yaw: f32,
    /// Facing pitch (degrees).
    pub pitch: f32,
}

impl Player {
    /// Spawn a player with feet at `feet`, deriving an offline-style UUID from
    /// the name.
    pub fn spawn(entity_id: i32, name: impl Into<String>, feet: Vec3) -> Self {
        let name = name.into();
        Self {
            entity_id,
            uuid: offline_uuid(&name),
            body: Body::player(feet),
            game_mode: GameMode::Creative,
            yaw: 0.0,
            pitch: 0.0,
            name,
        }
    }

    /// The player's feet (base-centre) position.
    pub fn position(&self) -> Vec3 {
        self.body.feet()
    }

    /// UUID formatted `8-4-4-4-12` as vanilla expects on the wire.
    pub fn uuid_hyphenated(&self) -> String {
        let hex = format!("{:032x}", self.uuid);
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

/// A deterministic offline-mode UUID derived from the player name.
///
/// Vanilla uses a name-based (MD5) UUID; a real client in offline mode does not
/// verify it, so a stable FNV-based value with the version/variant nibbles set
/// is sufficient and keeps this crate dependency-free.
fn offline_uuid(name: &str) -> u128 {
    let seed = format!("OfflinePlayer:{name}");
    // Two FNV-1a passes to fill 128 bits.
    let lo = fnv1a64(seed.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let hi = fnv1a64(seed.as_bytes(), 0x84222325cbf29ce4);
    let mut v = ((hi as u128) << 64) | lo as u128;
    // Set version 3 and RFC-4122 variant so it is a well-formed UUID.
    v &= !(0xf000u128 << 64);
    v |= 0x3000u128 << 64;
    v &= !(0xc000u128 << 48);
    v |= 0x8000u128 << 48;
    v
}

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_is_stable_and_well_formed() {
        let a = Player::spawn(1, "Steve", Vec3::new(0.0, 64.0, 0.0));
        let b = Player::spawn(2, "Steve", Vec3::new(1.0, 64.0, 0.0));
        assert_eq!(a.uuid, b.uuid, "same name -> same offline uuid");
        let hy = a.uuid_hyphenated();
        assert_eq!(hy.len(), 36);
        assert_eq!(hy.as_bytes()[14], b'3', "version nibble is 3");
        assert!(matches!(hy.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }

    #[test]
    fn different_names_differ() {
        let a = Player::spawn(1, "Alex", Vec3::ZERO);
        let b = Player::spawn(1, "Steve", Vec3::ZERO);
        assert_ne!(a.uuid, b.uuid);
    }
}
