//! # aether-entity
//!
//! The Phase 1 **Data-Oriented Entity Engine** (spec §9/§10): entities are not
//! objects but rows in parallel component arrays (Structure-of-Arrays), so a
//! system that only touches, say, positions and velocities streams two tight
//! contiguous arrays instead of chasing scattered `struct Entity` allocations.
//!
//! ```text
//! Instead of:  Vec<Entity { pos, vel, size, health }>   (AoS, pointer chasing)
//! we store:    positions: Vec<Vec3>
//!              velocities: Vec<Vec3>
//!              sizes:      Vec<(w, h)>
//!              healths:    Vec<f32>          (SoA, cache- and SIMD-friendly)
//! ```
//!
//! An [`EntityId`] is a 64-bit handle: a slot **index** plus a **generation**.
//! Despawning a slot bumps its generation, so a stale id that happens to point
//! at a reused slot is rejected — no dangling references, no ABA hazard.
//!
//! ```
//! use aether_entity::{EntityStore, Spawn};
//! use aether_core::math::Vec3;
//!
//! let mut store = EntityStore::new();
//! let zombie = store.spawn(Spawn {
//!     position: Vec3::new(8.0, 64.0, 8.0),
//!     velocity: Vec3::new(0.1, 0.0, 0.0),
//!     size: (0.6, 1.95),
//!     health: 20.0,
//! });
//!
//! // Advance every entity's position by its velocity (one tick).
//! store.integrate(1.0);
//! assert_eq!(store.position(zombie).unwrap().x, 8.1);
//!
//! store.despawn(zombie);
//! assert!(!store.contains(zombie));
//! ```

use aether_core::math::{Aabb, Vec3};

/// A stable handle to an entity: a slot **index** and a **generation** guard.
///
/// Cheap to copy and compare. A handle stays valid until the entity is
/// despawned; afterwards the slot's generation advances and the old handle is
/// rejected by [`EntityStore::contains`] and every accessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId {
    index: u32,
    generation: u32,
}

impl EntityId {
    /// The slot index this id refers to.
    #[inline]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The generation this id was minted at.
    #[inline]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Pack into a single 64-bit value (generation in the high 32 bits).
    #[inline]
    pub const fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    /// Reconstruct from [`EntityId::to_bits`].
    #[inline]
    pub const fn from_bits(bits: u64) -> Self {
        Self {
            index: bits as u32,
            generation: (bits >> 32) as u32,
        }
    }
}

/// The initial state for a freshly spawned entity.
///
/// Implements [`Default`] (origin, at rest, a 0.6×1.8 player-ish box, 20 HP) so
/// callers can override only the fields they care about with struct-update
/// syntax: `Spawn { health: 30.0, ..Default::default() }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spawn {
    /// Feet (base-centre) position.
    pub position: Vec3,
    /// Initial velocity (blocks/tick).
    pub velocity: Vec3,
    /// Collision box footprint as `(width, height)`.
    pub size: (f64, f64),
    /// Starting health.
    pub health: f32,
}

impl Default for Spawn {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            size: (0.6, 1.8),
            health: 20.0,
        }
    }
}

/// Structure-of-Arrays storage for entities.
///
/// Component columns are indexed by an entity's slot; despawned slots leave
/// holes that later spawns reuse. Dead slots hold inert data (zero velocity),
/// so the batch [`EntityStore::integrate`] can run one flat loop over every
/// slot without special-casing the gaps.
#[derive(Debug, Clone, Default)]
pub struct EntityStore {
    // Per-slot metadata.
    generation: Vec<u32>,
    alive: Vec<bool>,
    free: Vec<u32>,
    // SoA component columns (parallel to the slot arrays).
    position: Vec<Vec3>,
    velocity: Vec<Vec3>,
    size: Vec<(f64, f64)>,
    health: Vec<f32>,
    // Number of currently-alive entities.
    live: usize,
}

