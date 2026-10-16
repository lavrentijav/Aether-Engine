//! Live-ops state for the web admin (`plugins/webadmin`).
//!
//! This module is the **core** half of the admin interface: it samples cheap
//! counters (TPS/MSPT from the tick window, per-region activity from the
//! entity, container and scheduled-tick stores), keeps a small per-player
//! profile store (first/last seen, IPs, playtime), and implements
//! [`aether_ops::OpsService`] over the live world. The `plugins/webadmin`
//! plugin resolves that service through the plugin host and serves it as HTML.
//! No HTTP, no HTML and no plugin dependency lives here — the core builds and
//! runs with the plugin absent.
//!
//! Everything the service returns is read-only and owned, by contract.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aether_ops::{
    Dashboard, InventoryView, ItemView, MapMark, MapOverlay, OpsService, PlayerEntry,
    PlayerProfile, RegionStat,
};
use aether_world::KvBackend;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;

/// Ticks kept in the MSPT window (five seconds at twenty TPS).
const TICK_WINDOW: usize = 100;
/// Chunks per region side, as a shift (vanilla's region-file grid: 32×32).
const REGION_CHUNK_SHIFT: i32 = 5;
/// Blocks per chunk side, as a shift.
const CHUNK_BLOCK_SHIFT: i32 = 4;

/// The region a **block** coordinate falls in.
fn region_of_block(x: i32, z: i32) -> (i32, i32) {
    (
        x >> (CHUNK_BLOCK_SHIFT + REGION_CHUNK_SHIFT),
        z >> (CHUNK_BLOCK_SHIFT + REGION_CHUNK_SHIFT),
    )
}

/// The region a **chunk** coordinate falls in.
fn region_of_chunk(cx: i32, cz: i32) -> (i32, i32) {
    (cx >> REGION_CHUNK_SHIFT, cz >> REGION_CHUNK_SHIFT)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

struct Metrics {
    start: Instant,
    ticks: Mutex<VecDeque<u64>>,
    regions: Mutex<BTreeMap<(i32, i32), RegionStat>>,
    /// Column sends since the last sample, by region.
    chunk_sends: Mutex<HashMap<(i32, i32), u32>>,
    chunk_sends_total: AtomicU64,
    /// Session start per online player, keyed by uuid, for playtime.
    sessions: Mutex<HashMap<u128, Instant>>,
}

fn metrics() -> &'static Metrics {
    static M: OnceLock<Metrics> = OnceLock::new();
    M.get_or_init(|| Metrics {
        start: Instant::now(),
        ticks: Mutex::new(VecDeque::with_capacity(TICK_WINDOW)),
        regions: Mutex::new(BTreeMap::new()),
        chunk_sends: Mutex::new(HashMap::new()),
        chunk_sends_total: AtomicU64::new(0),
        sessions: Mutex::new(HashMap::new()),
    })
}

/// Record one server tick's duration. Called from the tick loop.
pub fn record_tick(micros: u64) {
    let m = metrics();
    let mut w = m.ticks.lock().unwrap();
    if w.len() >= TICK_WINDOW {
        w.pop_front();
    }
    w.push_back(micros);
}

/// Note that a column was sent to a client's centre at `(cx, cz)`.
///
/// Called wherever a `ChunkColumn` reaches a socket — cheap (one lock, once
/// per column) and never per block.
pub fn note_chunk_send(cx: i32, cz: i32) {
    let m = metrics();
    m.chunk_sends_total.fetch_add(1, Ordering::Relaxed);
    *m.chunk_sends
        .lock()
        .unwrap()
        .entry(region_of_chunk(cx, cz))
        .or_insert(0) += 1;
}

/// Mean and slowest tick in the window, in milliseconds.
fn tick_window() -> (f64, f64) {
    let w = metrics().ticks.lock().unwrap();
    if w.is_empty() {
        return (0.0, 0.0);
    }
    let sum: u64 = w.iter().sum();
    let max = *w.iter().max().unwrap();
    ((sum as f64 / w.len() as f64) / 1000.0, max as f64 / 1000.0)
}

