//! What the redstone engine needs from a world, and a test world that provides
//! it without a server.
//!
//! The engine never touches storage. It reads and writes block states through
//! [`RedstoneView`], so the same code runs against the real world later and
//! against [`MemWorld`] today. Keeping the surface this small is what lets the
//! whole engine be exercised by tests that build a circuit in a few lines.

use std::collections::{BTreeMap, HashMap};

use aether_world::BlockRegistry;
use aether_world::BlockStateId;

use crate::engine::Redstone;
use crate::pos::BlockPos;

/// The window, in game ticks, over which redstone-torch toggles are counted for
/// burnout — vanilla's `RECENT_TOGGLE_TIMER`.
pub const TOGGLE_WINDOW: u64 = 60;

/// How urgent a scheduled block tick is.
///
/// Lower runs first. The values are vanilla's `TickPriority`; the names are the
/// ones the engine uses. Only the priority relative to other ticks matters, so
/// a scheduler may order by the numeric value directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TickPriority {
    /// `TickPriority.EXTREMELY_HIGH` — a repeater whose output feeds a diode.
    ExtremelyHigh,
    /// `TickPriority.VERY_HIGH` — a repeater turning off.
    VeryHigh,
    /// `TickPriority.HIGH` — a repeater turning on.
    High,
    /// `TickPriority.NORMAL`.
    Normal,
    /// `TickPriority.LOW`.
    Low,
    /// `TickPriority.VERY_LOW`.
    VeryLow,
    /// `TickPriority.EXTREMELY_LOW`.
    ExtremelyLow,
}

impl TickPriority {
    /// The numeric value vanilla sorts by (lower is earlier).
    pub const fn value(self) -> i8 {
        match self {
            TickPriority::ExtremelyHigh => -3,
            TickPriority::VeryHigh => -2,
            TickPriority::High => -1,
            TickPriority::Normal => 0,
            TickPriority::Low => 1,
            TickPriority::VeryLow => 2,
            TickPriority::ExtremelyLow => 3,
        }
    }
}

/// `Block.UpdateFlags`: what a block write should do besides storing the state.
///
/// The bits are vanilla's (`net.minecraft.world.level.block.Block`). The engine
/// reads them in [`crate::behaviour::Context::set_block`] to decide whether to
/// run the removal hook, the placement hook, the neighbour updates and the
/// shape updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SetBlockFlags(u32);

impl SetBlockFlags {
    /// No side effects at all.
    pub const NONE: Self = Self(0);
    /// `UPDATE_NEIGHBORS` — notify the six neighbours.
    pub const UPDATE_NEIGHBORS: Self = Self(1);
    /// `UPDATE_CLIENTS` — send the block to clients.
    pub const UPDATE_CLIENTS: Self = Self(2);
    /// `UPDATE_INVISIBLE`.
    pub const UPDATE_INVISIBLE: Self = Self(4);
    /// `UPDATE_IMMEDIATE`.
    pub const UPDATE_IMMEDIATE: Self = Self(8);
    /// `UPDATE_KNOWN_SHAPE` — suppress the shape updates.
    pub const UPDATE_KNOWN_SHAPE: Self = Self(16);
    /// `UPDATE_SUPPRESS_DROPS`.
    pub const UPDATE_SUPPRESS_DROPS: Self = Self(32);
    /// `UPDATE_MOVE_BY_PISTON`.
    pub const UPDATE_MOVE_BY_PISTON: Self = Self(64);
    /// `UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE` — a shape update skips redstone wire.
    pub const UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE: Self = Self(128);
    /// `UPDATE_SKIP_ON_PLACE` — suppress the placement hook.
    pub const UPDATE_SKIP_ON_PLACE: Self = Self(512);
    /// `UPDATE_NONE`.
    pub const UPDATE_NONE: Self = Self(260);

    /// The usual "place a block" flags: neighbour notify plus clients — vanilla's
    /// `setBlockAndUpdate` (`3`).
    pub const PLACE: Self = Self(3);

    /// The flags redstone state changes usually use: clients only (`2`). The
    /// engine still runs the placement hook, which is where the component does
    /// its own neighbour work.
    pub const CLIENTS: Self = Self(2);

