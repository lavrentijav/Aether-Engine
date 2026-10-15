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
///
/// `state` is the wire's own state, used as the **substitute** at the removed
/// cell so the scan walks past a just-removed wire to the far hook (vanilla
/// passes it as `blockState2` with `i` the wire's distance from the hook).
fn update_source(ctx: &mut Context, pos: BlockPos, state: BlockStateId) {
    for dir in [Direction::South, Direction::West] {
        for i in 1..WIRE_DIST_MAX {
            let p = step(pos, dir, i);
            let s = ctx.get_block(p);
            if is_hook(s) {
                if props::facing(s) == dir.opposite() {
                    calculate_state(ctx, p, s, false, true, Some((i, state)));
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
        // `setPlacedBy`: `calculateState(..., false, false, -1, null)`.
        calculate_state(ctx, pos, state, false, false, None);
    }

    fn tick(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        // `tick`: `calculateState(..., false, true, -1, null)`.
        calculate_state(ctx, pos, state, false, true, None);
    }

    fn on_remove(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        // `affectNeighborsAfterRemoval`: `calculateState(..., true, false, -1, null)`.
        if props::bool_or(state, "attached", false) || props::powered(state) {
            calculate_state(ctx, pos, state, true, false, None);
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

/// `TripWireHookBlock.calculateState`: walk the line from this hook and set
/// `attached`/`powered` on both hooks (and `attached` on the wires between).
///
/// `removed` is vanilla's `bl` (this hook is being removed), `notify` its `bl2`,
/// and `substitute` its `(i, blockState2)` — the removed wire, if any, treated as
/// still present at distance `i` so the scan reaches the far hook.
fn calculate_state(
    ctx: &mut Context,
    hook_pos: BlockPos,
    hook_state: BlockStateId,
    removed: bool,
    notify: bool,
    substitute: Option<(i32, BlockStateId)>,
) {
    let facing = props::facing(hook_state);
    let old_attached = props::bool_or(hook_state, "attached", false);

    let mut attached = !removed;
    let mut any_powered = false;
    let mut j = 0i32;
    let mut wires: [Option<BlockStateId>; 42] = [None; 42];

    let sub_i = substitute.map(|(i, _)| i).unwrap_or(-1);
    let sub_state = substitute.map(|(_, s)| s);

    for k in 1..WIRE_DIST_MAX {
        let p2 = step(hook_pos, facing, k);
        let raw = ctx.get_block(p2);
        if is_hook(raw) {
            if props::facing(raw) == facing.opposite() {
                j = k;
            }
            break;
        }
        if !is_tripwire(raw) && k != sub_i {
            wires[k as usize] = None;
            attached = false;
        } else {
            // Substitute the removed wire at `k == i`.
            let s3 = if k == sub_i {
                sub_state.unwrap_or(raw)
            } else {
                raw
            };
            let not_disarmed = !props::bool_or(s3, "disarmed", false);
            any_powered |= not_disarmed && props::powered(s3);
            wires[k as usize] = Some(s3);
            if k == sub_i {
                ctx.schedule_tick(hook_pos, RECHECK_PERIOD, TickPriority::Normal);
                attached &= not_disarmed;
            }
        }
    }

    // `bl5 &= j > 1`: a line needs at least one wire between the hooks, so two
    // adjacent hooks are not considered attached to each other.
    attached &= j > 1;
    let powered = any_powered && attached;

    if j > 0 {
        let p2 = step(hook_pos, facing, j);
        let facing2 = facing.opposite();
        set_hook(ctx, p2, facing2, attached, powered);
        notify_neighbors(ctx, p2, facing2);
    }
    if !removed {
        set_hook(ctx, hook_pos, facing, attached, powered);
        if notify {
            notify_neighbors(ctx, hook_pos, facing);
        }
    }

    if old_attached != attached {
        for l in 1..j {
            let p3 = step(hook_pos, facing, l);
            if let Some(s5) = wires[l as usize] {
                let s6 = ctx.get_block(p3);
                if is_tripwire(s6) || is_hook(s6) {
                    let value = if attached { "true" } else { "false" };
                    let new = wire_set(s5, "attached", value);
                    ctx.set_block(p3, new, SetBlockFlags::PLACE);
                }
            }
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