fn memory_mib() -> (u64, u64) {
    #[cfg(target_os = "linux")]
    {
        let page = 4096u64;
        let used = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|pages| pages * page / (1024 * 1024))
            .unwrap_or(0);
        let total = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            })
            .map(|kb| kb / 1024)
            .unwrap_or(0);
        (used, total)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (0, 0)
    }
}

/// Sample per-region activity into the rolling heat map.
///
/// Runs on its **own** thread (see [`spawn_sampler`]), never the tick thread,
/// and reads each source through a `for_each` hook that holds the lock for the
/// walk instead of building a `Vec` per store — so a big scheduled-tick queue
/// costs no allocation and cannot stall a tick.
pub fn sample() {
    fn add(
        fresh: &mut HashMap<(i32, i32), RegionStat>,
        key: (i32, i32),
        f: impl FnOnce(&mut RegionStat),
    ) {
        let entry = fresh.entry(key).or_insert_with(|| RegionStat {
            rx: key.0,
            rz: key.1,
            entities: 0,
            block_entities: 0,
            block_updates: 0,
            redstone_updates: 0,
            chunk_sends: 0,
            cost: 0.0,
            samples: 0,
        });
        f(entry);
    }

    let mut fresh: HashMap<(i32, i32), RegionStat> = HashMap::new();
    crate::game::entities::for_each_named(&mut |_kind, p| {
        add(
            &mut fresh,
            region_of_block(p.x.floor() as i32, p.z.floor() as i32),
            |r| r.entities += 1,
        );
    });
    crate::game::containers::for_each_position(&mut |p| {
        add(&mut fresh, region_of_block(p.0, p.2), |r| {
            r.block_entities += 1
        });
    });
    crate::game::blockticks::for_each_pending(&mut |p| {
        add(&mut fresh, region_of_block(p.0, p.2), |r| {
            r.block_updates += 1
        });
    });
    crate::game::fluids::for_each_pending(&mut |p| {
        add(&mut fresh, region_of_block(p.0, p.2), |r| {
            r.block_updates += 1
        });
    });
    let sends: HashMap<(i32, i32), u32> =
        std::mem::take(&mut *metrics().chunk_sends.lock().unwrap());
    for (key, n) in sends {
        add(&mut fresh, key, |r| r.chunk_sends += n);
    }

    let m = metrics();
    let mut regions = m.regions.lock().unwrap();
    // Decay regions not touched this window: half their cost, drop at zero.
    regions.retain(|k, r| {
        if fresh.contains_key(k) {
            true
        } else {
            r.cost *= 0.5;
            r.samples = r.samples.saturating_sub(1);
            r.cost > 0.5 && r.samples > 0
        }
    });
    for (key, mut r) in fresh {
        r.cost = r.entities as f64
            + r.block_entities as f64 * 2.0
            + r.block_updates as f64
            + r.redstone_updates as f64
            + r.chunk_sends as f64 / 4.0;
        match regions.get(&key) {
            // Merge over the decaying previous sample: the rolling window is
            // a short moving average, not a single instantaneous spike.
            Some(prev) => {
                r.samples = prev.samples + 1;
                r.cost = (r.cost + prev.cost) / 2.0;
            }
            None => r.samples = 1,
        }
        regions.insert(key, r);
    }
}

/// Start the background sampler: one cheap tick per second, off the tick
/// thread so a large pending-tick queue can never delay the game loop.
pub fn spawn_sampler() {
    std::thread::Builder::new()
        .name("web-sampler".into())
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            sample();
        })
        .expect("failed to start the live-ops sampler");
}

// ---------------------------------------------------------------------------
// Player profiles
// ---------------------------------------------------------------------------

/// The persisted per-player profile (a JSON blob under `WB` + uuid).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredProfile {
    name: String,
    first_seen: i64,
    last_seen: i64,
    playtime_secs: u64,
    ips: Vec<String>,
}

/// The profile prefix, claimed in `aether_api::keys` (`WEB_PROFILE`). Repeated
/// here as a local constant so this module does not depend on that crate's
/// build order in the shared target directory.
const PROFILE_PREFIX: &[u8] = b"WB";

fn profile_key(uuid: u128) -> [u8; 18] {
    let mut k = [0u8; 18];
    k[0] = PROFILE_PREFIX[0];
    k[1] = PROFILE_PREFIX[1];
    k[2..].copy_from_slice(&uuid.to_be_bytes());
    k
}

