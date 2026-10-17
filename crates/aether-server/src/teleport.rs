//! Core teleport: exact moves, a safe-spot search, and the service plugins use.
//!
//! Vanilla has a `TeleportCommand` and a `findSafePosition`-style search; the
//! search belongs in the core because it reads collision (and, when the world
//! offers it, a surface hint) and runs on every `/rtp`, `/spawn` and
//! Essentials-style `/top`/`/back`. Keeping it here means plugins call one
//! function instead of each re-deriving "where can a player actually stand".
//!
//! # What lives here
//!
//! * [`TeleportTarget`] — an exact world position, or a block to snap onto a
//!   safe spot.
//! * [`TeleportCause`] — why the move happened, so a region `entry` check or a
//!   plugin observer can treat a portal jump differently from a command.
//! * [`find_safe_spot`] / [`random_safe_spot`] — the search. It reads the block
//!   collision the engine already publishes through [`BlockView`] (a slab is a
//!   half cell, a fence is 1.5 tall, an open gate is passable), requires two
//!   clear cells over a solid, non-hazardous floor, and refuses lava, fire and
//!   cactus.
//! * [`teleport`] / [`teleport_with`] — move a player, emit the teleport and
//!   move events, and (via [`TeleportHooks`]) let the region system veto the
//!   destination and the plugin host observe the result.
//!
//! # Scope
//!
//! The engine runs one world per process today, so this is an intra-world
//! move. Cross-dimension support needs a world registry that does not exist
//! yet; the [`TeleportCause`] split is the seam it will hang from (a portal
//! move will carry [`TeleportCause::Portal`] so the codec can send the
//! dimension-change dance). Passengers and mounts follow from the same future
//! entity model.

use std::f64::consts::TAU;

use aether_api::Vector3;

use aether_physics::BlockView;

use crate::game::tables::Rng;
use crate::players::{PlayerHandle, PosLook, SharedRegistry};
use crate::protocol::{BlockSource, ServerEvent};

/// How far [`TeleportTarget::Safe`] searches from the requested block for a
/// spot an entity can actually stand on.
const SAFE_RADIUS: u32 = 8;

/// An integer block coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockPos {
    /// East(+)/west(−).
    pub x: i32,
    /// Up(+)/down(−).
    pub y: i32,
    /// South(+)/north(−).
    pub z: i32,
}

impl BlockPos {
    /// Construct a block coordinate.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// The centre of the block's bottom face — where an entity's feet rest.
    pub fn feet(self) -> Vector3 {
        Vector3::new(self.x as f64 + 0.5, self.y as f64, self.z as f64 + 0.5)
    }
}

/// Where a teleport should put an entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TeleportTarget {
    /// An exact world position and look, already resolved — including any `~`
    /// relative coordinates, which the caller resolves before this call.
    Exact(Vector3, f32, f32),
    /// A block to stand on. The search snaps the entity down onto the nearest
    /// safe spot around it, or the move fails with [`TeleportError::NoSafeSpot`].
    Safe(BlockPos),
}

/// Why a teleport is happening.
///
/// The cause is carried to the hooks so a region can treat a portal crossing
/// differently from an operator command, and a plugin observer can log it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleportCause {
    /// An operator or player command (`/tp`, `/spawn`).
    Command,
    /// A plugin asked for the move (the SDK `players.teleport`).
    Plugin,
    /// The entity walked through a portal.
    Portal,
}

impl TeleportCause {
    /// The cause's wire name, matching the SDK's `teleport-cause` enum.
    pub fn as_str(self) -> &'static str {
        match self {
            TeleportCause::Command => "command",
            TeleportCause::Plugin => "plugin",
            TeleportCause::Portal => "portal",
        }
    }
}

/// Why a teleport did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleportError {
    /// No safe spot was found near a [`TeleportTarget::Safe`] block.
    NoSafeSpot,
    /// A hook refused the destination — a region's `entry` flag.
    EntryDenied,
}

impl std::fmt::Display for TeleportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeleportError::NoSafeSpot => f.write_str("no safe spot to teleport to"),
            TeleportError::EntryDenied => f.write_str("the destination refused entry"),
        }
    }
}