    /// Combine two flag sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Remove `other` from this set.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether every bit in `other` is set here.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// The raw bits.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for SetBlockFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// A redstone orientation (`net.minecraft.world.level.redstone.Orientation`).
///
/// 1.21.11 threads this through neighbour updates so the experimental evaluator
/// and direction-aware components (pistons, observers) know which face an
/// update came from. The default evaluator ignores it, so this stage carries it
/// but does not act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Orientation {
    /// The front face.
    pub front: crate::dir::Direction,
    /// The up face.
    pub up: crate::dir::Direction,
}

impl Orientation {
    /// An orientation with the given front and up faces.
    pub const fn new(front: crate::dir::Direction, up: crate::dir::Direction) -> Self {
        Self { front, up }
    }
}

/// The world as the redstone engine sees it.
///
/// A host wires this to whatever it actually stores blocks in. The engine only
/// ever calls these; it holds no state of its own, so the same `Redstone`
/// instance can drive any number of worlds.
pub trait RedstoneView {
    /// The block state at `pos`. Air for anything unset.
    fn get_block(&self, pos: BlockPos) -> BlockStateId;

    /// Write a block state at `pos`.
    ///
    /// This is the raw write: it does **not** notify neighbours or schedule
    /// anything. The engine generates its own update cascade around it.
    fn set_block(&mut self, pos: BlockPos, state: BlockStateId);

    /// Schedule `pos` to be re-evaluated after `delay` game ticks, for `block`.
    ///
    /// Vanilla keys a scheduled tick by `(block, pos)`, and when the tick runs
    /// it is dropped unless the cell still holds that block
    /// (`ServerLevel.tickBlock`). Scheduling the same `(block, pos)` again while
    /// it is pending is a no-op: the first delay wins.
    fn schedule_block_tick(
        &mut self,
        block: u16,
        pos: BlockPos,
        delay: u32,
        priority: TickPriority,
    );

    /// Whether a tick for `(block, pos)` is already pending *this* game tick.
    ///
    /// Used to avoid scheduling the same tick twice. Defaults to `false`.
    fn will_tick_this_tick(&self, _block: u16, _pos: BlockPos) -> bool {
        false
    }

    /// Whether a tick for `(block, pos)` is pending at all — vanilla's
    /// `LevelTicks.hasScheduledTick`, which a target or observer checks before
    /// starting a new timer (as opposed to [`Self::will_tick_this_tick`], which
    /// is only true within the current tick). Defaults to `false`.
    fn has_scheduled_tick(&self, _block: u16, _pos: BlockPos) -> bool {
        false
    }

    /// The current game time in ticks. Redstone torches need it to burn out.
    fn game_time(&self) -> u64 {
        0
    }

    /// The chain limit for one neighbour-update cascade — vanilla's gamerule
    /// `maxChainedNeighborUpdates` (default **1,000,000**). Past it the rest of a
    /// single cascade is dropped, which bounds a runaway circuit.
    ///
    /// This is per level in vanilla, so it belongs on the world, not the
    /// stateless engine. `usize::MAX` disables the limit.
    fn max_chained_neighbor_updates(&self) -> usize {
        1_000_000
    }

    /// Record that the redstone torch at `pos` toggled at game time `when`.
    ///
    /// This is vanilla's `RECENT_TOGGLES`. It lives on the **world**, not the
    /// engine, because a level's torch burnout is per level: one engine may
    /// drive several worlds, and their logs must not be shared. The default is
    /// a no-op, so a world that never hosts a torch needs nothing; a world that
    /// does should prune entries older than 60 ticks.
    fn record_toggle(&mut self, _pos: BlockPos, _when: u64) {}

    /// How many toggles the torch at `pos` has recorded at or after `since`.
    fn toggle_count(&self, _pos: BlockPos, _since: u64) -> u32 {
        0
    }

