//! Tripwire and its hook: `TripWireBlock` and `TripWireHookBlock`.
//!
//! A line of tripwire between two hooks. When an entity enters a wire, the wire
//! becomes `powered`, the hooks at both ends become `powered` and `attached`,
//! and the hooks emit 15. The crate has no entity model, so the host reports
//! occupancy through [`crate::view::RedstoneView::entity_count`] and calls
//! [`crate::Redstone::on_entity_inside`].

use aether_world::BlockStateId;

use crate::behaviour::{Context, SignalCtx};
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::shapes;
use crate::view::{SetBlockFlags, TickPriority};

/// Tripwire (the wire part).
pub struct TripWire;
/// The tripwire hook.
pub struct TripWireHook;

const WIRE_DIST_MAX: i32 = 42;
const RECHECK_PERIOD: u32 = 10;

fn step(pos: BlockPos, dir: Direction, n: i32) -> BlockPos {
    let (dx, dy, dz) = dir.offset();
    BlockPos::new(pos.x + dx * n, pos.y + dy * n, pos.z + dz * n)
}

fn is_tripwire(state: BlockStateId) -> bool {
    props::name_of(state) == Some("minecraft:tripwire")
}

fn is_hook(state: BlockStateId) -> bool {
    props::name_of(state) == Some("minecraft:tripwire_hook")
}

fn wire_set(state: BlockStateId, key: &str, value: &str) -> BlockStateId {
    props::with(state, key, value).unwrap_or(state)
}

fn hook_signal(state: BlockStateId) -> u8 {
    if props::powered(state) {
        15
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Tripwire
// ---------------------------------------------------------------------------

impl BlockBehaviour for TripWire {
    fn on_place(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos, old: BlockStateId) {
        if crate::behaviour::block_id_of(old) != crate::behaviour::block_id_of(state) {
            update_source(ctx, pos, state);
        }
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        // Recompute the hooks as if this wire were still powered-but-gone.
        let powered = wire_set(state, "powered", "true");
        update_source(ctx, pos, powered);
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        if !dir.is_horizontal() {
            return None;
        }
        let connect = should_connect_to(ctx.get_block(pos.offset(dir)), dir);
        let value = if connect { "true" } else { "false" };
        let new = wire_set(state, props::side_prop(dir), value);
        (new != state).then_some(new)
    }

    fn entity_inside(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if !props::powered(state) {
            check_pressed(ctx, pos);
        }
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::powered(state) {
            check_pressed(ctx, pos);
        }
    }
}

/// `TripWireBlock.shouldConnectTo`.
fn should_connect_to(state: BlockStateId, dir: Direction) -> bool {
    if is_hook(state) {
        props::facing(state) == dir.opposite()
    } else {
        is_tripwire(state)
    }
}

/// `TripWireBlock.checkPressed`.
fn check_pressed(ctx: &mut Context, pos: BlockPos) {
    let state = ctx.get_block(pos);
    let was = props::powered(state);
    let now = ctx.entity_count(pos) > 0;
    if now != was {
        let value = if now { "true" } else { "false" };
        let new = wire_set(state, "powered", value);
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
        update_source(ctx, pos, new);
    }
    if now {
        ctx.schedule_tick(pos, RECHECK_PERIOD, TickPriority::Normal);
    }
}

/// `TripWireBlock.updateSource`: find a hook on the south or west leg and ask it
/// to recompute the whole line.
fn update_source(ctx: &mut Context, pos: BlockPos, _state: BlockStateId) {
    for dir in [Direction::South, Direction::West] {
        for i in 1..WIRE_DIST_MAX {
            let p = step(pos, dir, i);
            let s = ctx.get_block(p);
            if is_hook(s) {
                if props::facing(s) == dir.opposite() {
                    calculate_state(ctx, p, s, false);
                }
                break;
            }
            if !is_tripwire(s) {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tripwire hook
// ---------------------------------------------------------------------------

impl BlockBehaviour for TripWireHook {
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
        hook_signal(state)
    }

    fn get_direct_signal(
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

    fn set_placed_by(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        calculate_state(ctx, pos, state, false);
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        calculate_state(ctx, pos, state, false);
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        if props::bool_or(state, "attached", false) || props::powered(state) {
            calculate_state(ctx, pos, state, true);
        }
        if props::powered(state) {
            notify_neighbors(ctx, pos, props::facing(state));
        }
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        _state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        let state = ctx.get_block(pos);
        let facing = props::facing(state);
        if dir == facing.opposite() {
            let support = ctx.get_block(pos.offset(facing.opposite()));
            if !shapes::face_full(support, facing) {
                return Some(crate::behaviour::AIR);
            }
        }
        None
    }
}

/// `TripWireHookBlock.calculateState`, reduced: walk the line from this hook and
/// set `attached`/`powered` on both hooks (and `attached` on the wires between).
fn calculate_state(ctx: &mut Context, hook_pos: BlockPos, hook_state: BlockStateId, removed: bool) {
    let facing = props::facing(hook_state);
    let mut wires: Vec<(BlockPos, BlockStateId)> = Vec::new();
    let mut far: Option<(i32, BlockPos, BlockStateId)> = None;
    let mut line_intact = !removed;
    let mut any_powered = false;

    for k in 1..WIRE_DIST_MAX {
        let p = step(hook_pos, facing, k);
        let s = ctx.get_block(p);
        if is_hook(s) {
            if props::facing(s) == facing.opposite() {
                far = Some((k, p, s));
            }
            break;
        }
        if !is_tripwire(s) {
            line_intact = false;
            break;
        }
        wires.push((p, s));
        any_powered |= !props::bool_or(s, "disarmed", false) && props::powered(s);
    }

    let attached = line_intact && far.is_some();
    let powered = any_powered && attached;

    set_hook(ctx, hook_pos, facing, attached, powered);
    if let Some((k, p, s)) = far {
        let _ = k;
        set_hook(ctx, p, props::facing(s), attached, powered);
    }
    for (p, s) in wires {
        if props::bool_or(s, "attached", false) != attached {
            let value = if attached { "true" } else { "false" };
            let new = wire_set(s, "attached", value);
            ctx.set_block(p, new, SetBlockFlags::PLACE);
        }
    }
    if powered {
        notify_neighbors(ctx, hook_pos, facing);
        if let Some((_, p, s)) = far {
            let _ = s;
            notify_neighbors(ctx, p, facing.opposite());
        }
    }
}

fn set_hook(ctx: &mut Context, pos: BlockPos, facing: Direction, attached: bool, powered: bool) {
    let state = ctx.get_block(pos);
    let a = if attached { "true" } else { "false" };
    let p = if powered { "true" } else { "false" };
    let mut new = wire_set(state, "attached", a);
    new = wire_set(new, "powered", p);
    new = props::with(new, "facing", facing_name(facing)).unwrap_or(new);
    if new != state {
        ctx.set_block(pos, new, SetBlockFlags::PLACE);
    }
}

fn facing_name(dir: Direction) -> &'static str {
    match dir {
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        _ => "east",
    }
}

/// `TripWireHookBlock.notifyNeighbors`.
fn notify_neighbors(ctx: &mut Context, pos: BlockPos, facing: Direction) {
    let back = facing.opposite();
    ctx.update_neighbors_at(pos);
    ctx.update_neighbors_at(pos.offset(back));
}