impl std::error::Error for TeleportError {}

/// Tunables for the safe-spot search.
#[derive(Debug, Clone, Copy)]
pub struct SafeSpotConfig {
    /// The lowest addressable block Y in the world (inclusive).
    pub min_y: i32,
    /// The highest addressable block Y in the world (inclusive).
    pub max_y: i32,
    /// How many random columns [`random_safe_spot`] may try before giving up.
    pub max_attempts: u32,
    /// Try the world's surface hint at a column before scanning from `around.y`.
    pub prefer_surface: bool,
}

impl Default for SafeSpotConfig {
    fn default() -> Self {
        // The engine's vertical range: `-64..=383` (see `session::WORLD_BOTTOM`).
        Self {
            min_y: -64,
            max_y: 383,
            max_attempts: 96,
            prefer_surface: true,
        }
    }
}

/// A world the safe-spot search can read: collision from [`BlockView`], plus
/// the two facts the search needs that collision alone cannot express.
///
/// A plain [`BlockView`] is enough to *compile* (`is_hazard_at` and
/// `surface_y` default to "nothing known"), but hazard avoidance only works
/// when a world answers [`TeleportWorld::is_hazard_at`].
pub trait TeleportWorld: BlockView {
    /// Whether the block at `(x, y, z)` is one an entity must not be placed in:
    /// lava, fire, a cactus, a campfire. The default knows no hazards.
    fn is_hazard_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    /// The Y at which an entity's feet rest on this column's surface, if the
    /// world has a cheap hint. Used by [`SafeSpotConfig::prefer_surface`].
    fn surface_y(&self, _x: i32, _z: i32) -> Option<i32> {
        None
    }
}

/// Every engine [`aether_api::World`] is a teleport world: its per-state shape
/// table is the collision view, its block name table names the hazards, and
/// `height_hint` is the surface.
impl<B, G> TeleportWorld for aether_api::World<B, G>
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    fn is_hazard_at(&self, x: i32, y: i32, z: i32) -> bool {
        let id = self.get_block(x, y, z);
        // Air is the overwhelmingly common answer; skip the name lookup.
        if id == aether_world::registry::ids::AIR {
            return false;
        }
        aether_world::registry::blocks::block_of_state(id)
            .is_some_and(|(_, name)| is_hazard_name(name))
    }

    fn surface_y(&self, x: i32, z: i32) -> Option<i32> {
        // `height_hint` is the topmost non-air block; feet rest one above it.
        Some(self.height_hint(x, z) + 1)
    }
}

/// Whether a base block name (with or without its `minecraft:` prefix) is a
/// hazard the search must not place a body in.
pub fn is_hazard_name(name: &str) -> bool {
    let base = name.rsplit(':').next().unwrap_or(name);
    matches!(
        base,
        "lava" | "fire" | "soul_fire" | "cactus" | "campfire" | "soul_campfire"
    )
}

/// Whether any collision box overlaps the cell `(x, y, z)`, reusing `scratch`
/// for the box list.
fn collides_with<V: BlockView + ?Sized>(
    view: &V,
    x: i32,
    y: i32,
    z: i32,
    scratch: &mut Vec<aether_api::Aabb>,
) -> bool {
    scratch.clear();
    view.collision_boxes(x, y, z, scratch);
    !scratch.is_empty()
}

/// Whether `pos` is a safe place for a player (two clear cells over a solid,
/// non-hazardous floor) within the configured world height.
///
/// This is the single predicate the search is built from, exposed so callers
/// can validate a coordinate they already have.
pub fn is_safe_spot<W: TeleportWorld + ?Sized>(
    world: &W,
    pos: BlockPos,
    config: &SafeSpotConfig,
) -> bool {
    let mut scratch = Vec::new();
    is_safe_spot_with(world, pos, config, &mut scratch)
}

