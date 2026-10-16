//! The live-ops API the web admin serves.
//!
//! The web admin is a native plugin (`plugins/webadmin`), and the core must
//! never name a plugin. So the *shape* of the data crosses the boundary in one
//! small crate both sides depend on: `aether-server` implements [`OpsService`]
//! over its live state and publishes it in the plugin host's service registry,
//! and the plugin resolves and reads it. Nothing here touches the world; every
//! method is a read that returns owned, serialisable views.
//!
//! The service is deliberately read-only: WEB1 ships no world writes from the
//! web, and this trait has no method that could make one.

use serde::Serialize;
use std::sync::Arc;

/// The service name the server publishes and the plugin resolves
/// (`ops@0.1` in the manifest's `requires`).
pub const SERVICE: &str = "ops";
/// The service version. Bump on a breaking shape change.
pub const VERSION: &str = "0.1";

/// The dashboard's headline numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dashboard {
    /// Ticks per second, measured over a short window and capped at twenty.
    pub tps: f64,
    /// Mean milliseconds per server tick over the same window.
    pub mspt: f64,
    /// The slowest tick in the window, in milliseconds.
    pub mspt_max: f64,
    /// Connected players.
    pub players_online: usize,
    /// `[server] max_players`.
    pub players_max: usize,
    /// Ticks since the server started.
    pub ticks: u64,
    /// Seconds since the game loop started.
    pub uptime_secs: u64,
    /// Resident set size of the process, in mebibytes (0 where it cannot be read).
    pub memory_used_mib: u64,
    /// Machine memory, in mebibytes (0 where it cannot be read).
    pub memory_total_mib: u64,
    /// Columns the server has sent at least once since startup, summed over
    /// players (a coarse "chunk traffic" figure).
    pub chunk_sends: u64,
}

/// One sampled region's recent activity and derived cost.
///
/// A region is a 32×32-chunk square (vanilla's region-file grid), keyed by
/// `(cx >> 5, cz >> 5)`; chunk coordinates are `(cx, cz)`. The numbers are a
/// cheap *sample*, not a continuous profile: each field is the activity
/// observed in the most recent sampling window, and `cost` is a weighted
/// score used only for ranking.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegionStat {
    /// Region index: `cx >> 5`.
    pub rx: i32,
    /// Region index: `cz >> 5`.
    pub rz: i32,
    /// Non-player entities standing in the region at the sample.
    pub entities: u32,
    /// Loaded block entities (containers, furnaces, …) in the region.
    pub block_entities: u32,
    /// Block ticks that ran in the region during the window (fluids, growth,
    /// falling blocks).
    pub block_updates: u32,
    /// Redstone updates. Zero until the redstone engine is wired.
    pub redstone_updates: u32,
    /// Columns sent to clients whose centre is in the region, over the window.
    pub chunk_sends: u32,
    /// `entities + block_entities*2 + block_updates + redstone_updates +
    /// chunk_sends/4`, floored. Ranking only.
    pub cost: f64,
    /// How many sampling windows this region was seen in (i.e. it was active).
    pub samples: u64,
}

/// One player's summary row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayerEntry {
    /// UUID as 32 lowercase hex digits.
    pub uuid: String,
    /// Last known name.
    pub name: String,
    /// Whether the player is connected now.
    pub online: bool,
    /// The address of the current session, if online.
    pub ip: Option<String>,
    /// Unix seconds of the first join ever seen.
    pub first_seen: i64,
    /// Unix seconds of the most recent join.
    pub last_seen: i64,
    /// Accumulated connected seconds (finished sessions plus the current one).
    pub playtime_secs: u64,
}

/// A player's full profile, online or off.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayerProfile {
    /// The summary row.
    #[serde(flatten)]
    pub entry: PlayerEntry,
    /// Every address seen for this player, most recent first.
    pub ips: Vec<String>,
    /// Seconds in the current session (`0` when offline).
    pub session_secs: u64,
    /// Current position `(x, y, z)`, when online.
    pub position: Option<(f64, f64, f64)>,
    /// Health, when online.
    pub health: Option<f32>,
    /// Food level, when online.
    pub food: Option<i32>,
    /// Game mode name (`creative`/`survival`/…), when online.
    pub game_mode: Option<String>,
}

/// One inventory slot as the web renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemView {
    /// Window slot index.
    pub slot: usize,
    /// Canonical item name, e.g. `minecraft:diamond`.
    pub item: String,
    /// Stack size.
    pub count: u32,
    /// Wear (`0` for an undamaged or non-damaging item).
    pub damage: u16,
    /// Potion registry index, for a potion.
    pub potion: Option<u16>,
}

/// A player's inventory, split into the parts the web shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InventoryView {
    pub uuid: String,
    pub name: String,
    pub online: bool,
    /// The selected hotbar slot, `0..9`.
    pub held_slot: u8,
    /// Main inventory and hotbar, empty slots omitted.
    pub slots: Vec<ItemView>,
    /// Armour slots (helmet, chest, legs, boots), empty entries included as
    /// `slot`-only rows so the web can keep the layout.
    pub armor: Vec<ItemView>,
    /// Ender chest. Empty until the engine has one.
    pub ender: Vec<ItemView>,
}

/// One thing drawn on the map overlay: a player or a non-player entity.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapMark {
    /// A stable category: `player`, `mob`, or the entity type name.
    pub kind: String,
    /// A human label (the player name, or the entity type).
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Live overlay of everything currently in the world: online players and
/// non-player entities. Empty when nothing is running.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MapOverlay {
    pub players: Vec<MapMark>,
    pub entities: Vec<MapMark>,
}

/// Live, read-only live-ops state.
///
/// Every method is cheap and must not block the tick: implementations read
/// atomics and snapshots.
pub trait OpsService: Send + Sync {
    /// The dashboard numbers.
    fn dashboard(&self) -> Dashboard;

    /// Up to `limit` regions with the highest `cost`.
    fn regions(&self, limit: usize) -> Vec<RegionStat>;

    /// Every player ever seen, online first then by name.
    fn players(&self) -> Vec<PlayerEntry>;

    /// One player by UUID hex or name (case-insensitive), if known.
    fn profile(&self, id: &str) -> Option<PlayerProfile>;

    /// One player's inventory by UUID hex or name, live when online and from
    /// the saved store when off. `None` if the player is unknown.
    fn inventory(&self, id: &str) -> Option<InventoryView>;

    /// Live map overlay: online players and non-player entities with their
    /// positions. A snapshot; the default is empty.
    fn overlay(&self) -> MapOverlay {
        MapOverlay::default()
    }

    /// The display name for an actor uuid, from the single player registry
    /// (the online list, else the persisted profile store). `None` when the
    /// uuid is unknown, so the caller falls back to the hex form.
    fn actor_name(&self, _uuid: u128) -> Option<String> {
        None
    }
}

/// A sized handle to an [`OpsService`], so the plugin host's `Arc<T>`-shaped
/// service registry can store and hand out a trait object: one side registers
/// `Arc<OpsHandle>`, the other resolves `OpsHandle` and calls through it.
#[derive(Clone)]
pub struct OpsHandle(pub Arc<dyn OpsService>);

impl OpsHandle {
    /// The service behind this handle.
    pub fn get(&self) -> &dyn OpsService {
        &*self.0
    }
}

impl std::fmt::Debug for OpsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpsHandle")
    }
}
