//! The neutral region shape a plugin uses to feed the core region index.
//!
//! The core owns the fast spatial index and the protection check
//! (`aether_server::region`, CMD-29a/29b); a plugin such as `regions` or
//! `claims` owns the *model* — which boxes exist, who owns them and at what
//! priority. This type is the one value that crosses between the two without
//! making the plugin depend on the server crate (which would be a cycle).
//!
//! A plugin reads the regions that already exist through
//! [`crate::HostBackend::regions_overlapping`] and writes its own through
//! [`crate::HostBackend::region_upsert`]. The server stamps the plugin id as
//! the region's source, so one plugin can never remove another's region.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// A cuboid region in one world, as a plugin sees it.
///
/// All three axes are inclusive, matching the core's `Region`. A claim is
/// normally a full-height column (`min[1] = -64`, `max[1] = 383`), but the type
/// is the general cuboid a WorldGuard-style region needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionInfo {
    /// The region id, chosen by the owning plugin and stable across restarts.
    pub id: u64,
    /// The plugin that owns the region, filled by the host on read.
    pub source: Option<String>,
    /// The world (dimension) key.
    pub world: String,
    /// Inclusive minimum corner `[x, y, z]`.
    pub min: [i32; 3],
    /// Inclusive maximum corner `[x, y, z]`.
    pub max: [i32; 3],
    /// Higher wins when regions overlap.
    pub priority: i32,
    /// Explicit flags; an absent flag falls back to the core default.
    pub flags: BTreeMap<String, bool>,
    /// Player names with build/use rights when no flag is set.
    pub members: Vec<String>,
    /// Player names with full rights when no flag is set.
    pub owners: Vec<String>,
    /// Per-player action grants: player name → the core action flag names
    /// (`build`, `use`, `entry`, …) that player may perform despite not being
    /// a member. This is how the claims plugin writes trust levels.
    pub trusted: BTreeMap<String, BTreeSet<String>>,
}

impl RegionInfo {
    /// A region with no flags, members or owners.
    pub fn new(id: u64, world: impl Into<String>, min: [i32; 3], max: [i32; 3]) -> RegionInfo {
        RegionInfo {
            id,
            source: None,
            world: world.into(),
            min,
            max,
            priority: 0,
            flags: BTreeMap::new(),
            members: Vec::new(),
            owners: Vec::new(),
            trusted: BTreeMap::new(),
        }
    }

    /// Whether `pos` lies inside the box on every axis.
    pub fn contains(&self, pos: [i32; 3]) -> bool {
        (0..3).all(|axis| self.min[axis] <= pos[axis] && pos[axis] <= self.max[axis])
    }

    /// Whether this box overlaps the inclusive box `min..=max`.
    ///
    /// Touching faces are **not** an overlap: `0..=15` and `16..=31` are
    /// adjacent, not intersecting, which is what lets two claims share a border.
    pub fn overlaps(&self, min: [i32; 3], max: [i32; 3]) -> bool {
        (0..3).all(|axis| self.min[axis] <= max[axis] && min[axis] <= self.max[axis])
    }

    /// Whether this region belongs to `plugin`.
    pub fn is_from(&self, plugin: &str) -> bool {
        self.source.as_deref() == Some(plugin)
    }
}

/// Why a plugin's region write was refused.
///
/// The host applies the ownership rule atomically: a plugin may only replace or
/// remove a region it created. A core-created region and another plugin's
/// region are both protected, and a claim insert may additionally be refused
/// because the land is already taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegionWriteError {
    /// The id belongs to a region created directly in the core.
    OwnedByCore,
    /// The id belongs to another plugin's region.
    OwnedByOther(String),
    /// The id is already occupied, including by a region the caller's own
    /// plugin created. A fresh insert never reuses an id.
    IdTaken,
    /// The box overlaps an existing region the plugin's insert must avoid.
    Overlap,
    /// The host has no region store (a test backend that did not implement the
    /// hooks), so nothing was written.
    Unavailable,
}

impl fmt::Display for RegionWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegionWriteError::OwnedByCore => {
                write!(f, "the region id is owned by the core")
            }
            RegionWriteError::OwnedByOther(source) => {
                write!(f, "the region id belongs to `{source}`")
            }
            RegionWriteError::IdTaken => write!(f, "the region id is already in use"),
            RegionWriteError::Overlap => write!(f, "the land is already claimed"),
            RegionWriteError::Unavailable => write!(f, "the host has no region store"),
        }
    }
}

/// The next globally unique region-id serial.
///
/// The allocator is process-wide and lock-free, so two plugin instances (or two
/// threads) can never be handed the same serial. `floor` is the caller's next
/// free serial — the value it would use after a restart — so allocation never
/// goes backwards past a persisted id.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Allocate a region-id serial at least `floor`.
pub fn allocate_serial(floor: u64) -> u64 {
    let mut current = NEXT_SERIAL.load(Ordering::Relaxed);
    loop {
        let candidate = current.max(floor).max(1);
        match NEXT_SERIAL.compare_exchange_weak(
            current,
            candidate.saturating_add(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn allocated_serials_are_unique_across_threads() {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| (0..1_000).map(|_| allocate_serial(1)).collect::<Vec<_>>())
            })
            .collect();
        let mut seen = HashSet::new();
        for worker in workers {
            for serial in worker.join().unwrap() {
                assert!(seen.insert(serial), "serial {serial} allocated twice");
            }
        }
    }

    #[test]
    fn a_floor_never_reuses_a_persisted_serial() {
        let floor = 1_000_000;
        let first = allocate_serial(floor);
        let second = allocate_serial(floor);
        assert!(first >= floor);
        assert!(second > first, "{first} then {second}");
    }
}
