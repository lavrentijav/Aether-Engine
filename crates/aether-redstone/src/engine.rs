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
//! `UPDATE_ORDER` (west, east, down, up, north, south). This stage (`R6`) still
//! drains them breadth-first; R7 replaces that with a port of vanilla's
//! depth-first `CollectingNeighborUpdater`.

use aether_world::BlockStateId;

use crate::behaviour::{self, Context};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::view::RedstoneView;

/// A hard cap on the update cascade, so a malformed circuit cannot spin a call
/// forever. A real cascade is a few hundred updates; this is far above that.
const MAX_UPDATES: usize = 1_000_000;

/// The redstone engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct Redstone;

impl Redstone {
    /// A new engine. It holds nothing; this is a constructor for symmetry.
    pub const fn new() -> Self {
        Redstone
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
    // Entry points
    // -----------------------------------------------------------------------

    /// A block was placed at `pos`. The host writes it first, then calls this.
    pub fn on_placed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut ctx = Context::new(view);
        ctx.update_neighbors_at(pos);
        ctx.queue.push_back(pos);
        let state = ctx.get_block(pos);
        if behaviour::behaviour(state).is_diode() {
            behaviour::behaviour(state).on_place(&mut ctx, state, pos, state);
        }
        drain(&mut ctx);
    }

    /// A block was removed from `pos`, whose old state was `old`.
    pub fn on_removed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos, old: BlockStateId) {
        let mut ctx = Context::new(view);
        behaviour::behaviour(old).on_remove(&mut ctx, old, pos);
        drain(&mut ctx);
    }

    /// A neighbour of the block at `pos` (the block at `from`) changed.
    pub fn on_neighbour_changed<V: RedstoneView>(
        &self,
        view: &mut V,
        pos: BlockPos,
        from: BlockPos,
    ) {
        let _ = from;
        let mut ctx = Context::new(view);
        ctx.queue.push_back(pos);
        drain(&mut ctx);
    }

    /// A block tick scheduled for `pos` has come due.
    pub fn on_scheduled_tick<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut ctx = Context::new(view);
        ctx.dispatch_tick(pos);
        drain(&mut ctx);
    }

    /// A player used the block at `pos`: a lever pulls, a button presses, a
    /// repeater cycles its delay, a wire toggles cross/dot.
    pub fn on_use<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut ctx = Context::new(view);
        ctx.dispatch_use(pos);
        drain(&mut ctx);
    }
}

/// Run the pending neighbour updates, the way vanilla's neighbour updater does.
fn drain(ctx: &mut Context) {
    let mut guard = 0usize;
    while let Some(pos) = ctx.queue.pop_front() {
        guard += 1;
        if guard > MAX_UPDATES {
            break;
        }
        ctx.dispatch_neighbor_changed(pos, None);
    }
}
