//! The game itself: survival rules, entities, windows and the world tick.
//!
//! Everything here runs against the server's own state and talks to clients
//! only through version-neutral [`crate::protocol::ServerEvent`]s, so the
//! rules are the same for every client; how much of it a client *sees*
//! depends on its codec (see [`crate::protocol::ProtocolCodec::full_gameplay`]).

pub mod combat;
pub mod commands;
pub mod containers;
#[rustfmt::skip]
pub mod data;
pub mod entities;
pub mod interact;
pub mod mobs;
pub mod player;
pub mod tables;
pub mod tick;
pub mod window;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::players::SharedRegistry;
use crate::session::DemoWorld;

static REGISTRY: OnceLock<SharedRegistry> = OnceLock::new();
static WORLD: OnceLock<Arc<DemoWorld>> = OnceLock::new();
static AGE: AtomicU64 = AtomicU64::new(0);
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