fn load_profile(world: &DemoWorld, uuid: u128) -> Option<StoredProfile> {
    let blob = world.get_meta(&profile_key(uuid)).ok().flatten()?;
    serde_json::from_slice(&blob).ok()
}

fn save_profile(world: &DemoWorld, uuid: u128, p: &StoredProfile) {
    if let Ok(blob) = serde_json::to_vec(p) {
        let _ = world.put_meta(&profile_key(uuid), &blob);
    }
}

fn all_profiles(world: &DemoWorld) -> Vec<(u128, StoredProfile)> {
    let Ok(rows) = world.journal().backend().scan_prefix(PROFILE_PREFIX) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|(k, v)| {
            if k.len() != 18 {
                return None;
            }
            let uuid = u128::from_be_bytes(k[2..].try_into().ok()?);
            let p: StoredProfile = serde_json::from_slice(&v).ok()?;
            Some((uuid, p))
        })
        .collect()
}

/// Record a join: update first/last seen, remember the address, start the
/// session clock and persist.
pub fn on_join(handle: &PlayerHandle, world: &DemoWorld) {
    let now = unix_now();
    let mut p = load_profile(world, handle.uuid).unwrap_or_default();
    if p.first_seen == 0 {
        p.first_seen = now;
    }
    p.last_seen = now;
    p.name = handle.name.clone();
    if let Some(ip) = handle.ip {
        let ip = ip.to_string();
        if p.ips.first() != Some(&ip) {
            p.ips.insert(0, ip);
            p.ips.truncate(16);
        }
    }
    save_profile(world, handle.uuid, &p);
    metrics()
        .sessions
        .lock()
        .unwrap()
        .insert(handle.uuid, Instant::now());
}

/// Record a leave: fold the finished session into playtime and persist.
pub fn on_leave(handle: &PlayerHandle, world: &DemoWorld) {
    let elapsed = metrics()
        .sessions
        .lock()
        .unwrap()
        .remove(&handle.uuid)
        .map(|start| start.elapsed().as_secs())
        .unwrap_or(0);
    let mut p = load_profile(world, handle.uuid).unwrap_or_default();
    p.name = handle.name.clone();
    p.last_seen = unix_now();
    p.playtime_secs = p.playtime_secs.saturating_add(elapsed);
    save_profile(world, handle.uuid, &p);
}

fn uuid_hex(uuid: u128) -> String {
    format!("{uuid:032x}")
}

