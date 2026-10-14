//! The redstone torch and its wall-mounted form: `RedstoneTorchBlock` and
//! `RedstoneWallTorchBlock`.

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// A redstone torch standing on top of a block.
pub struct Torch;
/// A redstone torch attached to the side of a block.
pub struct WallTorch;

fn is_wall(state: BlockStateId) -> bool {
    // A wall torch is the one with a horizontal `facing`; a standing torch has
    // no `facing` property at all.
    props::has(state, "facing")
}

fn has_neighbor_signal(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> bool {
    if is_wall(state) {
        let dir = props::facing(state).opposite();
        ctx.get_signal(pos.offset(dir), dir) > 0
    } else {
        ctx.get_signal(pos.offset(Direction::Down), Direction::Down) > 0
    }
}

fn signal(_wall: bool, state: BlockStateId, dir: Direction) -> u8 {
    if !props::lit(state) {
        return 0;
    }
    if is_wall(state) {
        if props::facing(state) != dir {
            15
        } else {
            0
        }
    } else if dir != Direction::Up {
        15
    } else {
        0
    }
}

fn direct_signal(wall: bool, state: BlockStateId, dir: Direction) -> u8 {
    if dir == Direction::Down {
        signal(wall, state, dir)
    } else {
        0
    }
}

fn notify_neighbors(ctx: &mut Context, pos: BlockPos) {
    for dir in Direction::ALL {
        ctx.update_neighbors_at(pos.offset(dir));
    }
}

fn neighbor_changed(ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
    if props::lit(state) == has_neighbor_signal(&ctx.signals(), pos, state)
        && !ctx.will_tick_this_tick(pos)
    {
        ctx.schedule_tick(pos, 2, TickPriority::Normal);
    }
}

fn tick(ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
    let signal = has_neighbor_signal(&ctx.signals(), pos, state);
    if props::lit(state) {
        if signal {
            let new = props::with(state, "lit", "false").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::PLACE);
            let now = ctx.game_time();
            ctx.push_toggle(pos, now);
            if toggle_count(ctx, pos, now) >= 8 {
                ctx.schedule_tick(pos, 160, TickPriority::Normal);
            }
        }
    } else if !signal {
        let now = ctx.game_time();
        if toggle_count(ctx, pos, now) < 8 {
            let new = props::with(state, "lit", "true").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::PLACE);
        }
    }
}

fn toggle_count(ctx: &Context, pos: BlockPos, now: u64) -> u32 {
    ctx.toggles_since(pos, now.saturating_sub(60))
}

impl BlockBehaviour for Torch {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        signal(false, state, dir)
    }

    fn get_direct_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        direct_signal(false, state, dir)
    }

    fn neighbor_changed(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        neighbor_changed(ctx, state, pos);
    }

    fn on_place(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos, _old: BlockStateId) {
        notify_neighbors(ctx, pos);
    }

    fn on_remove(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos) {
        notify_neighbors(ctx, pos);
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        tick(ctx, state, pos);
    }
}

impl BlockBehaviour for WallTorch {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        signal(true, state, dir)
    }

    fn get_direct_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        direct_signal(true, state, dir)
    }

    fn neighbor_changed(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        neighbor_changed(ctx, state, pos);
    }

    fn on_place(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos, _old: BlockStateId) {
        notify_neighbors(ctx, pos);
    }

    fn on_remove(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos) {
        notify_neighbors(ctx, pos);
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        tick(ctx, state, pos);
    }
}
