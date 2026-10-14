//! The redstone engine: the public API and the block-behaviour dispatch.
//!
//! [`Redstone`] holds no block data. Every method is a function of the
//! [`RedstoneView`] it is handed, so the same engine drives any world and two
//! runs of the same circuit produce the same result. The pending neighbour
//! updates live on the stack for the duration of one call.
//!
//! The behaviour of a block lives in a [`crate::blocks`] module and is reached
//! through a state-indexed table ([`crate::behaviour::behaviour`]); this file
//! never matches on a block name.
//!
//! # Update order
//!
//! This is the **1.21.11 default** — the experimental "Eigencraft" wire
//! evaluator is off. Neighbour notifications are generated in vanilla's
//! `UPDATE_ORDER` (west, east, down, up, north, south) and drained by a port of
//! vanilla's depth-first `CollectingNeighborUpdater` ([`crate::behaviour`]):
//! a LIFO stack where a multi-neighbour update yields to freshly generated
//! updates before resuming, with the chain limit.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use aether_world::BlockStateId;

use crate::behaviour::{self, block_id_of, Context};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::view::{RedstoneView, SetBlockFlags};

/// The number of game ticks a torch burns out for once it toggles too often.
const BURNOUT_WINDOW: u64 = 60;

/// The redstone engine and its per-world bookkeeping.
///
/// Block data lives in the [`RedstoneView`], never here — the engine is
/// stateless with respect to the world. The one thing it owns is the
/// redstone-torch burnout log (vanilla's `RECENT_TOGGLES`), which belongs to
/// the engine rather than to the world interface.
#[derive(Debug, Default)]
pub struct Redstone {
    toggles: RefCell<HashMap<BlockPos, VecDeque<u64>>>,
}

impl Redstone {
    /// A new engine with an empty burnout log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the torch at `pos` toggled at `when`, pruning entries older
    /// than 60 ticks first — vanilla prunes at the start of every torch tick.
    pub(crate) fn record_toggle(&self, pos: BlockPos, when: u64) {
        let mut toggles = self.toggles.borrow_mut();
        let log = toggles.entry(pos).or_default();
        while let Some(&front) = log.front() {
            if when.saturating_sub(front) > BURNOUT_WINDOW {
                log.pop_front();
            } else {
                break;
            }
        }
        log.push_back(when);
    }

    /// How many toggles the torch at `pos` has recorded at or after `since`,
    /// pruning anything older on the way.
    pub(crate) fn toggle_count(&self, pos: BlockPos, since: u64) -> u32 {
        let mut toggles = self.toggles.borrow_mut();
        let Some(log) = toggles.get_mut(&pos) else {
            return 0;
        };
        while let Some(&front) = log.front() {
            if front < since {
                log.pop_front();
            } else {
                break;
            }
        }
        log.len() as u32
    }

    // -----------------------------------------------------------------------
    // Power queries
    // -----------------------------------------------------------------------

    /// The **weak** redstone signal emitted into the neighbour in `dir`.
    ///
    /// `dir` points from the receiver toward the block being read. A conductor
    /// also contributes the strong power it holds, which is how a lever on a
    /// block powers wire on the far side.
    pub fn get_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos, dir: Direction) -> u8 {
        self.signals(view).get_signal(pos, dir)
    }

    /// The **strong** signal this block provides to the neighbour in `dir`.
    pub fn get_direct_signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        self.signals(view).get_direct_signal(pos, dir)
    }

    /// The largest weak signal among a block's six neighbours.
    pub fn get_best_neighbor_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> u8 {
        self.signals(view).get_best_neighbor_signal(pos)
    }

    /// Whether any of a block's six neighbours emits a signal into it.
    pub fn has_neighbor_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> bool {
        self.signals(view).has_neighbor_signal(pos)
    }

    fn signals<'a, V: RedstoneView>(&self, view: &'a V) -> behaviour::SignalCtx<'a> {
        behaviour::SignalCtx {
            view,
            suppress_wire: false,
        }
    }

    // -----------------------------------------------------------------------
    // Block mutation
    // -----------------------------------------------------------------------

    /// Vanilla's `Level.setBlock(pos, state, flags)`.
    ///
    /// The host promises nothing was written yet; the engine runs the removal
    /// and placement hooks and the neighbour/shape updates the flags select.
    /// Returns whether the state changed.
    pub fn set_block<V: RedstoneView>(
        &self,
        view: &mut V,
        pos: BlockPos,
        state: BlockStateId,
        flags: SetBlockFlags,
    ) -> bool {
        let mut ctx = Context::new(view, self);
        ctx.set_block(pos, state, flags)
    }

    // -----------------------------------------------------------------------
    // Entry points
    // -----------------------------------------------------------------------

    /// A block was placed at `pos`. The host writes the state first, then calls
    /// this; the engine runs the placement hook and notifies the neighbours.
    pub fn on_placed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut ctx = Context::new(view, self);
        let state = ctx.get_block(pos);
        behaviour::behaviour(state).on_place(&mut ctx, state, pos, behaviour::AIR);
        ctx.update_neighbors_at_with(pos, state, None);
        ctx.update_neighbour_shapes(pos, SetBlockFlags::UPDATE_NEIGHBORS);
    }

    /// A block was removed from `pos`, whose old state was `old`. The host has
    /// already written air.
    pub fn on_removed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos, old: BlockStateId) {
        let mut ctx = Context::new(view, self);
        behaviour::behaviour(old).on_remove(&mut ctx, old, pos);
        ctx.update_neighbour_shapes(pos, SetBlockFlags::NONE);
    }

    /// A neighbour of the block at `pos` (the block at `from`) changed.
    pub fn on_neighbour_changed<V: RedstoneView>(
        &self,
        view: &mut V,
        pos: BlockPos,
        from: BlockPos,
    ) {
        let mut ctx = Context::new(view, self);
        let source = ctx.get_block(from);
        ctx.dispatch_neighbor_changed(pos, Some(behaviour::NeighborSource::plain(from, source)));
    }

    /// A block tick scheduled for `(block, pos)` has come due.
    ///
    /// The tick is dropped unless the cell still holds `block`, as
    /// `ServerLevel.tickBlock` guards with `blockState.is(block)`.
    pub fn on_scheduled_tick<V: RedstoneView>(&self, view: &mut V, block: u16, pos: BlockPos) {
        if block_id_of(view.get_block(pos)) != Some(block) {
            return;
        }
        let mut ctx = Context::new(view, self);
        ctx.dispatch_tick(pos);
    }

    /// A player used the block at `pos`: a lever pulls, a button presses, a
    /// repeater cycles its delay, a wire toggles cross/dot.
    pub fn on_use<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut ctx = Context::new(view, self);
        ctx.dispatch_use(pos);
    }
}
