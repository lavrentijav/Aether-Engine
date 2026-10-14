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

    /// Schedule `pos` to be re-evaluated after `delay` game ticks.
    ///
    /// Scheduling the same position again while it is pending is a no-op: the
    /// first delay wins, as in vanilla.
    fn schedule_block_tick(&mut self, pos: BlockPos, delay: u32, priority: TickPriority);

    /// Whether a tick is already pending for `pos` *this* game tick.
    ///
    /// Used to avoid scheduling the same tick twice. Defaults to `false`.
    fn will_tick_this_tick(&self, _pos: BlockPos) -> bool {
        false
    }

    /// The current game time in ticks. Redstone torches need it to burn out.
    fn game_time(&self) -> u64 {
        0
    }

    /// Record that the torch at `pos` toggled at `when`.
    ///
    /// Only redstone torches call this; a world that never hosts one can leave
    /// the default in place.
    fn push_toggle(&mut self, _pos: BlockPos, _when: u64) {}

    /// How many times the torch at `pos` has toggled at or after `since`.
    fn toggles_since(&self, _pos: BlockPos, _since: u64) -> u32 {
        0
    }
}

/// An in-memory world: a map of states, a tick queue and enough bookkeeping for
/// redstone torches.
///
/// Enough to build and run a circuit in a test. Not a real world — it has one
/// flat coordinate space, no storage and no entities.
#[derive(Debug, Default, Clone)]
pub struct MemWorld {
    blocks: HashMap<BlockPos, BlockStateId>,
    /// `(due tick, priority, sequence) -> position`. The sequence keeps the
    /// queue stable for identical `(due, priority)` pairs.
    due: BTreeMap<(u64, i8, u64), BlockPos>,
    pending: HashMap<BlockPos, u64>,
    seq: u64,
    game_time: u64,
    toggles: HashMap<BlockPos, Vec<u64>>,
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
    /// exactly as vanilla's tick loop does within a tick.
    pub fn step(&mut self, engine: &Redstone) {
        self.game_time += 1;
        loop {
            let next = self.due.keys().next().copied();
            let Some(key) = next else { break };
            if key.0 > self.game_time {
                break;
            }
            let pos = self.due.remove(&key).expect("key just read");
            self.pending.remove(&pos);
            engine.on_scheduled_tick(self, pos);
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

    fn schedule_block_tick(&mut self, pos: BlockPos, delay: u32, priority: TickPriority) {
        if self.pending.contains_key(&pos) {
            return;
        }
        let due = self.game_time + u64::from(delay);
        self.pending.insert(pos, due);
        self.seq += 1;
        self.due.insert((due, priority.value(), self.seq), pos);
    }

    fn will_tick_this_tick(&self, pos: BlockPos) -> bool {
        self.pending.get(&pos) == Some(&self.game_time)
    }

    fn game_time(&self) -> u64 {
        self.game_time
    }

    fn push_toggle(&mut self, pos: BlockPos, when: u64) {
        self.toggles.entry(pos).or_default().push(when);
    }

    fn toggles_since(&self, pos: BlockPos, since: u64) -> u32 {
        self.toggles
            .get(&pos)
            .map(|v| v.iter().filter(|&&w| w >= since).count() as u32)
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

    fn schedule_block_tick(&mut self, pos: BlockPos, delay: u32, priority: TickPriority) {
        self.inner.schedule_block_tick(pos, delay, priority);
    }

    fn will_tick_this_tick(&self, pos: BlockPos) -> bool {
        self.inner.will_tick_this_tick(pos)
    }

    fn game_time(&self) -> u64 {
        self.inner.game_time()
    }

    fn push_toggle(&mut self, pos: BlockPos, when: u64) {
        self.inner.push_toggle(pos, when);
    }

    fn toggles_since(&self, pos: BlockPos, since: u64) -> u32 {
        self.inner.toggles_since(pos, since)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduling_is_first_wins() {
        let mut w = MemWorld::new();
        let p = BlockPos::new(0, 0, 0);
        w.schedule_block_tick(p, 5, TickPriority::Normal);
        w.schedule_block_tick(p, 1, TickPriority::Normal);
        // The later, shorter schedule is ignored: position already pending.
        assert_eq!(w.pending.get(&p), Some(&5));
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