fn parse_uuid(id: &str) -> Option<u128> {
    if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        u128::from_str_radix(id, 16).ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The server's [`OpsService`] implementation, registered into the plugin
/// host's service registry as `ops@0.1`.
pub struct ServerOps {
    world: Arc<DemoWorld>,
    registry: SharedRegistry,
    config: Arc<Config>,
}

impl ServerOps {
    /// An implementation over the live server state.
    pub fn new(world: Arc<DemoWorld>, registry: SharedRegistry, config: Arc<Config>) -> ServerOps {
        ServerOps {
            world,
            registry,
            config,
        }
    }

    /// Resolve a uuid hex or (case-insensitive) name to a uuid from the store.
    fn resolve(&self, id: &str) -> Option<u128> {
        if let Some(uuid) = parse_uuid(id) {
            return Some(uuid);
        }
        let online = self
            .registry
            .snapshot()
            .into_iter()
            .find(|h| h.name.eq_ignore_ascii_case(id))
            .map(|h| h.uuid);
        online.or_else(|| {
            all_profiles(&self.world)
                .into_iter()
                .find(|(_, p)| p.name.eq_ignore_ascii_case(id))
                .map(|(uuid, _)| uuid)
        })
    }

    fn entry(&self, uuid: u128, p: &StoredProfile) -> PlayerEntry {
        let online = self.registry.by_uuid(uuid);
        let session = metrics()
            .sessions
            .lock()
            .unwrap()
            .get(&uuid)
            .map(|s| s.elapsed().as_secs())
            .unwrap_or(0);
        PlayerEntry {
            uuid: uuid_hex(uuid),
            name: online
                .as_ref()
                .map(|h| h.name.clone())
                .unwrap_or_else(|| p.name.clone()),
            online: online.is_some(),
            ip: online.as_ref().and_then(|h| h.ip).map(|i| i.to_string()),
            first_seen: p.first_seen,
            last_seen: p.last_seen,
            playtime_secs: p.playtime_secs + session,
        }
    }
}

impl OpsService for ServerOps {
    fn dashboard(&self) -> Dashboard {
        let (mspt, mspt_max) = tick_window();
        let tps = if mspt > 0.0 {
            (1000.0 / mspt).min(20.0)
        } else {
            20.0
        };
        let (used, total) = memory_mib();
        let m = metrics();
        Dashboard {
            tps,
            mspt,
            mspt_max,
            players_online: self.registry.snapshot().len(),
            players_max: self.config.server.max_players as usize,
            ticks: crate::game::now(),
            uptime_secs: m.start.elapsed().as_secs(),
            memory_used_mib: used,
            memory_total_mib: total,
            chunk_sends: m.chunk_sends_total.load(Ordering::Relaxed),
        }
    }

    fn regions(&self, limit: usize) -> Vec<RegionStat> {
        let regions = metrics().regions.lock().unwrap();
        let mut out: Vec<RegionStat> = regions.values().filter(|r| r.cost > 0.0).cloned().collect();
        out.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.rx.cmp(&b.rx))
                .then_with(|| a.rz.cmp(&b.rz))
        });
        out.truncate(limit);
        out
    }

    fn players(&self) -> Vec<PlayerEntry> {
        let mut by_uuid: BTreeMap<u128, StoredProfile> =
            all_profiles(&self.world).into_iter().collect();
        // An online player always has a profile (written on join); fold in
        // anyone the store somehow missed so the list matches the server.
        for h in self.registry.snapshot() {
            by_uuid.entry(h.uuid).or_default();
        }
        let mut out: Vec<PlayerEntry> = by_uuid
            .iter()
            .map(|(uuid, p)| self.entry(*uuid, p))
            .collect();
        out.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        out
    }

    fn profile(&self, id: &str) -> Option<PlayerProfile> {
        let uuid = self.resolve(id)?;
        let stored = load_profile(&self.world, uuid).unwrap_or_default();
        let entry = self.entry(uuid, &stored);
        let online = self.registry.by_uuid(uuid);
        let (position, health, food, game_mode) = match &online {
            Some(h) => {
                let pos = h.pos();
                let st = h.game();
                (
                    Some((pos.x, pos.y, pos.z)),
                    Some(st.health),
                    Some(st.food),
                    Some(game_mode_name(st.mode)),
                )
            }
            None => (None, None, None, None),
        };
        let session_secs = metrics()
            .sessions
            .lock()
            .unwrap()
            .get(&uuid)
            .map(|s| s.elapsed().as_secs())
            .unwrap_or(0);
        Some(PlayerProfile {
            entry,
            ips: stored.ips,
            session_secs,
            position,
            health,
            food,
            game_mode,
        })
    }

    fn inventory(&self, id: &str) -> Option<InventoryView> {
        let uuid = self.resolve(id)?;
        let stored = load_profile(&self.world, uuid).unwrap_or_default();
        let online = self.registry.by_uuid(uuid);
        let name = online
            .as_ref()
            .map(|h| h.name.clone())
            .unwrap_or(stored.name);

        let (slots, armor, held, is_online);
        match &online {
            Some(h) => {
                let inv = h.inventory();
                let (s, a) = item_views(&inv);
                slots = s;
                armor = a;
                held = inv.held_slot();
                is_online = true;
            }
            None => {
                let key = crate::inventory::key(uuid);
                let inv = self
                    .world
                    .get_meta(&key)
                    .ok()
                    .flatten()
                    .and_then(|blob| crate::inventory::decode(&blob))?;
                let (s, a) = item_views(&inv);
                slots = s;
                armor = a;
                held = inv.held_slot();
                is_online = false;
            }
        }

        Some(InventoryView {
            uuid: uuid_hex(uuid),
            name,
            online: is_online,
            held_slot: held,
            slots,
            armor,
            // The engine has no ender chest yet; the field is in the shape so
            // the page is stable when it lands.
            ender: Vec::new(),
        })
    }

    fn overlay(&self) -> MapOverlay {
        let players = self
            .registry
            .snapshot()
            .into_iter()
            .map(|h| {
                let p = h.pos();
                MapMark {
                    kind: "player".to_string(),
                    name: h.name.clone(),
                    x: p.x,
                    y: p.y,
                    z: p.z,
                }
            })
            .collect();
        let mut entities = Vec::new();
        crate::game::entities::for_each_named(&mut |kind, p| {
            entities.push(MapMark {
                kind: kind.to_string(),
                name: kind.to_string(),
                x: p.x,
                y: p.y,
                z: p.z,
            });
        });
        MapOverlay { players, entities }
    }

    fn actor_name(&self, uuid: u128) -> Option<String> {
        if let Some(h) = self.registry.by_uuid(uuid) {
            return Some(h.name.clone());
        }
        load_profile(&self.world, uuid)
            .map(|p| p.name)
            .filter(|n| !n.is_empty())
    }
}

