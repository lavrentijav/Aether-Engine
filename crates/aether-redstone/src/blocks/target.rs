//! The target block: `TargetBlock`.
//!
//! A projectile that hits the target sets `power` to a value 1–15 that grows as
//! the hit lands nearer the centre of the face, and clears it after 20 ticks (an
//! arrow) or 8 (anything else). The crate has no projectiles, so the host
//! reports the hit through [`crate::Redstone::on_projectile_hit`].

use aether_world::BlockStateId;

use crate::behaviour::{Context, SignalCtx};
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// The target block.
pub struct Target;

impl BlockBehaviour for Target {
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
        props::power(state)
    }

    fn on_place(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos, old: BlockStateId) {
        // Vanilla clears a stale power on the block changing, guarded with
        // `hasScheduledTick` (any pending tick), not `willTickThisTick`.
        if crate::behaviour::block_id_of(old) != crate::behaviour::block_id_of(state)
            && props::power(state) > 0
            && !ctx.has_scheduled_tick(pos)
        {
            let new = props::with(state, "power", "0").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::RESET);
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) != 0 {
            let new = props::with(state, "power", "0").unwrap_or(state);
            ctx.set_block(pos, new, SetBlockFlags::PLACE);
        }
    }

    fn projectile_hit(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        face: Direction,
        position: [f32; 3],
        arrow: bool,
    ) {
        // `TargetBlock.updateRedstoneOutput`: a hit is ignored while a reset tick
        // is already pending, so a rapid second hit does not restart the timer.
        if ctx.has_scheduled_tick(pos) {
            return;
        }
        let strength = redstone_strength(face, position);
        let new = props::with(state, "power", &strength.to_string()).unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
        let delay = if arrow { 20 } else { 8 };
        ctx.schedule_tick(pos, delay, TickPriority::Normal);
    }
}

/// `TargetBlock.getRedstoneStrength`.
fn redstone_strength(face: Direction, position: [f32; 3]) -> u8 {
    let d = (frac(position[0]) - 0.5).abs();
    let e = (frac(position[1]) - 0.5).abs();
    let f = (frac(position[2]) - 0.5).abs();
    let g = match face {
        Direction::Up | Direction::Down => d.max(f),
        Direction::North | Direction::South => d.max(e),
        Direction::West | Direction::East => e.max(f),
    };
    let scaled = (15.0 * ((0.5 - g) / 0.5).clamp(0.0, 1.0)).ceil() as i32;
    scaled.max(1).clamp(1, 15) as u8
}

fn frac(x: f32) -> f32 {
    x - x.floor()
}