    /// Emit the item drops for a block the engine is destroying at `pos` (a
    /// wire or torch whose support is gone, so its `updateShape` returned air).
    ///
    /// **Dropping is delegated to the host, not implemented here.** The crate
    /// has no item model — it cannot name or spawn an item — so vanilla's
    /// `destroyBlock(pos, drop = true)`, which runs the block's loot table, is
    /// left to the world. The engine calls this once per destroyed block, before
    /// the cell is replaced with air, and only when
    /// [`SetBlockFlags::UPDATE_SUPPRESS_DROPS`] is not set (so a piston or a
    /// command that suppresses drops stays silent). The default does nothing; a
    /// host that wants items implements it.
    fn drop_block(&mut self, _pos: BlockPos, _state: BlockStateId) {}

    /// The **analog output** signal (0–15) the block at `pos` provides to a
    /// redstone comparator — a container's fill level, a cake's bites, a
    /// composter's level, and so on. `None` if the block has no analog output.
    ///
    /// The crate has no inventory model, so the host computes this from
    /// whatever it stores: vanilla's `Container.getRedstoneSignalFromContainer`
    /// is `floor(1 + fillFraction * 14)`. The default reports no container.
    fn analog_output(&self, _pos: BlockPos) -> Option<u8> {
        None
    }

    /// A daylight detector's raw daylight at `pos`: vanilla's
    /// `getBrightness(LightLayer.SKY, pos) - getSkyDarken()`. May be negative.
    /// The default is 0 (night).
    fn sky_light(&self, _pos: BlockPos) -> i32 {
        0
    }

    /// The sun angle at `pos` in radians (vanilla's `SUN_ANGLE` environment
    /// attribute). The default is 0 (midnight/noon of the raw term).
    fn sun_angle(&self, _pos: BlockPos) -> f32 {
        0.0
    }

    /// The number of entities whose touch box overlaps the cell `pos` — what a
    /// pressure plate reads to decide whether it is pressed
    /// (`BasePressurePlateBlock.getEntityCount` over its `TOUCH_AABB`).
    ///
    /// The crate has no entity model, so the host counts them. The default is
    /// none (an unpressed plate).
    fn entity_count(&self, _pos: BlockPos) -> usize {
        0
    }
}

/// An in-memory world: a map of states, a block-tick queue and the redstone
/// torch toggle log.
///
/// Enough to build and run a circuit in a test. Not a real world — it has one
/// flat coordinate space, no storage and no entities.
#[derive(Debug, Default, Clone)]
pub struct MemWorld {
    blocks: HashMap<BlockPos, BlockStateId>,
    /// `(due tick, priority, sequence) -> (block, position)`. The sequence
    /// keeps the queue stable for identical `(due, priority)` pairs.
    due: BTreeMap<(u64, i8, u64), (u16, BlockPos)>,
    pending: HashMap<(u16, BlockPos), u64>,
    seq: u64,
    game_time: u64,
    /// Per-position torch toggle times, pruned to the 60-tick burnout window.
    toggles: HashMap<BlockPos, std::collections::VecDeque<u64>>,
    /// The configured chain limit; `None` means vanilla's 1,000,000.
    max_chained: Option<usize>,
    /// Blocks the engine destroyed, as `(pos, state)`, in order.
    drops: Vec<(BlockPos, BlockStateId)>,
    /// Analog output a test sets for a container-like cell.
    analog: HashMap<BlockPos, u8>,
    /// The world's daylight: `(sky_light, sun_angle)`.
    daylight: (i32, f32),
    /// The entity count a test sets for a pressure-plate cell.
    entities: HashMap<BlockPos, usize>,
}

impl MemWorld {
    /// A fresh, empty world at game time 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the neighbour-update chain limit (vanilla's
    /// `maxChainedNeighborUpdates`). For tests that need to observe truncation.
    pub fn with_max_chained(mut self, limit: usize) -> Self {
        self.max_chained = Some(limit);
        self
    }

    /// Put the block whose state name is `name` at `pos`.
    ///
    /// The name may carry properties — `minecraft:repeater[facing=east,delay=3]`
    /// — and may be partial; unnamed properties keep their defaults.
    pub fn put(&mut self, pos: BlockPos, name: &str) {
        let state = BlockRegistry::new()
            .get(name)
            .unwrap_or_else(|| panic!("unknown block state `{name}`"));
        self.blocks.insert(pos, state);
    }

