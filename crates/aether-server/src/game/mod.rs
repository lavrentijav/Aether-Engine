//! The game itself: survival rules, entities, windows and the world tick.
//!
//! Everything here runs against the server's own state and talks to clients
//! only through version-neutral [`crate::protocol::ServerEvent`]s, so the
//! rules are the same for every client; how much of it a client *sees*
//! depends on its codec (see [`crate::protocol::ProtocolCodec::full_gameplay`]).

pub mod blockticks;
pub mod brewing;
pub mod combat;
pub mod commands;
pub mod commands_player;
pub mod containers;
#[rustfmt::skip]
pub mod data;
pub mod effects;
pub mod enchantment;
pub mod entities;
pub mod fluids;
pub mod interact;
pub mod loot;
pub mod mobs;
pub mod player;
pub mod provenance;
pub mod recipes;
pub mod scheduler;
pub mod tables;
pub mod tick;
pub mod tickstore;
pub mod window;

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::players::SharedRegistry;
use crate::session::DemoWorld;

static REGISTRY: OnceLock<SharedRegistry> = OnceLock::new();
static WORLD: OnceLock<Arc<DemoWorld>> = OnceLock::new();
static AGE: AtomicU64 = AtomicU64::new(0);

/// The slowest tick since the status line last read it, in microseconds.
pub static SLOWEST_TICK_US: AtomicU64 = AtomicU64::new(0);
/// The measured ticks per second, as `f64` bits; `0` means "not measured yet".
static TPS_BITS: AtomicU64 = AtomicU64::new(0);
static TIME_OF_DAY: AtomicI64 = AtomicI64::new(1000);

/// The default game mode for players who join from now on. `u8::MAX` means
/// "not changed at runtime"; fall back to the configured server mode.
static DEFAULT_GAME_MODE: AtomicU8 = AtomicU8::new(u8::MAX);

/// A `/setworldspawn` override. `None` keeps the generated dry-column spawn.
static WORLD_SPAWN: OnceLock<Mutex<Option<WorldSpawn>>> = OnceLock::new();

/// A world spawn point set with `/setworldspawn`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldSpawn {
    /// Block X.
    pub x: i32,
    /// Block Y.
    pub y: i32,
    /// Block Z.
    pub z: i32,
    /// Spawn yaw for a new player.
    pub angle: f32,
}

/// Set the world spawn point.
pub fn set_world_spawn(x: i32, y: i32, z: i32, angle: f32) {
    let cell = WORLD_SPAWN.get_or_init(|| Mutex::new(None));
    *cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(WorldSpawn { x, y, z, angle });
}

/// The `/setworldspawn` override, if an operator has set one.
pub fn world_spawn_override() -> Option<WorldSpawn> {
    *WORLD_SPAWN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The game mode a new player joins with.
///
/// `/defaultgamemode` changes this at runtime; until then it is the mode the
/// server was started with. Existing players are never touched.
pub fn default_game_mode() -> crate::protocol::GameMode {
    match DEFAULT_GAME_MODE.load(Ordering::Relaxed) {
        0 => crate::protocol::GameMode::Survival,
        1 => crate::protocol::GameMode::Creative,
        2 => crate::protocol::GameMode::Adventure,
        3 => crate::protocol::GameMode::Spectator,
        _ => crate::server_game_mode(),
    }
}

/// Change the default game mode for players who join later.
pub fn set_default_game_mode(mode: crate::protocol::GameMode) {
    DEFAULT_GAME_MODE.store(mode.wire(), Ordering::Relaxed);
}

/// The player registry, once the game has started.
pub fn registry() -> Option<&'static SharedRegistry> {
    REGISTRY.get()
}

/// The world, once the game has started.
pub fn world() -> Option<&'static DemoWorld> {
    WORLD.get().map(|w| &**w)
}

/// Ticks since the server started.
pub fn now() -> u64 {
    AGE.load(Ordering::Relaxed)
}

/// The daylight cycle time in absolute ticks: the counter `/time set` and
/// `/time add` change, and the one `/time query day` divides by 24,000.
pub fn daylight_ticks() -> i64 {
    TIME_OF_DAY.load(Ordering::Relaxed)
}

/// Time of day, `0..24000`; 0 is sunrise, 6000 noon, 18000 midnight.
pub fn time_of_day() -> i64 {
    daylight_ticks().rem_euclid(24_000)
}

/// The current day number of the daylight cycle.
pub fn day() -> i64 {
    daylight_ticks().div_euclid(24_000)
}

/// Set the absolute daylight cycle time. The stored value is kept whole so
/// the day count survives; [`time_of_day`] takes the modulo.
pub fn set_time_of_day(t: i64) {
    TIME_OF_DAY.store(t, Ordering::Relaxed);
}

/// The measured ticks per second, smoothed; the ideal 20 before the first
/// measurement. Read by the status line and the `%tps%` placeholder.
pub fn tps() -> f64 {
    let bits = TPS_BITS.load(Ordering::Relaxed);
    if bits == 0 {
        20.0
    } else {
        f64::from_bits(bits)
    }
}

/// Fold one measured tick rate into the smoothed value.
pub fn set_tps(instant: f64) {
    let smoothed = tps() * 0.9 + instant * 0.1;
    TPS_BITS.store(smoothed.to_bits(), Ordering::Relaxed);
}

/// Whether it is night — when monsters spawn under the open sky.
pub fn is_night() -> bool {
    let t = time_of_day();
    (13000..23000).contains(&t)
}

/// The world-time event to send a client.
pub fn time_event() -> crate::protocol::ServerEvent<'static> {
    crate::protocol::ServerEvent::Time {
        age: now() as i64,
        // Vanilla sends the absolute daylight time; the client takes the
        // modulo itself.
        time_of_day: daylight_ticks(),
    }
}

/// The block's `minecraft:` name without its state properties —
/// `minecraft:grass_block`, not `minecraft:grass_block[snowy=false]` — which
/// is what every game table is keyed by.
pub fn block_name(world: &DemoWorld, id: aether_world::BlockStateId) -> Option<String> {
    let mut n = world.block_name_of(id)?;
    if let Some(i) = n.find('[') {
        n.truncate(i);
    }
    Some(n)
}
