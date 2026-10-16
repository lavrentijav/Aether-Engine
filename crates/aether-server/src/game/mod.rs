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
pub mod containers;
#[rustfmt::skip]
pub mod data;
pub mod effects;
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

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

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

/// Time of day, `0..24000`; 0 is sunrise, 6000 noon, 18000 midnight.
pub fn time_of_day() -> i64 {
    TIME_OF_DAY.load(Ordering::Relaxed)
}

/// Set the time of day.
pub fn set_time_of_day(t: i64) {
    TIME_OF_DAY.store(t.rem_euclid(24000), Ordering::Relaxed);
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
        time_of_day: time_of_day(),
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
