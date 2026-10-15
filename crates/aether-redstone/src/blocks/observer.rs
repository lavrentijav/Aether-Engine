//! The observer: `ObserverBlock`.
//!
//! An observer watches the block on its `facing` side. When that block's shape
//! updates, the observer pulses: it turns `powered` for two ticks, then off,
//! notifying the block behind it. Wire connects to the observer's facing side
//! whether or not it is powered (`RedStoneWireBlock.shouldConnectTo`).

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::blocks::{diode, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// The observer.
pub struct Observer;

impl BlockBehaviour for Observer {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn connects_to_wire(&self, state: BlockStateId, dir: Option<Direction>) -> bool {
        // `RedStoneWireBlock.shouldConnectTo` special-cases the observer: wire
        // connects on its facing side, powered or not.
        dir == Some(props::direction(state))
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if props::powered(state) && props::direction(state) == dir {
            15
        } else {
            0
        }
    }

    fn get_direct_signal(
        &self,
        ctx: &SignalCtx,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        self.get_signal(ctx, state, pos, dir)
    }

    fn neighbor_changed(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        // The observer reacts to shape updates, not to plain neighbour changes.
    }

    fn on_place(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos, old: BlockStateId) {
        // Vanilla clears a stale `powered` when the block changes, unless a tick
        // is already pending (`hasScheduledTick`).
        if crate::behaviour::block_id_of(old) != crate::behaviour::block_id_of(state)
            && props::powered(state)
            && !ctx.has_scheduled_tick(pos)
        {
            let new = props::with(state, "powered", "false").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::RESET);
            diode::update_neighbors_in_front(ctx, pos, new);
        }
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        diode::update_neighbors_in_front(ctx, pos, state);
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        // `ObserverBlock.updateShape`: a shape change on the watched side starts
        // the pulse.
        if props::direction(state) == dir && !props::powered(state) {
            start_signal(ctx, pos);
        }
        None
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::powered(state) {
            let new = props::with(state, "powered", "false").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::CLIENTS);
        } else {
            let new = props::with(state, "powered", "true").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::CLIENTS);
            ctx.schedule_tick(pos, 2, TickPriority::Normal);
        }
        diode::update_neighbors_in_front(ctx, pos, state);
    }
}

/// `ObserverBlock.startSignal`: schedule the two-tick pulse unless one is
/// already pending (`hasScheduledTick`).
fn start_signal(ctx: &mut Context, pos: BlockPos) {
    if !ctx.has_scheduled_tick(pos) {
        ctx.schedule_tick(pos, 2, TickPriority::Normal);
    }
}