    /// The raw state at `pos`.
    pub fn state(&self, pos: BlockPos) -> BlockStateId {
        self.blocks.get(&pos).copied().unwrap_or(BlockStateId::AIR)
    }

    /// The state name at `pos`, e.g. `minecraft:redstone_wire[...]`.
    pub fn name(&self, pos: BlockPos) -> Option<String> {
        BlockRegistry::new().name_of(self.state(pos))
    }

    /// The block name at `pos`, without properties.
    pub fn block_name(&self, pos: BlockPos) -> Option<&'static str> {
        crate::props::name_of(self.state(pos))
    }

    /// The current game time.
    pub fn now(&self) -> u64 {
        self.game_time
    }

    /// The blocks the engine destroyed, as `(pos, state)`, oldest first.
    pub fn dropped(&self) -> &[(BlockPos, BlockStateId)] {
        &self.drops
    }

    /// Set the analog output (0–15) a container-like cell reports.
    pub fn set_analog_output(&mut self, pos: BlockPos, signal: u8) {
        self.analog.insert(pos, signal);
    }

    /// Set the world's daylight: the raw sky light and the sun angle (radians),
    /// as a daylight detector reads them.
    pub fn set_daylight(&mut self, sky_light: i32, sun_angle: f32) {
        self.daylight = (sky_light, sun_angle);
    }

    /// Set how many entities are touching the cell `pos`, as a pressure plate
    /// would see.
    pub fn set_entities(&mut self, pos: BlockPos, count: usize) {
        self.entities.insert(pos, count);
    }

    /// Advance one game tick and run every block tick that has come due.
    ///
    /// Ticks are drained until none remain for `now` or earlier, so a tick that
    /// schedules another for the same instant is resolved in the same step,
    /// exactly as vanilla's tick loop does within a tick. Each due tick carries
    /// the block it was scheduled for; the engine drops it unless the cell still
    /// holds that block (`ServerLevel.tickBlock`).
    pub fn step(&mut self, engine: &Redstone) {
        self.game_time += 1;
        loop {
            let next = self.due.keys().next().copied();
            let Some(key) = next else { break };
            if key.0 > self.game_time {
                break;
            }
            let (block, pos) = self.due.remove(&key).expect("key just read");
            self.pending.remove(&(block, pos));
            engine.on_scheduled_tick(self, block, pos);
        }
    }

    /// Advance `ticks` game ticks.
    pub fn run(&mut self, engine: &Redstone, ticks: u32) {
        for _ in 0..ticks {
            self.step(engine);
        }
    }
}

impl RedstoneView for MemWorld {
    fn get_block(&self, pos: BlockPos) -> BlockStateId {
        self.state(pos)
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.blocks.insert(pos, state);
    }

    fn schedule_block_tick(
        &mut self,
        block: u16,
        pos: BlockPos,
        delay: u32,
        priority: TickPriority,
    ) {
        let key = (block, pos);
        if self.pending.contains_key(&key) {
            return;
        }
        let due = self.game_time + u64::from(delay);
        self.pending.insert(key, due);
        self.seq += 1;
        self.due.insert((due, priority.value(), self.seq), key);
    }

    fn will_tick_this_tick(&self, block: u16, pos: BlockPos) -> bool {
        self.pending.get(&(block, pos)) == Some(&self.game_time)
    }

    fn has_scheduled_tick(&self, block: u16, pos: BlockPos) -> bool {
        self.pending.contains_key(&(block, pos))
    }

    fn game_time(&self) -> u64 {
        self.game_time
    }

    fn max_chained_neighbor_updates(&self) -> usize {
        self.max_chained.unwrap_or(1_000_000)
    }

    fn record_toggle(&mut self, pos: BlockPos, when: u64) {
        let log = self.toggles.entry(pos).or_default();
        // Prune entries outside the 60-tick burnout window, so a clock torch
        // does not grow the log without bound.
        while let Some(&front) = log.front() {
            if when.saturating_sub(front) > TOGGLE_WINDOW {
                log.pop_front();
            } else {
                break;
            }
        }
        log.push_back(when);
    }

