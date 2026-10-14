//! Buttons: `ButtonBlock`. Stone is held 20 ticks, wood 30.

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::blocks::{update_powered_source_neighbours, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// A stone-family button (20 ticks).
pub struct StoneButton;
/// A wood-family button (30 ticks).
pub struct WoodButton;

const STONE_TICKS: u32 = 20;
const WOOD_TICKS: u32 = 30;

fn get_signal(state: BlockStateId) -> u8 {
    if props::powered(state) {
        15
    } else {
        0
    }
}

fn get_direct_signal(state: BlockStateId, dir: Direction) -> u8 {
    if props::powered(state) && props::connected_direction(state) == dir {
        15
    } else {
        0
    }
}

fn use_block(ctx: &mut Context, state: BlockStateId, pos: BlockPos, ticks: u32) {
    if props::powered(state) {
        return;
    }
    let new = props::with(state, "powered", "true").unwrap_or(state);
    ctx.set_block(pos, new, SetBlockFlags::PLACE);
    update_powered_source_neighbours(ctx, pos, new);
    ctx.schedule_tick(pos, ticks, TickPriority::Normal);
}

fn tick(ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
    if props::powered(state) {
        let new = props::with(state, "powered", "false").unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
        update_powered_source_neighbours(ctx, pos, new);
    }
}

impl BlockBehaviour for StoneButton {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        _dir: Direction,
    ) -> u8 {
        get_signal(state)
    }

    fn get_direct_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        get_direct_signal(state, dir)
    }

    fn neighbor_changed(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::powered(state) {
            update_powered_source_neighbours(ctx, pos, state);
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        crate::blocks::face_attached_support(ctx, state, pos, dir)
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        tick(ctx, state, pos);
    }

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        use_block(ctx, state, pos, STONE_TICKS);
    }
}

impl BlockBehaviour for WoodButton {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        _dir: Direction,
    ) -> u8 {
        get_signal(state)
    }

    fn get_direct_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        get_direct_signal(state, dir)
    }

    fn neighbor_changed(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::powered(state) {
            update_powered_source_neighbours(ctx, pos, state);
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        crate::blocks::face_attached_support(ctx, state, pos, dir)
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        tick(ctx, state, pos);
    }

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        use_block(ctx, state, pos, WOOD_TICKS);
    }
}
