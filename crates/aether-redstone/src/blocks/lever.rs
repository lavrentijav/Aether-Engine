//! The lever: `LeverBlock`.

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::blocks::{update_powered_source_neighbours, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::SetBlockFlags;

/// A lever.
pub struct Lever;

impl BlockBehaviour for Lever {
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
        if props::powered(state) {
            15
        } else {
            0
        }
    }

    fn get_direct_signal(
        &self,
        _ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if props::powered(state) && props::connected_direction(state) == dir {
            15
        } else {
            0
        }
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

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        let value = if props::powered(state) {
            "false"
        } else {
            "true"
        };
        let new = props::with(state, "powered", value).unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
        update_powered_source_neighbours(ctx, pos, new);
    }
}
