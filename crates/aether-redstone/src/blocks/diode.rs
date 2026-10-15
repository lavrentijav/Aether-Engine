//! Shared `DiodeBlock` behaviour for the repeater and the comparator.
//!
//! Both are diodes: a horizontal directional block that reads an input from the
//! block in front, is optionally locked by a side diode, and drives the block
//! behind it. This module holds the pieces both share so each component module
//! stays about its own rule.

use aether_world::BlockStateId;

use crate::behaviour::{behaviour, Context, SignalCtx};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::shapes;

/// The power of a redstone wire, or 0 for anything else.
pub(crate) fn wire_power(state: BlockStateId) -> u8 {
    if behaviour(state).is_wire() {
        props::power(state)
    } else {
        0
    }
}

/// `DiodeBlock.getInputSignal`: the signal from the block in front, plus its
/// wire power.
pub(crate) fn base_input_signal(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> u8 {
    let dir = props::facing(state);
    let input = pos.offset(dir);
    let signal = ctx.get_signal(input, dir);
    if signal >= 15 {
        return signal;
    }
    signal.max(wire_power(ctx.get_block(input)))
}

/// `SignalGetter.getControlInputSignal`: a side control input.
pub(crate) fn control_input(
    ctx: &SignalCtx,
    pos: BlockPos,
    dir: Direction,
    diodes_only: bool,
) -> u8 {
    let state = ctx.get_block(pos);
    if diodes_only {
        if behaviour(state).is_diode() {
            ctx.get_direct_signal(pos, dir)
        } else {
            0
        }
    } else if behaviour(state).is_redstone_block() {
        15
    } else if behaviour(state).is_wire() {
        props::power(state)
    } else if behaviour(state).is_signal_source(state) {
        ctx.get_direct_signal(pos, dir)
    } else {
        0
    }
}

/// `DiodeBlock.getAlternateSignal`: the strongest side input, used both for
/// locking a repeater (`diodes_only`) and for a comparator's compare/subtract
/// (not diodes-only).
pub(crate) fn alternate_signal(
    ctx: &SignalCtx,
    pos: BlockPos,
    state: BlockStateId,
    diodes_only: bool,
) -> u8 {
    let facing = props::facing(state);
    let cw = facing.clockwise();
    let ccw = facing.counter_clockwise();
    control_input(ctx, pos.offset(cw), cw, diodes_only).max(control_input(
        ctx,
        pos.offset(ccw),
        ccw,
        diodes_only,
    ))
}

/// `DiodeBlock.shouldPrioritize`: the diode faces into another diode.
pub(crate) fn should_prioritize(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> bool {
    let opposite = props::facing(state).opposite();
    let neighbour = ctx.get_block(pos.offset(opposite));
    behaviour(neighbour).is_diode() && props::facing(neighbour) != opposite
}

/// `DiodeBlock.canSurviveOn(UP)`: `isFaceSturdy(UP, SupportType.RIGID)`.
pub(crate) fn supported(ctx: &Context, pos: BlockPos) -> bool {
    let below = ctx.get_block(pos.offset(Direction::Down));
    shapes::face_rigid(below, Direction::Up)
}

/// `DiodeBlock.updateNeighborsInFront`: notify the block behind the diode and
/// its neighbours, skipping the facing, with **the diode itself** as the source
/// block.
pub(crate) fn update_neighbors_in_front(ctx: &mut Context, pos: BlockPos, state: BlockStateId) {
    let facing = props::facing(state);
    let output = pos.offset(facing.opposite());
    ctx.neighbor_changed_at(output, pos, state);
    ctx.update_neighbors_at_except_from_facing(output, pos, state, facing, None);
}