impl EntityStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty store pre-sized for `cap` entities.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            generation: Vec::with_capacity(cap),
            alive: Vec::with_capacity(cap),
            free: Vec::new(),
            position: Vec::with_capacity(cap),
            velocity: Vec::with_capacity(cap),
            size: Vec::with_capacity(cap),
            health: Vec::with_capacity(cap),
            live: 0,
        }
    }

    /// Number of currently-alive entities.
    #[inline]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether no entities are alive.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Number of allocated slots (alive + reusable holes).
    #[inline]
    pub fn slots(&self) -> usize {
        self.alive.len()
    }

    /// Spawn an entity, returning its handle. Reuses a free slot if one exists,
    /// otherwise grows every column by one.
    pub fn spawn(&mut self, desc: Spawn) -> EntityId {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let i = index as usize;
            self.alive[i] = true;
            self.position[i] = desc.position;
            self.velocity[i] = desc.velocity;
            self.size[i] = desc.size;
            self.health[i] = desc.health;
            EntityId {
                index,
                generation: self.generation[i],
            }
        } else {
            let index = self.alive.len() as u32;
            self.generation.push(0);
            self.alive.push(true);
            self.position.push(desc.position);
            self.velocity.push(desc.velocity);
            self.size.push(desc.size);
            self.health.push(desc.health);
            EntityId {
                index,
                generation: 0,
            }
        }
    }

    /// Despawn `id`. Returns `true` if it was alive (and is now removed), or
    /// `false` if the handle was already stale.
    pub fn despawn(&mut self, id: EntityId) -> bool {
        if !self.contains(id) {
            return false;
        }
        let i = id.index as usize;
        self.alive[i] = false;
        // Bump the generation so any lingering copy of this handle is rejected.
        self.generation[i] = self.generation[i].wrapping_add(1);
        // Zero the velocity so the dead slot is inert under batch integration.
        self.velocity[i] = Vec3::ZERO;
        self.free.push(id.index);
        self.live -= 1;
        true
    }

    /// Whether `id` refers to a live entity (slot alive and generation current).
    #[inline]
    pub fn contains(&self, id: EntityId) -> bool {
        let i = id.index as usize;
        i < self.alive.len() && self.alive[i] && self.generation[i] == id.generation
    }

    /// Position of `id`, if live.
    #[inline]
    pub fn position(&self, id: EntityId) -> Option<Vec3> {
        self.contains(id).then(|| self.position[id.index as usize])
    }

    /// Velocity of `id`, if live.
    #[inline]
    pub fn velocity(&self, id: EntityId) -> Option<Vec3> {
        self.contains(id).then(|| self.velocity[id.index as usize])
    }

    /// Health of `id`, if live.
    #[inline]
    pub fn health(&self, id: EntityId) -> Option<f32> {
        self.contains(id).then(|| self.health[id.index as usize])
    }

    /// Collision box of `id` (built from its position + size), if live.
    #[inline]
    pub fn aabb(&self, id: EntityId) -> Option<Aabb> {
        if !self.contains(id) {
            return None;
        }
        let i = id.index as usize;
        let (w, h) = self.size[i];
        Some(Aabb::from_base(self.position[i], w, h))
    }

    /// Overwrite the position of `id`. Returns `false` if the handle is stale.
    pub fn set_position(&mut self, id: EntityId, pos: Vec3) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.position[id.index as usize] = pos;
        true
    }

    /// Overwrite the velocity of `id`. Returns `false` if the handle is stale.
    pub fn set_velocity(&mut self, id: EntityId, vel: Vec3) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.velocity[id.index as usize] = vel;
        true
    }

    /// Overwrite the health of `id`. Returns `false` if the handle is stale.
    pub fn set_health(&mut self, id: EntityId, hp: f32) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.health[id.index as usize] = hp;
        true
    }

    /// Advance every entity by `dt` ticks of its velocity: `pos += vel * dt`.
    ///
    /// One flat pass over the position/velocity columns — the auto-vectorisable
    /// hot loop the SoA layout exists for. Dead slots carry zero velocity, so
    /// they contribute nothing and need no branch.
    pub fn integrate(&mut self, dt: f64) {
        for (p, v) in self.position.iter_mut().zip(self.velocity.iter()) {
            p.x += v.x * dt;
            p.y += v.y * dt;
            p.z += v.z * dt;
        }
    }

    /// Call `f` for each live entity with its id and a mutable view of its
    /// position and velocity — the usual per-entity system shape.
    pub fn for_each_mut(&mut self, mut f: impl FnMut(EntityId, &mut Vec3, &mut Vec3)) {
        for i in 0..self.alive.len() {
            if !self.alive[i] {
                continue;
            }
            let id = EntityId {
                index: i as u32,
                generation: self.generation[i],
            };
            // `position` and `velocity` are distinct fields, so borrowing one
            // element of each at once is a disjoint mutable borrow.
            f(id, &mut self.position[i], &mut self.velocity[i]);
        }
    }

    /// The live entity ids, in slot order.
    pub fn ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        (0..self.alive.len()).filter_map(move |i| {
            self.alive[i].then_some(EntityId {
                index: i as u32,
                generation: self.generation[i],
            })
        })
    }

    /// Read-only view of the raw position column (includes dead-slot holes).
    /// Systems iterating this must respect [`EntityStore::ids`] for liveness.
    #[inline]
    pub fn positions(&self) -> &[Vec3] {
        &self.position
    }

    /// Read-only view of the raw velocity column (dead slots hold zero).
    #[inline]
    pub fn velocities(&self) -> &[Vec3] {
        &self.velocity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zombie_at(x: f64) -> Spawn {
        Spawn {
            position: Vec3::new(x, 64.0, 0.0),
            velocity: Vec3::new(1.0, 0.0, 0.0),
            size: (0.6, 1.95),
            health: 20.0,
        }
    }

    #[test]
    fn spawn_then_read_components() {
        let mut s = EntityStore::new();
        let e = s.spawn(zombie_at(3.0));
        assert_eq!(s.len(), 1);
        assert_eq!(s.position(e), Some(Vec3::new(3.0, 64.0, 0.0)));
        assert_eq!(s.velocity(e), Some(Vec3::new(1.0, 0.0, 0.0)));
        assert_eq!(s.health(e), Some(20.0));
        let bb = s.aabb(e).unwrap();
        assert_eq!(bb.min, Vec3::new(3.0 - 0.3, 64.0, -0.3));
        assert_eq!(bb.max, Vec3::new(3.0 + 0.3, 64.0 + 1.95, 0.3));
    }

    #[test]
    fn integrate_moves_by_velocity() {
        let mut s = EntityStore::new();
        let e = s.spawn(zombie_at(0.0));
        s.integrate(1.0);
        assert_eq!(s.position(e).unwrap().x, 1.0);
        s.integrate(0.5);
        assert_eq!(s.position(e).unwrap().x, 1.5);
    }

    #[test]
    fn despawn_invalidates_handle() {
        let mut s = EntityStore::new();
        let e = s.spawn(zombie_at(0.0));
        assert!(s.contains(e));
        assert!(s.despawn(e));
        assert!(!s.contains(e));
        assert_eq!(s.len(), 0);
        // Double-despawn is a no-op.
        assert!(!s.despawn(e));
        // Accessors reject the stale handle.
        assert_eq!(s.position(e), None);
        assert!(!s.set_health(e, 1.0));
    }

    #[test]
    fn free_slot_is_reused_with_new_generation() {
        let mut s = EntityStore::new();
        let a = s.spawn(zombie_at(0.0));
        s.despawn(a);
        let b = s.spawn(zombie_at(5.0));
        // Same slot index, but a fresh generation.
        assert_eq!(a.index(), b.index());
        assert_ne!(a.generation(), b.generation());
        // The old handle must not resolve to the new entity.
        assert!(!s.contains(a));
        assert!(s.contains(b));
        assert_eq!(s.position(b), Some(Vec3::new(5.0, 64.0, 0.0)));
        // No slot growth: the hole was reused.
        assert_eq!(s.slots(), 1);
    }

    #[test]
    fn dead_slots_are_inert_under_integration() {
        let mut s = EntityStore::new();
        let a = s.spawn(zombie_at(0.0));
        let b = s.spawn(zombie_at(10.0));
        s.despawn(a); // leaves a hole with zeroed velocity
        s.integrate(1.0);
        // b advanced; the dead slot did not drift.
        assert_eq!(s.position(b).unwrap().x, 11.0);
        assert_eq!(s.velocities()[a.index() as usize], Vec3::ZERO);
    }

    #[test]
    fn ids_yields_only_live_entities() {
        let mut s = EntityStore::new();
        let a = s.spawn(zombie_at(0.0));
        let b = s.spawn(zombie_at(1.0));
        let c = s.spawn(zombie_at(2.0));
        s.despawn(b);
        let live: Vec<_> = s.ids().collect();
        assert_eq!(live, vec![a, c]);
    }

    #[test]
    fn for_each_mut_updates_in_place() {
        let mut s = EntityStore::new();
        let e = s.spawn(zombie_at(0.0));
        s.for_each_mut(|_id, pos, vel| {
            pos.y += 1.0;
            *vel = Vec3::ZERO;
        });
        assert_eq!(s.position(e).unwrap().y, 65.0);
        assert_eq!(s.velocity(e).unwrap(), Vec3::ZERO);
    }

    #[test]
    fn entity_id_bit_round_trip() {
        let id = EntityId {
            index: 12345,
            generation: 678,
        };
        assert_eq!(EntityId::from_bits(id.to_bits()), id);
    }

    #[test]
    fn spawn_default_is_playerish() {
        let mut s = EntityStore::new();
        let e = s.spawn(Spawn::default());
        assert_eq!(s.position(e), Some(Vec3::ZERO));
        assert_eq!(s.health(e), Some(20.0));
        let bb = s.aabb(e).unwrap();
        assert_eq!(bb.max.y, 1.8);
    }

    #[test]
    fn many_spawns_and_despawns_keep_counts_consistent() {
        let mut s = EntityStore::with_capacity(1000);
        let mut ids = Vec::new();
        for i in 0..1000 {
            ids.push(s.spawn(zombie_at(i as f64)));
        }
        assert_eq!(s.len(), 1000);
        // Despawn every other one.
        for (n, id) in ids.iter().enumerate() {
            if n % 2 == 0 {
                s.despawn(*id);
            }
        }
        assert_eq!(s.len(), 500);
        assert_eq!(s.ids().count(), 500);
        // Respawn fills the holes without growing past the high-water mark.
        for i in 0..500 {
            s.spawn(zombie_at(i as f64));
        }
        assert_eq!(s.len(), 1000);
        assert_eq!(s.slots(), 1000);
    }
}
