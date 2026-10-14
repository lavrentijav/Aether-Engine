//! Redstone wire: `RedStoneWireBlock`.
//!
//! Power 0–15 decaying one per block, the connection shape, the up-and-over a
//! conductor and the cut when a conductor sits above. The power evaluator is
//! the **default** one (`DefaultRedstoneWireEvaluator`) — 1.21.11's
//! `REDSTONE_EXPERIMENTS` evaluator is off — and this is the algorithm the
//! later `Redstone` update order documents.

use aether_world::BlockStateId;

use crate::behaviour::{behaviour, Context, NeighborSource, SignalCtx};
use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::shapes;
use crate::view::SetBlockFlags;

/// Redstone wire.
pub struct Wire;

impl BlockBehaviour for Wire {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn is_wire(&self) -> bool {
        true
    }

    fn get_signal(
        &self,
        ctx: &SignalCtx,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        if ctx.suppress_wire {
            return 0;
        }
        let power = props::power(state);
        if power == 0 || dir == Direction::Down {
            0
        } else if dir == Direction::Up || props::side_connected(state, dir.opposite()) {
            power
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
        // `RedStoneWireBlock.getDirectSignal` is `getSignal` while signalling.
        self.get_signal(ctx, state, pos, dir)
    }

    fn neighbor_changed(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        _from: Option<NeighborSource>,
    ) {
        update(ctx, pos, state);
    }

    fn on_place(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos, old: BlockStateId) {
        // Vanilla's `onPlace` only acts when the block changed (`!old.is(this)`),
        // which is what stops a wire's own power update from recursing.
        if crate::behaviour::block_id_of(old) == crate::behaviour::block_id_of(state) {
            return;
        }
        placement_update(ctx, pos, state);
        ctx.update_neighbors_of_neighboring_wires(pos);
    }

    fn on_remove(&self, ctx: &mut Context, _state: BlockStateId, pos: BlockPos) {
        for dir in Direction::ALL {
            ctx.update_neighbors_at(pos.offset(dir));
        }
        ctx.update_neighbors_of_neighboring_wires(pos);
    }

    fn update_shape(
        &self,
        ctx: &mut Context,
        state: BlockStateId,
        pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        // A wire's support is not re-checked for destruction this stage (see
        // KNOWN_ISSUES): only its connection shape follows a neighbour change.
        let _ = dir;
        let shaped = {
            let signals = ctx.signals();
            connection_state(&signals, pos, state)
        };
        (shaped != state).then_some(shaped)
    }

    fn use_block(&self, ctx: &mut Context, state: BlockStateId, pos: BlockPos) {
        let mut toggled = state;
        if props::is_cross(state) {
            for dir in Direction::HORIZONTAL {
                toggled = props::set_side(toggled, dir, "none");
            }
        } else if props::is_dot(state) {
            for dir in Direction::HORIZONTAL {
                toggled = props::set_side(toggled, dir, "side");
            }
        } else {
            return;
        }
        let shaped = {
            let signals = ctx.signals();
            connection_state(&signals, pos, toggled)
        };
        if shaped != state {
            ctx.set_block(pos, shaped, SetBlockFlags::PLACE);
            ctx.notify_around(pos);
        }
    }
}

/// Recompute a wire's shape and power, writing it if it changed.
pub(crate) fn update(ctx: &mut Context, pos: BlockPos, state: BlockStateId) {
    update_base(ctx, pos, state, state);
}

/// Placement uses vanilla's `crossState` as the connection base, so an isolated
/// dust is a **cross** (all four sides `side`), not a dot. Every later update
/// starts from the wire's own state, which is what preserves a player-set dot.
fn placement_update(ctx: &mut Context, pos: BlockPos, state: BlockStateId) {
    let mut base = state;
    for dir in Direction::HORIZONTAL {
        base = props::set_side(base, dir, "side");
    }
    update_base(ctx, pos, state, base);
}

fn update_base(ctx: &mut Context, pos: BlockPos, state: BlockStateId, base: BlockStateId) {
    let shaped = {
        let signals = ctx.signals();
        connection_state(&signals, pos, base)
    };
    let target = {
        let signals = ctx.signals();
        target_strength(&signals, pos)
    };
    let new = props::with(shaped, "power", &target.to_string()).unwrap_or(shaped);
    if new != state {
        ctx.set_block(pos, new, SetBlockFlags::CLIENTS);
        ctx.notify_around(pos);
    }
}

/// `RedStoneWireBlock.canSurviveOn`: a sturdy top face, or a hopper.
fn can_survive_on(state: BlockStateId) -> bool {
    shapes::face_full(state, Direction::Up) || shapes::is_hopper(state)
}

/// `RedStoneWireBlock.shouldConnectTo`.
fn should_connect_to(state: BlockStateId, dir: Option<Direction>) -> bool {
    if behaviour(state).is_wire() {
        return true;
    }
    if behaviour(state).is_repeater() {
        let facing = props::facing(state);
        return dir == Some(facing) || Some(facing.opposite()) == dir;
    }
    behaviour(state).is_signal_source(state) && dir.is_some()
}

fn connecting_side(
    ctx: &SignalCtx,
    pos: BlockPos,
    dir: Direction,
    above_not_conductor: bool,
) -> &'static str {
    let neighbour = pos.offset(dir);
    let state = ctx.get_block(neighbour);
    if above_not_conductor {
        let sturdy = is_trapdoor(state) || can_survive_on(state);
        if sturdy && should_connect_to(ctx.get_block(neighbour.offset(Direction::Up)), None) {
            if shapes::face_full(state, dir.opposite()) {
                return "up";
            }
            return "side";
        }
    }
    let connects = should_connect_to(state, Some(dir));
    if !connects
        && (ctx.is_conductor(neighbour)
            || !should_connect_to(ctx.get_block(neighbour.offset(Direction::Down)), None))
    {
        "none"
    } else {
        "side"
    }
}