    fn toggle_count(&self, pos: BlockPos, since: u64) -> u32 {
        self.toggles
            .get(&pos)
            .map(|log| log.iter().filter(|&&w| w >= since).count() as u32)
            .unwrap_or(0)
    }

    fn drop_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.drops.push((pos, state));
    }

    fn analog_output(&self, pos: BlockPos) -> Option<u8> {
        self.analog.get(&pos).copied()
    }

    fn sky_light(&self, _pos: BlockPos) -> i32 {
        self.daylight.0
    }

    fn sun_angle(&self, _pos: BlockPos) -> f32 {
        self.daylight.1
    }

    fn entity_count(&self, pos: BlockPos) -> usize {
        self.entities.get(&pos).copied().unwrap_or(0)
    }
}

/// A world that records every state write, so a test can assert the exact
/// sequence it produced.
#[derive(Debug, Default)]
pub struct TraceWorld {
    inner: MemWorld,
    writes: Vec<(BlockPos, BlockStateId)>,
}

impl TraceWorld {
    /// A fresh tracing world.
    pub fn new() -> Self {
        Self::default()
    }

    /// The writes recorded so far, oldest first.
    pub fn writes(&self) -> &[(BlockPos, BlockStateId)] {
        &self.writes
    }

    /// Everything written, as `(pos, state name)` pairs.
    pub fn names(&self) -> Vec<(BlockPos, Option<String>)> {
        let reg = BlockRegistry::new();
        self.writes
            .iter()
            .map(|(p, s)| (*p, reg.name_of(*s)))
            .collect()
    }

    /// Put a named block, recording the write too.
    pub fn put(&mut self, pos: BlockPos, name: &str) {
        self.inner.put(pos, name);
        self.writes.push((pos, self.inner.state(pos)));
    }
}

impl RedstoneView for TraceWorld {
    fn get_block(&self, pos: BlockPos) -> BlockStateId {
        self.inner.get_block(pos)
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.writes.push((pos, state));
        self.inner.set_block(pos, state);
    }

    fn schedule_block_tick(
        &mut self,
        block: u16,
        pos: BlockPos,
        delay: u32,
        priority: TickPriority,
    ) {
        self.inner.schedule_block_tick(block, pos, delay, priority);
    }

    fn will_tick_this_tick(&self, block: u16, pos: BlockPos) -> bool {
        self.inner.will_tick_this_tick(block, pos)
    }

    fn has_scheduled_tick(&self, block: u16, pos: BlockPos) -> bool {
        self.inner.has_scheduled_tick(block, pos)
    }

    fn game_time(&self) -> u64 {
        self.inner.game_time()
    }

    fn max_chained_neighbor_updates(&self) -> usize {
        self.inner.max_chained_neighbor_updates()
    }

    fn record_toggle(&mut self, pos: BlockPos, when: u64) {
        self.inner.record_toggle(pos, when);
    }

    fn toggle_count(&self, pos: BlockPos, since: u64) -> u32 {
        self.inner.toggle_count(pos, since)
    }

    fn drop_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.inner.drop_block(pos, state);
    }

    fn analog_output(&self, pos: BlockPos) -> Option<u8> {
        self.inner.analog_output(pos)
    }

    fn sky_light(&self, pos: BlockPos) -> i32 {
        self.inner.sky_light(pos)
    }

    fn sun_angle(&self, pos: BlockPos) -> f32 {
        self.inner.sun_angle(pos)
    }

    fn entity_count(&self, pos: BlockPos) -> usize {
        self.inner.entity_count(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduling_is_first_wins() {
        let mut w = MemWorld::new();
        let p = BlockPos::new(0, 0, 0);
        w.schedule_block_tick(1, p, 5, TickPriority::Normal);
        w.schedule_block_tick(1, p, 1, TickPriority::Normal);
        // The later, shorter schedule is ignored: `(block, pos)` already pending.
        assert_eq!(w.pending.get(&(1, p)), Some(&5));
    }

    #[test]
    fn step_runs_ticks_at_their_tick() {
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        w.put(BlockPos::new(0, 0, 0), "minecraft:stone");
        // No scheduled ticks: stepping just advances time.
        w.run(&engine, 3);
        assert_eq!(w.now(), 3);
    }
}
