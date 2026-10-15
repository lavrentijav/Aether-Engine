//! The block of redstone: `PoweredBlock`.

use aether_world::BlockStateId;

use crate::behaviour::SignalCtx;
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;

/// The block of redstone. It emits weak power 15 in every direction but no
/// strong power, and vanilla makes it a **non**-conductor so it never smothers
/// the signal it gives.
pub struct RedstoneBlock;

impl BlockBehaviour for RedstoneBlock {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn is_redstone_block(&self) -> bool {
        true
    }

    fn is_conductor(&self, _state: BlockStateId) -> bool {
        // `Blocks.REDSTONE_BLOCK` is registered with `isRedstoneConductor(never)`.
        false
    }

    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        _state: BlockStateId,
        _pos: BlockPos,
        _dir: Direction,
    ) -> u8 {
        15
    }
}
