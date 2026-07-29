//! Projectiles and dropped items (spec §9.4).
//!
//! Arrows, potions, fireballs and dropped items are short-lived and numerous, so
//! they live in their own minimal Structure-of-Arrays store — separate from the
//! richer [`EntityStore`](crate::EntityStore) — and are advanced as one flat
//! ballistic batch (`vel.y -= gravity; pos += vel; vel *= drag`) that the
//! compiler can auto-vectorise into SIMD packets.
//!
//! Unlike [`EntityStore`](crate::EntityStore) these carry **no** generational
//! handle: they are fire-and-forget, so the store keeps its columns **dense**
//! (swap-remove on despawn) and iterated in bulk rather than addressed by id.
//!
//! ```
//! use aether_entity::ProjectileStore;
//! use aether_core::math::Vec3;
//!
//! let mut arrows = ProjectileStore::new();
//! arrows.spawn(Vec3::new(0.0, 64.0, 0.0), Vec3::new(1.0, 0.5, 0.0), 200);
//! // One tick of ballistic motion (gravity 0.05, drag 0.99).
//! let expired = arrows.integrate(0.05, 0.99);
//! assert_eq!(expired, 0);
//! assert_eq!(arrows.len(), 1);
//! ```

use aether_core::math::Vec3;

/// A dense SoA store of ballistic projectiles / dropped items.
#[derive(Debug, Clone, Default)]
pub struct ProjectileStore {
    position: Vec<Vec3>,
    velocity: Vec<Vec3>,
    /// Remaining lifetime in ticks; a projectile is removed when it hits 0.
    life: Vec<u32>,
}

impl ProjectileStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty store pre-sized for `cap` projectiles.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            position: Vec::with_capacity(cap),
            velocity: Vec::with_capacity(cap),
            life: Vec::with_capacity(cap),
        }
    }

    /// Number of live projectiles.
    #[inline]
    pub fn len(&self) -> usize {
        self.life.len()
    }

    /// Whether there are no projectiles.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.life.is_empty()
    }

    /// Spawn a projectile with `life` ticks to live, returning its current dense
    /// index (indices are **not** stable across [`ProjectileStore::integrate`]
    /// or [`ProjectileStore::despawn`]).
    pub fn spawn(&mut self, position: Vec3, velocity: Vec3, life: u32) -> usize {
        let i = self.life.len();
        self.position.push(position);
        self.velocity.push(velocity);
        self.life.push(life);
        i
    }

    /// Remove the projectile at dense `index` by swapping the last one into its
    /// place (O(1), order not preserved).
    pub fn despawn(&mut self, index: usize) {
        self.position.swap_remove(index);
        self.velocity.swap_remove(index);
        self.life.swap_remove(index);
    }

    /// Advance every projectile one tick of ballistic motion, then drop any that
    /// have expired. Returns how many expired this tick.
    ///
    /// The motion pass is a single flat SoA loop — the shape the compiler
    /// vectorises — and expiry is a second dense compaction pass.
    pub fn integrate(&mut self, gravity: f64, drag: f64) -> usize {
        for i in 0..self.life.len() {
            let v = &mut self.velocity[i];
            v.y -= gravity;
            let p = &mut self.position[i];
            p.x += v.x;
            p.y += v.y;
            p.z += v.z;
            v.x *= drag;
            v.y *= drag;
            v.z *= drag;
            self.life[i] = self.life[i].saturating_sub(1);
        }
        self.retire_expired()
    }

    /// Drop expired projectiles (life == 0), compacting the dense columns.
    /// Returns how many were removed.
    fn retire_expired(&mut self) -> usize {
        let mut removed = 0;
        let mut i = 0;
        while i < self.life.len() {
            if self.life[i] == 0 {
                self.despawn(i);
                removed += 1;
                // Do not advance `i`: a swapped-in projectile now occupies it.
            } else {
                i += 1;
            }
        }
        removed
    }

    /// Position of the projectile at dense `index`.
    #[inline]
    pub fn position(&self, index: usize) -> Vec3 {
        self.position[index]
    }

    /// Velocity of the projectile at dense `index`.
    #[inline]
    pub fn velocity(&self, index: usize) -> Vec3 {
        self.velocity[index]
    }

    /// Remaining lifetime (ticks) of the projectile at dense `index`.
    #[inline]
    pub fn life(&self, index: usize) -> u32 {
        self.life[index]
    }

    /// The raw position column (dense, index-parallel with velocity/life).
    #[inline]
    pub fn positions(&self) -> &[Vec3] {
        &self.position
    }

    /// The raw velocity column.
    #[inline]
    pub fn velocities(&self) -> &[Vec3] {
        &self.velocity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_grows_store() {
        let mut s = ProjectileStore::new();
        assert!(s.is_empty());
        s.spawn(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 100);
        s.spawn(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), 100);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn integrate_is_ballistic() {
        let mut s = ProjectileStore::new();
        s.spawn(Vec3::new(0.0, 10.0, 0.0), Vec3::new(2.0, 0.0, 0.0), 100);
        // No drag, gravity 1.0: after one tick vy = -1, pos moves by (2, -1, 0).
        s.integrate(1.0, 1.0);
        assert_eq!(s.position(0), Vec3::new(2.0, 9.0, 0.0));
        assert_eq!(s.velocity(0), Vec3::new(2.0, -1.0, 0.0));
        // Second tick: vy = -2, pos += (2, -2, 0).
        s.integrate(1.0, 1.0);
        assert_eq!(s.position(0), Vec3::new(4.0, 7.0, 0.0));
    }

    #[test]
    fn drag_decays_velocity() {
        let mut s = ProjectileStore::new();
        s.spawn(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), 100);
        s.integrate(0.0, 0.5);
        assert_eq!(s.velocity(0).x, 5.0);
        s.integrate(0.0, 0.5);
        assert_eq!(s.velocity(0).x, 2.5);
    }

    #[test]
    fn expired_projectiles_are_retired() {
        let mut s = ProjectileStore::new();
        s.spawn(Vec3::ZERO, Vec3::ZERO, 1); // dies next tick
        s.spawn(Vec3::ZERO, Vec3::ZERO, 5);
        let expired = s.integrate(0.0, 1.0);
        assert_eq!(expired, 1);
        assert_eq!(s.len(), 1);
        assert_eq!(s.life(0), 4);
    }

    #[test]
    fn despawn_is_dense_swap_remove() {
        let mut s = ProjectileStore::new();
        s.spawn(Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, 10);
        s.spawn(Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO, 10);
        s.spawn(Vec3::new(3.0, 0.0, 0.0), Vec3::ZERO, 10);
        s.despawn(0); // last (3.0) swaps into slot 0
        assert_eq!(s.len(), 2);
        assert_eq!(s.position(0).x, 3.0);
        assert_eq!(s.position(1).x, 2.0);
    }

    #[test]
    fn many_projectiles_all_expire_together() {
        let mut s = ProjectileStore::with_capacity(1000);
        for i in 0..1000 {
            s.spawn(Vec3::new(i as f64, 0.0, 0.0), Vec3::new(0.0, 0.1, 0.0), 3);
        }
        for _ in 0..2 {
            assert_eq!(s.integrate(0.05, 0.99), 0);
        }
        // Third tick: all reach life 0 and retire at once.
        assert_eq!(s.integrate(0.05, 0.99), 1000);
        assert!(s.is_empty());
    }
}
