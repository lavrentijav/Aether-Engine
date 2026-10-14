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

    /// The current game time in ticks. Redstone torches need it to burn out.
    fn game_time(&self) -> u64 {
        0
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
}

impl MemWorld {
    /// A fresh, empty world at game time 0.
    pub fn new() -> Self {
        Self::default()
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

    fn game_time(&self) -> u64 {
        self.game_time
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

    fn game_time(&self) -> u64 {
        self.inner.game_time()
    }

    fn record_toggle(&mut self, pos: BlockPos, when: u64) {
        self.inner.record_toggle(pos, when);
    }

    fn toggle_count(&self, pos: BlockPos, since: u64) -> u32 {
        self.inner.toggle_count(pos, since)
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
