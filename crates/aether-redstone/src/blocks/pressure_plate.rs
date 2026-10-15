//! Pressure plates: `BasePressurePlateBlock`, with the wood/stone
//! (`PressurePlateBlock`) and weighted (`WeightedPressurePlateBlock`) forms.
//!
//! A plate is pressed while entities overlap its cell. The crate has no entity
//! model, so the host reports the count through
//! [`crate::view::RedstoneView::entity_count`] and calls
//! [`crate::Redstone::on_entity_inside`] when an entity steps on or off. A
//! wood/stone plate is 15 when pressed; a weighted plate scales with the count.

use aether_world::BlockStateId;

use crate::behaviour::{Context, SignalCtx};
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::shapes;
use crate::view::{SetBlockFlags, TickPriority};

/// A wood or stone pressure plate (15 when pressed).
pub struct PressurePlate;
/// The light weighted pressure plate (max weight 15).
pub struct LightWeighted;
/// The heavy weighted pressure plate (max weight 150).
pub struct HeavyWeighted;

/// The pressed state's hold time before a re-check: 20 ticks for a plain plate,
/// 10 for a weighted one.
const PLATE_PRESSED_TIME: u32 = 20;
const WEIGHTED_PRESSED_TIME: u32 = 10;

fn can_survive(ctx: &Context, pos: BlockPos) -> bool {
    let below = ctx.get_block(pos.offset(Direction::Down));
    // `canSupportRigidBlock(...) || canSupportCenter(..., UP)`.
    shapes::face_rigid(below, Direction::Up) || shapes::face_center(below, Direction::Up)
}

fn update_neighbours(ctx: &mut Context, pos: BlockPos) {
    ctx.update_neighbors_at(pos);
    ctx.update_neighbors_at(pos.offset(Direction::Down));
}

/// `BasePressurePlateBlock.checkPressed`, specialised by the caller's signal
/// accessors.
fn check_pressed(
    ctx: &mut Context,
    pos: BlockPos,
    state: BlockStateId,
    signal_for_state: impl Fn(BlockStateId) -> u8,
    signal_strength: impl Fn(&Context, BlockPos) -> u8,
    set_signal: impl Fn(BlockStateId, u8) -> BlockStateId,
    pressed_time: u32,
) {
    let old = signal_for_state(state);
    let new = signal_strength(ctx, pos);
    if old != new {
        let new_state = set_signal(state, new);
        ctx.set_block(pos, new_state, SetBlockFlags::CLIENTS);
        update_neighbours(ctx, pos);
    }
    if new > 0 {
        ctx.schedule_tick(pos, pressed_time, TickPriority::Normal);
    }
}

fn shared_shape(
    ctx: &mut Context,
    _state: BlockStateId,
    pos: BlockPos,
    dir: Direction,
) -> Option<BlockStateId> {
    if dir == Direction::Down && !can_survive(ctx, pos) {
        return Some(crate::behaviour::AIR);
    }
    None
}

impl BlockBehaviour for PressurePlate {
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
        ctx: &SignalCtx,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if dir == Direction::Up {
            self.get_signal(ctx, state, pos, dir)
        } else {
            0
        }
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::powered(state) {
            update_neighbours(ctx, pos);
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        shared_shape(ctx, state, pos, dir)
    }

    fn entity_inside(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if !props::powered(state) {
            check(ctx, pos, state, PLATE_PRESSED_TIME, |s| {
                if props::powered(s) {
                    15
                } else {
                    0
                }
            });
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::powered(state) {
            check(ctx, pos, state, PLATE_PRESSED_TIME, |s| {
                if props::powered(s) {
                    15
                } else {
                    0
                }
            });
        }
    }
}

impl BlockBehaviour for LightWeighted {
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

    fn get_direct_signal(
        &self,
        ctx: &SignalCtx,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if dir == Direction::Up {
            self.get_signal(ctx, state, pos, dir)
        } else {
            0
        }
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) > 0 {
            update_neighbours(ctx, pos);
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        shared_shape(ctx, state, pos, dir)
    }

    fn entity_inside(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) == 0 {
            check_weighted(ctx, pos, state, 15);
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) > 0 {
            check_weighted(ctx, pos, state, 15);
        }
    }
}

impl BlockBehaviour for HeavyWeighted {
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

    fn get_direct_signal(
        &self,
        ctx: &SignalCtx,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if dir == Direction::Up {
            self.get_signal(ctx, state, pos, dir)
        } else {
            0
        }
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) > 0 {
            update_neighbours(ctx, pos);
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        shared_shape(ctx, state, pos, dir)
    }

    fn entity_inside(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) == 0 {
            check_weighted(ctx, pos, state, 150);
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::power(state) > 0 {
            check_weighted(ctx, pos, state, 150);
        }
    }
}

/// A plain plate's recompute.
fn check(
    ctx: &mut Context,
    pos: BlockPos,
    state: BlockStateId,
    pressed_time: u32,
    signal_for_state: impl Fn(BlockStateId) -> u8,
) {
    check_pressed(
        ctx,
        pos,
        state,
        signal_for_state,
        |ctx, pos| if ctx.entity_count(pos) > 0 { 15 } else { 0 },
        |s, v| {
            let powered = if v > 0 { "true" } else { "false" };
            props::with(s, "powered", powered).unwrap_or(s)
        },
        pressed_time,
    );
}

/// A weighted plate's recompute: `ceil(min(count, max) / max * 15)`.
fn check_weighted(ctx: &mut Context, pos: BlockPos, state: BlockStateId, max_weight: usize) {
    check_pressed(
        ctx,
        pos,
        state,
        props::power,
        |ctx, pos| {
            let count = ctx.entity_count(pos).min(max_weight);
            if count == 0 {
                0
            } else {
                ((count as f32 / max_weight as f32) * 15.0).ceil() as u8
            }
        },
        |s, v| props::with(s, "power", &v.to_string()).unwrap_or(s),
        WEIGHTED_PRESSED_TIME,
    );
}