fn is_safe_spot_with<W: TeleportWorld + ?Sized>(
    world: &W,
    pos: BlockPos,
    config: &SafeSpotConfig,
    scratch: &mut Vec<aether_api::Aabb>,
) -> bool {
    // Feet at `pos.y`, head at `pos.y + 1`; both must be addressable, and a
    // floor must exist below the feet (so `pos.y` needs at least one more
    // block of room under it). `pos.y >= max_y` is `pos.y + 1 > max_y` without
    // the addition, which is what keeps a near-`i32::MAX` coordinate panic-free.
    if pos.y <= config.min_y || pos.y >= config.max_y {
        return false;
    }
    // A solid floor to stand on.
    if !collides_with(world, pos.x, pos.y - 1, pos.z, scratch) {
        return false;
    }
    // Two clear body cells.
    if collides_with(world, pos.x, pos.y, pos.z, scratch)
        || collides_with(world, pos.x, pos.y + 1, pos.z, scratch)
    {
        return false;
    }
    // No lava, fire, cactus or campfire in the body or the floor.
    (-1..=1).all(|dy| !world.is_hazard_at(pos.x, pos.y + dy, pos.z))
}

/// Find a safe Y in one column, nearest to `around_y` first.
///
/// With [`SafeSpotConfig::prefer_surface`], the world's surface hint is tried
/// first — this is what makes a surface `/rtp` land on the ground rather than
/// deep in the first cave below it. Otherwise the scan expands outward from
/// `around_y` (down before up), so a search started in a cave finds the cave
/// floor and a search started near the surface finds the surface.
fn safe_y_in_column<W: TeleportWorld + ?Sized>(
    world: &W,
    x: i32,
    z: i32,
    around_y: i32,
    config: &SafeSpotConfig,
) -> Option<i32> {
    // Feet need a floor below (`min_y`) and a head cell above (`max_y`).
    let lo = config.min_y.saturating_add(1);
    let hi = config.max_y.saturating_sub(1);
    if hi < lo {
        return None;
    }
    let mut scratch = Vec::new();

    if config.prefer_surface {
        if let Some(sy) = world.surface_y(x, z) {
            if (lo..=hi).contains(&sy)
                && is_safe_spot_with(world, BlockPos::new(x, sy, z), config, &mut scratch)
            {
                return Some(sy);
            }
        }
    }

    let start = around_y.clamp(lo, hi);
    if is_safe_spot_with(world, BlockPos::new(x, start, z), config, &mut scratch) {
        return Some(start);
    }
    let mut d = 1;
    loop {
        let down = start - d;
        let up = start + d;
        let mut any = false;
        if down >= lo {
            any = true;
            if is_safe_spot_with(world, BlockPos::new(x, down, z), config, &mut scratch) {
                return Some(down);
            }
        }
        if up <= hi {
            any = true;
            if is_safe_spot_with(world, BlockPos::new(x, up, z), config, &mut scratch) {
                return Some(up);
            }
        }
        if !any {
            return None;
        }
        d += 1;
    }
}

/// Visit the cells of a Chebyshev ring around the origin, in a fixed order,
/// stopping as soon as `f` returns `Some`. Ring 0 is the origin itself.
fn scan_ring<F>(ring: i32, mut f: F) -> Option<BlockPos>
where
    F: FnMut(i32, i32) -> Option<BlockPos>,
{
    if ring == 0 {
        return f(0, 0);
    }
    for dx in -ring..=ring {
        if let Some(p) = f(dx, -ring) {
            return Some(p);
        }
    }
    for dz in -ring + 1..=ring {
        if let Some(p) = f(ring, dz) {
            return Some(p);
        }
    }
    for dx in (-ring..=ring - 1).rev() {
        if let Some(p) = f(dx, ring) {
            return Some(p);
        }
    }
    for dz in (-ring + 1..=ring - 1).rev() {
        if let Some(p) = f(-ring, dz) {
            return Some(p);
        }
    }
    None
}

