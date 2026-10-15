//! The redstone repeater: `RepeaterBlock` on top of `DiodeBlock`.

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::blocks::{diode, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// The redstone repeater.
pub struct Repeater;

impl BlockBehaviour for Repeater {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn is_diode(&self) -> bool {
        true
    }

    fn connects_to_wire(&self, state: BlockStateId, dir: Option<Direction>) -> bool {
        // `RedStoneWireBlock.shouldConnectTo` special-cases the repeater: it
        // connects only along its facing axis.
        match dir {
            Some(d) => {
                let f = props::facing(state);
                d == f || d == f.opposite()
            }
            None => false,
        }
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if props::powered(state) && props::facing(state) == dir {
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
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        let state = update_locked(ctx, pos, state);
        check_tick(ctx, pos, state);
    }

    fn on_place(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos, _old: BlockStateId) {
        diode::update_neighbors_in_front(ctx, pos, state);
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        diode::update_neighbors_in_front(ctx, pos, state);
    }

    fn set_placed_by(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        // `DiodeBlock.setPlacedBy`: a diode placed onto a powered input turns on
        // after one tick, not after its delay.
        if should_turn_on(&ctx.signals(), pos, state) {
            ctx.schedule_tick(pos, 1, TickPriority::Normal);
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if is_locked(&ctx.signals(), pos, state) {
            return;
        }
        let (is_on, should) = {
            let signals = ctx.signals();
            (props::powered(state), should_turn_on(&signals, pos, state))
        };
        if is_on && !should {
            let new = props::with(state, "powered", "false").unwrap_or(state);
            ctx.apply(pos, new);
        } else if !is_on {
            let new = props::with(state, "powered", "true").unwrap_or(state);
            ctx.apply(pos, new);
            if !should {
                ctx.schedule_tick(pos, delay(state), TickPriority::VeryHigh);
            }
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        // `RepeaterBlock.updateShape(DOWN)`: a repeater whose support is gone
        // pops off (`canSurviveOn` = RIGID).
        if dir == Direction::Down && !diode::supported(ctx, pos) {
            return Some(crate::behaviour::AIR);
        }
        // Then it recomputes LOCKED for any direction off the facing axis,
        // which includes UP and DOWN.
        let f = props::facing(state);
        if dir != f && dir != f.opposite() {
            let locked = is_locked(&ctx.signals(), pos, state);
            if locked != props::locked(state) {
                let value = if locked { "true" } else { "false" };
                return props::with(state, "locked", value);
            }
        }
        None
    }

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        let next = if props::delay(state) >= 4 {
            1
        } else {
            props::delay(state) + 1
        };
        let new = props::with(state, "delay", &next.to_string()).unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
    }
}

fn delay(state: BlockStateId) -> u32 {
    u32::from(props::delay(state)) * 2
}

fn should_turn_on(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> bool {
    diode::base_input_signal(ctx, pos, state) > 0
}

fn is_locked(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> bool {
    diode::alternate_signal(ctx, pos, state, true) > 0
}

fn update_locked(ctx: &mut Context, pos: BlockPos, state: BlockStateId) -> BlockStateId {
    let locked = is_locked(&ctx.signals(), pos, state);
    if locked == props::locked(state) {
        return state;
    }
    let value = if locked { "true" } else { "false" };
    let new = props::with(state, "locked", value).unwrap_or(state);
    ctx.set_block(pos, new, SetBlockFlags::CLIENTS);
    ctx.update_neighbors_at(pos);
    new
}

fn check_tick(ctx: &mut Context, pos: BlockPos, state: BlockStateId) {
    if is_locked(&ctx.signals(), pos, state) {
        return;
    }
    let (is_on, should, prioritize) = {
        let signals = ctx.signals();
        (
            props::powered(state),
            should_turn_on(&signals, pos, state),
            diode::should_prioritize(&signals, pos, state),
        )
    };
    if is_on != should && !ctx.will_tick_this_tick(pos) {
        let priority = if prioritize {
            TickPriority::ExtremelyHigh
        } else if is_on {
            TickPriority::VeryHigh
        } else {
            TickPriority::High
        };
        ctx.schedule_tick(pos, delay(state), priority);
    }
}
