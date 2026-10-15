//! The redstone lamp: `RedstoneLampBlock`.

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource};
use crate::blocks::BlockBehaviour;
use crate::pos::BlockPos;
use crate::props;
use crate::view::TickPriority;

/// The redstone lamp.
pub struct Lamp;

impl BlockBehaviour for Lamp {
    fn neighbor_changed(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        let is_lit = props::lit(state);
        let powered = ctx.signals().has_neighbor_signal(pos);
        if is_lit != powered {
            if is_lit {
                ctx.schedule_tick(pos, 4, TickPriority::Normal);
            } else {
                let new = props::with(state, "lit", "true").unwrap_or(state);
                ctx.apply(pos, new);
            }
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::lit(state) && !ctx.signals().has_neighbor_signal(pos) {
            let new = props::with(state, "lit", "false").unwrap_or(state);
            ctx.apply(pos, new);
        }
    }
}
