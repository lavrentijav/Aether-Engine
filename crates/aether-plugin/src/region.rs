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

use std::collections::BTreeMap;
use std::fmt;

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
            RegionWriteError::Overlap => write!(f, "the land is already claimed"),
            RegionWriteError::Unavailable => write!(f, "the host has no region store"),
        }
    }
}
