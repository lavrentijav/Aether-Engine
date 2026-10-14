//! The redstone comparator: `ComparatorBlock` on top of `DiodeBlock`.
//!
//! Compare mode passes the input through; subtract mode takes the side signal
//! away from the front input. The front input is analog when the block in front
//! (or the block behind a conductor in front) reports an analog output — a
//! container's fill level, via [`crate::view::RedstoneView::analog_output`].
//!
//! The output value lives in a block entity in vanilla; this engine computes it
//! from the current world on each query, so the steady state matches and no
//! per-comparator storage is needed. A neighbour change schedules a two-tick
//! tick to nudge the block behind, as `refreshOutputState` does.

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::blocks::{diode, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// The redstone comparator.
pub struct Comparator;

impl BlockBehaviour for Comparator {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn is_diode(&self) -> bool {
        true
    }

    fn get_signal(
        &self,
        ctx: &SignalCtx,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if props::powered(state) && props::facing(state) == dir {
            output_signal(ctx, pos, state)
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
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        // `ComparatorBlock.checkTickOnNeighbor`: schedule a two-tick refresh
        // unless one is already pending. Priority is HIGH when the comparator
        // faces a diode, else NORMAL.
        if !ctx.will_tick_this_tick(pos) {
            let priority = if diode::should_prioritize(&ctx.signals(), pos, state) {
                TickPriority::High
            } else {
                TickPriority::Normal
            };
            ctx.schedule_tick(pos, 2, priority);
        }
    }

    fn on_place(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos, _old: BlockStateId) {
        diode::update_neighbors_in_front(ctx, pos, state);
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        diode::update_neighbors_in_front(ctx, pos, state);
    }

    fn set_placed_by(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        // `DiodeBlock.setPlacedBy`: a diode placed onto a live input turns on
        // after one tick.
        if should_turn_on(&ctx.signals(), pos, state) {
            ctx.schedule_tick(pos, 1, TickPriority::Normal);
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        _state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        // `ComparatorBlock.updateShape(DOWN)`: pop off when the RIGID support is
        // gone. No other shape change (a comparator has no `locked` property).
        if dir == Direction::Down && !diode::supported(ctx, pos) {
            return Some(crate::behaviour::AIR);
        }
        None
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        refresh(ctx, pos, state);
    }

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        let mode = if props::get(state, "mode") == Some("subtract") {
            "compare"
        } else {
            "subtract"
        };
        let new = props::with(state, "mode", mode).unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
        refresh(ctx, pos, new);
    }
}

/// `DiodeBlock.getInputSignal` overridden for the comparator's analog input.
fn input_signal(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> u8 {
    let mut signal = diode::base_input_signal(ctx, pos, state);
    let dir = props::facing(state);
    let front = pos.offset(dir);
    if let Some(analog) = ctx.analog_output(front) {
        signal = analog;
    } else if signal < 15 && ctx.is_conductor(front) {
        // Through a conductor: read the block two cells ahead.
        if let Some(analog) = ctx.analog_output(front.offset(dir)) {
            signal = analog;
        }
    }
    signal
}

/// `ComparatorBlock.calculateOutputSignal`.
fn output_signal(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> u8 {
    let input = input_signal(ctx, pos, state);
    if input == 0 {
        return 0;
    }
    let alternate = diode::alternate_signal(ctx, pos, state, false);
    if alternate > input {
        0
    } else if props::get(state, "mode") == Some("subtract") {
        input - alternate
    } else {
        input
    }
}

/// `ComparatorBlock.shouldTurnOn`.
fn should_turn_on(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> bool {
    let input = input_signal(ctx, pos, state);
    if input == 0 {
        return false;
    }
    let alternate = diode::alternate_signal(ctx, pos, state, false);
    input > alternate || (input == alternate && props::get(state, "mode") != Some("subtract"))
}

/// `ComparatorBlock.refreshOutputState`: set `powered` and nudge the front.
fn refresh(ctx: &mut Context, pos: BlockPos, state: BlockStateId) {
    let should = should_turn_on(&ctx.signals(), pos, state);
    let is_on = props::powered(state);
    if is_on != should {
        let value = if should { "true" } else { "false" };
        let new = props::with(state, "powered", value).unwrap_or(state);
        ctx.apply(pos, new);
    }
    diode::update_neighbors_in_front(ctx, pos, state);
}