fn missing_connections(ctx: &SignalCtx, pos: BlockPos, state: BlockStateId) -> BlockStateId {
    let above_not_conductor = !ctx.is_conductor(pos.offset(Direction::Up));
    let mut state = state;
    for dir in Direction::HORIZONTAL {
        if !props::side_connected(state, dir) {
            let side = connecting_side(ctx, pos, dir, above_not_conductor);
            state = props::set_side(state, dir, side);
        }
    }
    state
}

/// `RedStoneWireBlock.getConnectionState`, reduced to the shape.
pub(crate) fn connection_state(
    ctx: &SignalCtx,
    pos: BlockPos,
    state: BlockStateId,
) -> BlockStateId {
    let was_dot = props::is_dot(state);
    let mut state = state;
    for dir in Direction::HORIZONTAL {
        state = props::set_side(state, dir, "none");
    }
    state = missing_connections(ctx, pos, state);
    if was_dot && props::is_dot(state) {
        return state;
    }
    let north = props::side_connected(state, Direction::North);
    let south = props::side_connected(state, Direction::South);
    let east = props::side_connected(state, Direction::East);
    let west = props::side_connected(state, Direction::West);
    let ns_none = !north && !south;
    let ew_none = !east && !west;
    if !west && ns_none {
        state = props::set_side(state, Direction::West, "side");
    }
    if !east && ns_none {
        state = props::set_side(state, Direction::East, "side");
    }
    if !north && ew_none {
        state = props::set_side(state, Direction::North, "side");
    }
    if !south && ew_none {
        state = props::set_side(state, Direction::South, "side");
    }
    state
}

/// `RedstoneWireEvaluator.getIncomingWireSignal`.
fn incoming_wire_signal(ctx: &SignalCtx, pos: BlockPos) -> u8 {
    let mut best = 0;
    let above = pos.offset(Direction::Up);
    for dir in Direction::HORIZONTAL {
        let neighbour = pos.offset(dir);
        let state = ctx.get_block(neighbour);
        best = best.max(wire_power(state));
        if ctx.is_conductor(neighbour) && !ctx.is_conductor(above) {
            best = best.max(wire_power(ctx.get_block(neighbour.offset(Direction::Up))));
        } else if !ctx.is_conductor(neighbour) {
            best = best.max(wire_power(ctx.get_block(neighbour.offset(Direction::Down))));
        }
    }
    best.saturating_sub(1)
}

fn wire_power(state: BlockStateId) -> u8 {
    if behaviour(state).is_wire() {
        props::power(state)
    } else {
        0
    }
}

fn target_strength(ctx: &SignalCtx, pos: BlockPos) -> u8 {
    // A wire ignores every other wire while computing its own input, so a block
    // powered by wire cannot re-power wire beside it.
    let block_signal = ctx.suppressed().get_best_neighbor_signal(pos);
    if block_signal == 15 {
        return 15;
    }
    block_signal.max(incoming_wire_signal(ctx, pos))
}

fn is_trapdoor(state: BlockStateId) -> bool {
    props::name_of(state).is_some_and(|n| n.ends_with("_trapdoor"))
}