/// Search outward from `around` for a safe spot, rings closest-first.
///
/// Deterministic: no randomness and a fixed ring order, so the same world and
/// arguments always answer the same block. `radius` is in blocks (Chebyshev).
pub fn find_safe_spot<W: TeleportWorld + ?Sized>(
    world: &W,
    around: BlockPos,
    radius: u32,
    config: SafeSpotConfig,
) -> Option<BlockPos> {
    let r = radius.min(i32::MAX as u32) as i32;
    for ring in 0..=r {
        let found = scan_ring(ring, |dx, dz| {
            // A near-`i32::MAX` origin plus a ring delta must not overflow.
            let x = around.x.checked_add(dx)?;
            let z = around.z.checked_add(dz)?;
            safe_y_in_column(world, x, z, around.y, &config).map(|y| BlockPos::new(x, y, z))
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Pick a random safe spot between `min_radius` and `max_radius` blocks from
/// `center`, giving up after [`SafeSpotConfig::max_attempts`] columns.
///
/// Bounded by construction — the loop runs a fixed number of times — so a
/// radius with no valid ground at all returns `None` rather than spinning.
/// Deterministic for a given `rng` state.
pub fn random_safe_spot<W: TeleportWorld + ?Sized>(
    world: &W,
    center: BlockPos,
    min_radius: u32,
    max_radius: u32,
    config: SafeSpotConfig,
    rng: &mut Rng,
) -> Option<BlockPos> {
    let (min_r, max_r) = if max_radius < min_radius {
        (max_radius, min_radius)
    } else {
        (min_radius, max_radius)
    };
    let span = max_r.saturating_sub(min_r);
    for _ in 0..config.max_attempts.max(1) {
        let angle = rng.f64() * TAU;
        // Draw the radius in `u64` so a span near `u32::MAX` cannot overflow
        // the arithmetic (and is not silently narrowed to the minimum).
        let radius = min_r
            + if span == 0 {
                0
            } else {
                (rng.next_u64() % (span as u64 + 1)) as u32
            };
        let dx = (angle.cos() * radius as f64).round() as i32;
        let dz = (angle.sin() * radius as f64).round() as i32;
        let Some(x) = center.x.checked_add(dx) else {
            continue;
        };
        let Some(z) = center.z.checked_add(dz) else {
            continue;
        };
        if let Some(y) = safe_y_in_column(world, x, z, center.y, &config) {
            return Some(BlockPos::new(x, y, z));
        }
    }
    None
}

/// A destination hook: a region's `entry` check and an after-the-fact observer.
///
/// The core is deliberately unaware of regions and the plugin host, so it calls
/// through this seam. A host with neither uses [`NoHooks`].
pub trait TeleportHooks {
    /// Whether the entity may enter `to`. Returning `false` cancels the move
    /// with [`TeleportError::EntryDenied`] and leaves the entity where it was.
    fn allow_entry(&self, _handle: &PlayerHandle, _to: Vector3, _cause: TeleportCause) -> bool {
        true
    }

    /// Called after a move has been applied and announced.
    fn after_teleport(
        &self,
        _handle: &PlayerHandle,
        _from: PosLook,
        _to: PosLook,
        _cause: TeleportCause,
    ) {
    }
}

/// A host with no region check and no observer.
pub struct NoHooks;

impl TeleportHooks for NoHooks {}

/// Move a player to `target`, firing the teleport and move events.
///
/// This is [`teleport_with`] with [`NoHooks`]; it preserves the behaviour the
/// old `game::commands::teleport` had (position set, `Teleport` event to the
/// mover, `EntityMove` broadcast) and is the entry point the built-in commands
/// still use.
pub fn teleport<W: TeleportWorld + BlockSource>(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &W,
    target: TeleportTarget,
    cause: TeleportCause,
) -> Result<Vector3, TeleportError> {
    teleport_with(handle, registry, world, target, cause, &NoHooks)
}

/// [`teleport`] with a [`TeleportHooks`] seam.
///
/// For [`TeleportTarget::Safe`], the destination is resolved with
/// [`find_safe_spot`] over a small radius; a column with no safe spot fails
/// instead of placing the player in lava or mid-air.
pub fn teleport_with<W: TeleportWorld + BlockSource>(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &W,
    target: TeleportTarget,
    cause: TeleportCause,
    hooks: &dyn TeleportHooks,
) -> Result<Vector3, TeleportError> {
    let before = handle.pos();
    let (to, yaw, pitch) = match target {
        TeleportTarget::Exact(pos, yaw, pitch) => (pos, yaw, pitch),
        TeleportTarget::Safe(block) => {
            // A `Safe` target means "stand at this block if it is safe, else at
            // the nearest safe spot". Surface preference is for the `/rtp` and
            // `/spawn` searches (they call `find_safe_spot` with their own
            // config); here it would drag a plugin teleport to an elevated home
            // back down to the natural surface, so it is off.
            let config = SafeSpotConfig {
                prefer_surface: false,
                ..SafeSpotConfig::default()
            };
            let found = find_safe_spot(world, block, SAFE_RADIUS, config)
                .ok_or(TeleportError::NoSafeSpot)?;
            (found.feet(), before.yaw, before.pitch)
        }
    };
    if !hooks.allow_entry(handle, to, cause) {
        return Err(TeleportError::EntryDenied);
    }
    let after = PosLook {
        x: to.x,
        y: to.y,
        z: to.z,
        yaw,
        pitch,
        on_ground: before.on_ground,
    };
    handle.set_pos(after);
    handle.game().fall_peak = None;
    handle.emit(
        &ServerEvent::Teleport {
            x: to.x,
            y: to.y,
            z: to.z,
            yaw,
            pitch,
        },
        world,
    );
    registry.broadcast_except(handle.entity_id, &ServerEvent::EntityMove(handle), world);
    hooks.after_teleport(handle, before, after, cause);
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::players::{PlayerHandle, Registry};
    use aether_api::Aabb;
    use aether_world::BlockStateId;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A hand-built world: a sparse map of named blocks, nothing else.
    #[derive(Default)]
    struct TestWorld {
        blocks: HashMap<(i32, i32, i32), &'static str>,
        surface: Option<i32>,
    }

    impl TestWorld {
        fn set(&mut self, x: i32, y: i32, z: i32, name: &'static str) {
            self.blocks.insert((x, y, z), name);
        }

        fn name(&self, x: i32, y: i32, z: i32) -> Option<&'static str> {
            self.blocks.get(&(x, y, z)).copied()
        }
    }

    fn colliding(name: &str) -> bool {
        !matches!(
            name.rsplit(':').next().unwrap_or(name),
            "air" | "water" | "lava" | "fire" | "soul_fire"
        )
    }

    impl BlockView for TestWorld {
        fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
            self.name(x, y, z).is_some_and(colliding)
        }

        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if self.name(x, y, z).is_some_and(colliding) {
                out.push(Aabb::block(x, y, z));
            }
        }
    }

    impl TeleportWorld for TestWorld {
        fn is_hazard_at(&self, x: i32, y: i32, z: i32) -> bool {
            self.name(x, y, z).is_some_and(is_hazard_name)
        }

        fn surface_y(&self, _x: i32, _z: i32) -> Option<i32> {
            self.surface
        }
    }

    impl BlockSource for TestWorld {
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
            BlockStateId::AIR
        }
    }

    fn cave() -> TestWorld {
        let mut w = TestWorld::default();
        for x in -2..=2 {
            for z in -2..=2 {
                w.set(x, 10, z, "minecraft:stone");
                w.set(x, 20, z, "minecraft:stone");
            }
        }
        w
    }

    #[test]
    fn seals_a_cave_and_returns_the_cave_floor() {
        let w = cave();
        let config = SafeSpotConfig {
            prefer_surface: false,
            ..Default::default()
        };
        let p = find_safe_spot(&w, BlockPos::new(0, 15, 0), 0, config).unwrap();
        // The floor is at 10, so a body stands with its feet at 11 — under the
        // roof at 20, not on top of it.
        assert_eq!(p, BlockPos::new(0, 11, 0));
        assert!(p.y < 20, "must not climb onto the sealed roof");
    }

    #[test]
    fn never_picks_a_spot_inside_lava() {
        let mut w = TestWorld::default();
        for x in -2..=2 {
            for z in -2..=2 {
                w.set(x, 10, z, "minecraft:stone");
            }
        }
        // Molten floor pocket in the centre column: the natural Y (11) sits in
        // lava and there is nothing else to stand on there.
        w.set(0, 11, 0, "minecraft:lava");
        let config = SafeSpotConfig {
            prefer_surface: false,
            ..Default::default()
        };
        assert_eq!(
            find_safe_spot(&w, BlockPos::new(0, 15, 0), 0, config),
            None,
            "the only standable height in that column is lava"
        );
        // A ring out reaches the neighbouring dry floor, never the lava.
        let p = find_safe_spot(&w, BlockPos::new(0, 15, 0), 1, config).unwrap();
        assert!(
            !w.is_hazard_at(p.x, p.y, p.z),
            "landed in a hazard at {p:?}"
        );
        assert_ne!(p, BlockPos::new(0, 11, 0));
        assert_eq!(p.y, 11, "the cave floor is still the only standable height");
    }

    #[test]
    fn a_full_column_picks_the_surface() {
        let mut w = TestWorld::default();
        for y in -64..=5 {
            w.set(0, y, 0, "minecraft:stone");
        }
        let config = SafeSpotConfig {
            prefer_surface: false,
            ..Default::default()
        };
        let p = find_safe_spot(&w, BlockPos::new(0, 8, 0), 0, config).unwrap();
        assert_eq!(p, BlockPos::new(0, 6, 0));
    }

    #[test]
    fn prefers_the_surface_hint_when_available() {
        let mut w = TestWorld::default();
        w.set(0, 3, 0, "minecraft:grass_block");
        w.surface = Some(4);
        // Start far above the surface; the hint wins without scanning down.
        let p = find_safe_spot(&w, BlockPos::new(0, 100, 0), 0, SafeSpotConfig::default()).unwrap();
        assert_eq!(p, BlockPos::new(0, 4, 0));
    }

    #[test]
    fn respects_the_world_height() {
        let mut w = TestWorld::default();
        w.set(0, 5, 0, "minecraft:stone");
        let config = SafeSpotConfig {
            min_y: 0,
            max_y: 5,
            prefer_surface: false,
            ..Default::default()
        };
        // Feet at 6 would need a head cell at 7, over the top (5) — refused.
        assert_eq!(find_safe_spot(&w, BlockPos::new(0, 5, 0), 0, config), None);
    }

    #[test]
    fn random_fails_after_the_budget_without_spinning() {
        let w = TestWorld::default(); // no ground anywhere
        let mut rng = Rng::new(1);
        let config = SafeSpotConfig {
            max_attempts: 8,
            prefer_surface: false,
            ..Default::default()
        };
        assert_eq!(
            random_safe_spot(&w, BlockPos::new(0, 64, 0), 10, 50, config, &mut rng),
            None
        );
    }

    #[test]
    fn random_is_deterministic_for_a_seed() {
        let mut w = TestWorld::default();
        for x in -40..=40 {
            for z in -40..=40 {
                w.set(x, 3, z, "minecraft:grass_block");
            }
        }
        let config = SafeSpotConfig::default();
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let first = random_safe_spot(&w, BlockPos::new(0, 64, 0), 8, 32, config, &mut a);
        let second = random_safe_spot(&w, BlockPos::new(0, 64, 0), 8, 32, config, &mut b);
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    fn handle() -> Arc<PlayerHandle> {
        PlayerHandle::for_test(1, "tester")
    }

    fn registry_with(handle: &Arc<PlayerHandle>) -> SharedRegistry {
        let registry = Arc::new(Registry::default());
        registry.insert_for_test(Arc::clone(handle));
        registry
    }

    #[test]
    fn exact_teleport_moves_the_player_and_preserves_look() {
        let handle = handle();
        let registry = registry_with(&handle);
        let world = TestWorld::default();
        let out = teleport(
            &handle,
            &registry,
            &world,
            TeleportTarget::Exact(Vector3::new(5.0, 70.0, -3.0), 90.0, 10.0),
            TeleportCause::Command,
        )
        .unwrap();
        assert_eq!(out, Vector3::new(5.0, 70.0, -3.0));
        let p = handle.pos();
        assert_eq!(
            (p.x, p.y, p.z, p.yaw, p.pitch),
            (5.0, 70.0, -3.0, 90.0, 10.0)
        );
    }

    #[test]
    fn safe_teleport_snaps_onto_the_floor() {
        let handle = handle();
        let registry = registry_with(&handle);
        let world = cave();
        let out = teleport(
            &handle,
            &registry,
            &world,
            TeleportTarget::Safe(BlockPos::new(0, 15, 0)),
            TeleportCause::Command,
        )
        .unwrap();
        assert_eq!(out, BlockPos::new(0, 11, 0).feet());
    }

    #[test]
    fn a_region_entry_deny_cancels_the_move() {
        struct DenyAll;
        impl TeleportHooks for DenyAll {
            fn allow_entry(&self, _h: &PlayerHandle, _to: Vector3, _c: TeleportCause) -> bool {
                false
            }
        }
        let handle = handle();
        let registry = registry_with(&handle);
        let world = TestWorld::default();
        let before = handle.pos();
        let err = teleport_with(
            &handle,
            &registry,
            &world,
            TeleportTarget::Exact(Vector3::new(5.0, 70.0, -3.0), 0.0, 0.0),
            TeleportCause::Command,
            &DenyAll,
        )
        .unwrap_err();
        assert_eq!(err, TeleportError::EntryDenied);
        let after = handle.pos();
        assert_eq!((after.x, after.y, after.z), (before.x, before.y, before.z));
    }

    #[test]
    fn the_cause_reaches_the_observer() {
        #[derive(Default)]
        struct Recorder {
            causes: Mutex<Vec<TeleportCause>>,
        }
        impl TeleportHooks for Recorder {
            fn after_teleport(
                &self,
                _h: &PlayerHandle,
                _from: PosLook,
                _to: PosLook,
                cause: TeleportCause,
            ) {
                self.causes.lock().unwrap().push(cause);
            }
        }
        let handle = handle();
        let registry = registry_with(&handle);
        let world = TestWorld::default();
        let recorder = Recorder::default();
        for cause in [TeleportCause::Command, TeleportCause::Portal] {
            teleport_with(
                &handle,
                &registry,
                &world,
                TeleportTarget::Exact(Vector3::new(0.0, 64.0, 0.0), 0.0, 0.0),
                cause,
                &recorder,
            )
            .unwrap();
        }
        assert_eq!(
            *recorder.causes.lock().unwrap(),
            vec![TeleportCause::Command, TeleportCause::Portal]
        );
    }

    #[test]
    fn hooks_are_asked_before_the_move_with_the_destination() {
        struct Checker {
            seen: Mutex<Option<(Vector3, TeleportCause)>>,
        }
        impl TeleportHooks for Checker {
            fn allow_entry(&self, _h: &PlayerHandle, to: Vector3, cause: TeleportCause) -> bool {
                *self.seen.lock().unwrap() = Some((to, cause));
                true
            }
        }
        let handle = handle();
        let registry = registry_with(&handle);
        let world = TestWorld::default();
        let checker = Checker {
            seen: Mutex::new(None),
        };
        teleport_with(
            &handle,
            &registry,
            &world,
            TeleportTarget::Exact(Vector3::new(1.0, 2.0, 3.0), 0.0, 0.0),
            TeleportCause::Plugin,
            &checker,
        )
        .unwrap();
        let (to, cause) = checker.seen.lock().unwrap().unwrap();
        assert_eq!(to, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(cause, TeleportCause::Plugin);
    }

    #[test]
    fn hazard_names_are_recognised_with_or_without_a_prefix() {
        for name in ["minecraft:lava", "lava", "minecraft:soul_campfire"] {
            assert!(is_hazard_name(name), "{name}");
        }
        for name in ["minecraft:stone", "air", "minecraft:water"] {
            assert!(!is_hazard_name(name), "{name}");
        }
    }

    #[test]
    fn searching_a_far_column_loads_it() {
        // The real engine world: a search far from spawn has to materialise the
        // column on demand, which `get_block` does through `ensure_column`.
        let world = aether_api::World::new(
            aether_api::MemStore::new(),
            aether_api::FlatGenerator::classic(),
        );
        // Materialise spawn so the baseline is not "nothing loaded yet".
        let _ = world.get_block(0, 0, 0);
        let before = world.resident_sections();
        let spot = find_safe_spot(
            &world,
            BlockPos::new(1000, 64, 1000),
            0,
            SafeSpotConfig::default(),
        )
        .unwrap();
        // The classic superflat surface: grass at y=3, feet at y=4.
        assert_eq!(spot.y, 4);
        assert!(
            world.resident_sections() > before,
            "the far column should have been materialised"
        );
    }

    #[test]
    fn a_safe_target_stays_where_asked_not_on_the_surface() {
        // A floating platform over ground, with the world reporting a surface
        // hint. A `Safe` teleport must honour the requested Y instead of being
        // dragged down to the natural surface (the surface preference belongs
        // to `/rtp`/`/spawn`, not to a plugin's chosen home).
        let mut w = TestWorld::default();
        w.set(0, 3, 0, "minecraft:grass_block");
        w.set(0, 10, 0, "minecraft:stone");
        w.surface = Some(4);
        let handle = handle();
        let registry = registry_with(&handle);
        let out = teleport(
            &handle,
            &registry,
            &w,
            TeleportTarget::Safe(BlockPos::new(0, 11, 0)),
            TeleportCause::Plugin,
        )
        .unwrap();
        assert_eq!(out, BlockPos::new(0, 11, 0).feet());
    }

    #[test]
    fn a_safe_target_passes_the_resolved_position_to_the_hook() {
        struct Checker {
            seen: Mutex<Option<Vector3>>,
        }
        impl TeleportHooks for Checker {
            fn allow_entry(&self, _h: &PlayerHandle, to: Vector3, _c: TeleportCause) -> bool {
                *self.seen.lock().unwrap() = Some(to);
                true
            }
        }
        let handle = handle();
        let registry = registry_with(&handle);
        let world = cave();
        let checker = Checker {
            seen: Mutex::new(None),
        };
        teleport_with(
            &handle,
            &registry,
            &world,
            TeleportTarget::Safe(BlockPos::new(0, 15, 0)),
            TeleportCause::Plugin,
            &checker,
        )
        .unwrap();
        assert_eq!(
            checker.seen.lock().unwrap().unwrap(),
            BlockPos::new(0, 11, 0).feet()
        );
    }

    #[test]
    fn a_search_near_the_coordinate_limit_does_not_overflow() {
        // Public API, so a near-`i32::MAX`/`i32::MIN` origin must not panic in a
        // debug build when the ring deltas are added.
        let world = TestWorld::default();
        let config = SafeSpotConfig {
            prefer_surface: false,
            ..SafeSpotConfig::default()
        };
        assert_eq!(
            find_safe_spot(
                &world,
                BlockPos::new(i32::MAX - 1, 64, i32::MIN + 1),
                3,
                config,
            ),
            None
        );
        // A minimal radius span, so the random path takes the coordinate-add
        // branch without a huge loop.
        let mut rng = Rng::new(1);
        assert_eq!(
            random_safe_spot(
                &world,
                BlockPos::new(i32::MAX - 1, 64, i32::MIN + 1),
                u32::MAX,
                u32::MAX,
                config,
                &mut rng,
            ),
            None
        );
    }

    #[test]
    fn a_random_spot_respects_the_radius_band() {
        // Ground everywhere in a wide square, so every sampled column resolves.
        let mut w = TestWorld::default();
        for x in -200..=200 {
            for z in -200..=200 {
                w.set(x, 3, z, "minecraft:grass_block");
            }
        }
        let config = SafeSpotConfig {
            prefer_surface: false,
            ..SafeSpotConfig::default()
        };
        let mut rng = Rng::new(3);
        for _ in 0..20 {
            let spot =
                random_safe_spot(&w, BlockPos::new(0, 64, 0), 100, 150, config, &mut rng).unwrap();
            let dist = (spot.x as f64).hypot(spot.z as f64);
            assert!(
                (99.0..=151.0).contains(&dist),
                "distance {dist} left the [100, 150] band"
            );
        }
        // A radius span past `i32::MAX` must not panic or wrap.
        let mut rng = Rng::new(7);
        let _ = random_safe_spot(&w, BlockPos::new(0, 64, 0), 0, u32::MAX, config, &mut rng);
    }
}
