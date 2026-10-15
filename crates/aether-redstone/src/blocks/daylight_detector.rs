//! The daylight detector: `DaylightDetectorBlock`.
//!
//! Recomputes its `power` every 20 ticks from the world's sky light and sun
//! angle, and can be inverted by interacting. The raw environment is a host
//! concern ([`crate::view::RedstoneView::sky_light`] / `sun_angle`).

use std::f32::consts::PI;

use aether_world::BlockStateId;

use crate::behaviour::Context;
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{SetBlockFlags, TickPriority};

/// The daylight detector.
pub struct DaylightDetector;

impl BlockBehaviour for DaylightDetector {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_signal(
        &self,
        _ctx: &crate::behaviour::SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        _dir: Direction,
    ) -> u8 {
        props::power(state)
    }

    fn on_place(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos, _old: BlockStateId) {
        update_signal(ctx, pos);
        ctx.schedule_tick(pos, 20, TickPriority::Normal);
    }

    fn tick(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos) {
        update_signal(ctx, pos);
        ctx.schedule_tick(pos, 20, TickPriority::Normal);
    }

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        let inverted = if props::bool_or(state, "inverted", false) {
            "false"
        } else {
            "true"
        };
        let new = props::with(state, "inverted", inverted).unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::CLIENTS);
        update_signal(ctx, pos);
    }
}

/// `DaylightDetectorBlock.updateSignalStrength`.
fn update_signal(ctx: &mut Context, pos: BlockPos) {
    let state = ctx.get_block(pos);
    let mut signal = ctx.sky_light(pos);
    let inverted = props::bool_or(state, "inverted", false);
    if inverted {
        signal = 15 - signal;
    } else if signal > 0 {
        let mut angle = ctx.sun_angle(pos);
        let target = if angle < PI { 0.0 } else { PI * 2.0 };
        angle += (target - angle) * 0.2;
        signal = (signal as f32 * angle.cos()).round() as i32;
    }
    let signal = signal.clamp(0, 15) as u8;
    if props::power(state) != signal {
        let new = props::with(state, "power", &signal.to_string()).unwrap_or(state);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
    }
}