fn game_mode_name(mode: crate::protocol::GameMode) -> String {
    match mode {
        crate::protocol::GameMode::Survival => "survival",
        crate::protocol::GameMode::Creative => "creative",
    }
    .to_string()
}

/// The view of an inventory: main+hotbar+offhand as `slots`, armour as
/// `armor`. Empty armour entries are kept (item name `air`) so the page can
/// show the four slots.
fn item_views(inv: &crate::inventory::Inventory) -> (Vec<ItemView>, Vec<ItemView>) {
    let view = |slot: usize| {
        let s = inv.slot(slot)?;
        Some(ItemView {
            slot,
            item: s
                .item
                .vanilla_name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("item#{}", s.item.0)),
            count: s.count as u32,
            damage: s.damage,
            potion: s.potion,
        })
    };
    let mut slots = Vec::new();
    for slot in 9..crate::inventory::SLOTS {
        if let Some(v) = view(slot) {
            slots.push(v);
        }
    }
    let mut armor = Vec::new();
    for slot in 5..9 {
        if let Some(v) = view(slot) {
            armor.push(v);
        }
    }
    (slots, armor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_index_groups_thirty_two_chunks() {
        // Block coordinates: a region is 512 blocks (32 chunks) on a side.
        assert_eq!(region_of_block(0, 0), (0, 0));
        assert_eq!(region_of_block(511, 511), (0, 0));
        assert_eq!(region_of_block(512, 0), (1, 0));
        assert_eq!(region_of_block(-1, -1), (-1, -1));
        assert_eq!(region_of_block(-512, 0), (-1, 0));
        // Chunk coordinates: region changes at chunk 32.
        assert_eq!(region_of_chunk(0, 0), (0, 0));
        assert_eq!(region_of_chunk(31, 31), (0, 0));
        assert_eq!(region_of_chunk(32, 0), (1, 0));
    }

    #[test]
    fn uuid_hex_round_trips() {
        let uuid = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdefu128;
        let hex = uuid_hex(uuid);
        assert_eq!(hex.len(), 32);
        assert_eq!(parse_uuid(&hex), Some(uuid));
        assert_eq!(parse_uuid("Steve"), None);
        assert_eq!(parse_uuid("xyz"), None);
    }

    #[test]
    fn item_views_split_main_from_armour_and_skip_empty() {
        let mut inv = crate::inventory::Inventory::new();
        inv.put_slot(
            36,
            Some(crate::inventory::Stack::named("minecraft:diamond", 5)),
        );
        inv.put_slot(
            5,
            Some(crate::inventory::Stack::named("minecraft:iron_helmet", 1)),
        );
        let (slots, armor) = item_views(&inv);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot, 36);
        assert_eq!(slots[0].item, "minecraft:diamond");
        assert_eq!(slots[0].count, 5);
        assert_eq!(armor.len(), 1);
        assert_eq!(armor[0].slot, 5);
        assert_eq!(armor[0].item, "minecraft:iron_helmet");
    }

    #[test]
    fn tick_window_is_average_and_max() {
        // A clean window: 50 ms every tick -> 20 TPS.
        for _ in 0..10 {
            record_tick(50_000);
        }
        record_tick(80_000);
        let (mean, max) = tick_window();
        assert!(mean > 50.0 && mean < 54.0, "mean was {mean}");
        assert!((max - 80.0).abs() < 0.001);
    }
}
