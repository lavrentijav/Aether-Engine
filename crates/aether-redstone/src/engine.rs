//! The redstone engine and the vanilla behaviour it models.
//!
//! One [`Redstone`] value drives any [`RedstoneView`]. It owns no world state,
//! so every method is a pure function of the view it is handed: call it twice
//! on the same view and you get the same result. The transient update queue a
//! call needs lives on the stack for the duration of that call.
//!
//! This module holds the **power model** — weak and strong signal, conductor
//! pass-through — the update plumbing, **redstone wire**, **repeaters** and
//! **lamps**, and the simple sources: levers, buttons and redstone torches.
//!
//! # Update order
//!
//! This is the **1.21.11 default** — the "Eigencraft" evaluator turned off.
//! Vanilla collects neighbour updates in a `CollectingNeighborUpdater` and runs
//! them depth-first in `UPDATE_ORDER`, with the subtlety that a multi-neighbour
//! update yields after each neighbour so freshly-generated updates interleave.
//! This engine reproduces the *set* of updates and the `UPDATE_ORDER` position
//! order, but drains the queue **breadth-first**. That is deterministic — the
//! same circuit always produces the same trace — and reaches the same steady
//! state, because every rule is a monotone fixed point over power (a wire
//! recomputes its target from its neighbours until they agree). Where a single
//! tick's *intermediate* state is observable (two diodes scheduled at the same
//! priority racing in one update), the winner may differ from vanilla. The
//! long-term plan is the compiled dependency graph in `docs/ROADMAP.md`; this is
//! the simple vanilla-style model it will replace.

use std::collections::VecDeque;

use aether_world::BlockStateId;

use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;
use crate::view::{RedstoneView, TickPriority};

/// A hard cap on the update cascade, so a malformed circuit cannot spin a call
/// forever. A real cascade is a few hundred updates; this is far above that.
const MAX_UPDATES: usize = 1_000_000;

/// A scheduled re-evaluation of one cell.
#[derive(Debug, Clone, Copy)]
struct Update(BlockPos);

/// The kind of redstone-relevant block a state is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Air,
    Wire,
    Repeater,
    Torch,
    WallTorch,
    Lever,
    Button,
    RedstoneBlock,
    Lamp,
    Comparator,
    Other,
}

fn kind_of(state: BlockStateId) -> Kind {
    let Some(name) = props::name_of(state) else {
        return Kind::Air;
    };
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    match name {
        "air" | "cave_air" | "void_air" => Kind::Air,
        "redstone_wire" => Kind::Wire,
        "repeater" => Kind::Repeater,
        "comparator" => Kind::Comparator,
        "redstone_torch" => Kind::Torch,
        "redstone_wall_torch" => Kind::WallTorch,
        "lever" => Kind::Lever,
        "redstone_block" => Kind::RedstoneBlock,
        "redstone_lamp" => Kind::Lamp,
        _ if name.ends_with("_button") => Kind::Button,
        _ => Kind::Other,
    }
}

fn is_signal_source(state: BlockStateId) -> bool {
    matches!(
        kind_of(state),
        Kind::Wire
            | Kind::Repeater
            | Kind::Comparator
            | Kind::Torch
            | Kind::WallTorch
            | Kind::Lever
            | Kind::Button
            | Kind::RedstoneBlock
    )
}

fn set_side(state: BlockStateId, dir: Direction, value: &str) -> BlockStateId {
    props::with(state, side_prop(dir), value).unwrap_or(state)
}

fn is_dot(state: BlockStateId) -> bool {
    !side_connected(state, Direction::North)
        && !side_connected(state, Direction::South)
        && !side_connected(state, Direction::East)
        && !side_connected(state, Direction::West)
}

fn is_cross(state: BlockStateId) -> bool {
    side_connected(state, Direction::North)
        && side_connected(state, Direction::South)
        && side_connected(state, Direction::East)
        && side_connected(state, Direction::West)
}

fn wire_power(state: BlockStateId) -> u8 {
    if kind_of(state) == Kind::Wire {
        power(state)
    } else {
        0
    }
}

fn is_trapdoor(state: BlockStateId) -> bool {
    props::name_of(state).is_some_and(|n| n.ends_with("_trapdoor"))
}

/// `true` for the diode family. Comparators are recognised so locking works,
/// but their comparison behaviour is a later stage.
fn is_diode(state: BlockStateId) -> bool {
    matches!(kind_of(state), Kind::Repeater | Kind::Comparator)
}

fn delay(state: BlockStateId) -> u8 {
    props::int_or(state, "delay", 1)
}

fn power(state: BlockStateId) -> u8 {
    props::int_or(state, "power", 0)
}

fn lit(state: BlockStateId) -> bool {
    props::bool_or(state, "lit", false)
}

fn powered(state: BlockStateId) -> bool {
    props::bool_or(state, "powered", false)
}

fn facing(state: BlockStateId) -> Direction {
    match props::get(state, "facing") {
        Some("north") => Direction::North,
        Some("south") => Direction::South,
        Some("west") => Direction::West,
        Some("east") => Direction::East,
        _ => Direction::North,
    }
}

/// The attachment direction for a lever or button (`getConnectedDirection`).
fn connected_direction(state: BlockStateId) -> Direction {
    match props::get(state, "face") {
        Some("floor") => Direction::Up,
        Some("ceiling") => Direction::Down,
        _ => facing(state),
    }
}

fn side_prop(dir: Direction) -> &'static str {
    match dir {
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
        _ => "north",
    }
}

fn side_connected(state: BlockStateId, dir: Direction) -> bool {
    props::get(state, side_prop(dir)).is_some_and(|v| v != "none")
}

/// The redstone engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct Redstone;

impl Redstone {
    /// A new engine. It holds nothing; this is a constructor for symmetry.
    pub const fn new() -> Self {
        Redstone
    }

    // -----------------------------------------------------------------------
    // Weak and strong power
    // -----------------------------------------------------------------------

    /// The **weak** redstone signal emitted into the neighbour in `dir`.
    ///
    /// `dir` points from the receiver toward the block being read. A conductor
    /// additionally contributes the strong power it holds, which is how a
    /// lever attached to a block powers wire on the far side of it.
    pub fn get_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos, dir: Direction) -> u8 {
        self.signal(view, pos, dir, false)
    }

    /// The **strong** signal this block provides to the neighbour in `dir`.
    pub fn get_direct_signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        dir: Direction,
    ) -> u8 {
        self.direct_signal(view, pos, dir, false)
    }

    /// The largest weak signal among a block's six neighbours
    /// (`getBestNeighborSignal`).
    pub fn get_best_neighbor_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> u8 {
        self.best_neighbor(view, pos, false)
    }

    /// Whether any of a block's six neighbours emits a signal into it
    /// (`hasNeighborSignal`).
    pub fn has_neighbor_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> bool {
        self.best_neighbor(view, pos, false) > 0
    }

    fn signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        dir: Direction,
        suppress_wire: bool,
    ) -> u8 {
        let state = view.get_block(pos);
        let own = self.block_signal(state, dir, suppress_wire);
        if view.is_conductor(pos) {
            own.max(self.direct_signal_to(view, pos, suppress_wire))
        } else {
            own
        }
    }

    fn direct_signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        dir: Direction,
        suppress_wire: bool,
    ) -> u8 {
        let state = view.get_block(pos);
        match kind_of(state) {
            Kind::Wire => {
                if suppress_wire {
                    0
                } else {
                    self.block_signal(state, dir, suppress_wire)
                }
            }
            Kind::Repeater | Kind::Comparator => self.block_signal(state, dir, suppress_wire),
            Kind::Torch | Kind::WallTorch => {
                if dir == Direction::Down {
                    self.block_signal(state, dir, suppress_wire)
                } else {
                    0
                }
            }
            Kind::Lever | Kind::Button if powered(state) && connected_direction(state) == dir => 15,
            Kind::Lever | Kind::Button => 0,
            _ => 0,
        }
    }

    fn direct_signal_to<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        suppress_wire: bool,
    ) -> u8 {
        let mut best = 0;
        for dir in Direction::ALL {
            let neighbour = pos.offset(dir);
            best = best.max(self.direct_signal(view, neighbour, dir, suppress_wire));
        }
        best
    }

    fn best_neighbor<V: RedstoneView>(&self, view: &V, pos: BlockPos, suppress_wire: bool) -> u8 {
        let mut best = 0;
        for dir in Direction::ALL {
            let signal = self.signal(view, pos.offset(dir), dir, suppress_wire);
            if signal >= 15 {
                return 15;
            }
            best = best.max(signal);
        }
        best
    }

    /// `BlockState.getSignal`, without the conductor pass-through.
    ///
    /// `dir` points from the receiver toward this block.
    fn block_signal(&self, state: BlockStateId, dir: Direction, suppress_wire: bool) -> u8 {
        match kind_of(state) {
            Kind::Wire => {
                if suppress_wire {
                    return 0;
                }
                let p = power(state);
                if p == 0 || dir == Direction::Down {
                    0
                } else if dir == Direction::Up || side_connected(state, dir.opposite()) {
                    p
                } else {
                    0
                }
            }
            Kind::Repeater | Kind::Comparator => {
                if powered(state) && facing(state) == dir {
                    15
                } else {
                    0
                }
            }
            Kind::Torch => {
                if lit(state) && dir != Direction::Up {
                    15
                } else {
                    0
                }
            }
            Kind::WallTorch => {
                if lit(state) && facing(state) != dir {
                    15
                } else {
                    0
                }
            }
            Kind::Lever | Kind::Button => {
                if powered(state) {
                    15
                } else {
                    0
                }
            }
            Kind::RedstoneBlock => 15,
            _ => 0,
        }
    }

    /// The control input a diode reads from a side block (`getControlInputSignal`).
    fn control_input<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        dir: Direction,
        diodes_only: bool,
    ) -> u8 {
        let state = view.get_block(pos);
        if diodes_only {
            if matches!(kind_of(state), Kind::Repeater | Kind::Comparator) {
                self.direct_signal(view, pos, dir, false)
            } else {
                0
            }
        } else {
            match kind_of(state) {
                Kind::RedstoneBlock => 15,
                Kind::Wire => power(state),
                _ if is_signal_source(state) => self.direct_signal(view, pos, dir, false),
                _ => 0,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Wire shape
    // -----------------------------------------------------------------------

    /// A conductor is a full solid opaque cube, the engine's summary of
    /// vanilla's full-collision-shape test. Also stands in for a sturdy face.
    fn face_sturdy<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> bool {
        view.is_conductor(pos)
    }

    fn can_survive_on<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> bool {
        self.face_sturdy(view, pos)
    }

    /// `RedStoneWireBlock.shouldConnectTo`: whether wire draws a connection
    /// toward this state. `dir` is `None` for the "no particular direction"
    /// query.
    fn should_connect_to(state: BlockStateId, dir: Option<Direction>) -> bool {
        match kind_of(state) {
            Kind::Wire => true,
            Kind::Repeater | Kind::Comparator => {
                let f = facing(state);
                dir == Some(f) || Some(f.opposite()) == dir
            }
            _ if is_signal_source(state) => dir.is_some(),
            _ => false,
        }
    }

    fn wire_connecting_side<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        dir: Direction,
        above_not_conductor: bool,
    ) -> &'static str {
        let neighbour = pos.offset(dir);
        let state = view.get_block(neighbour);
        if above_not_conductor {
            let sturdy = is_trapdoor(state) || self.can_survive_on(view, neighbour);
            if sturdy
                && Self::should_connect_to(view.get_block(neighbour.offset(Direction::Up)), None)
            {
                if self.face_sturdy(view, neighbour) {
                    return "up";
                }
                return "side";
            }
        }
        let connects = Self::should_connect_to(state, Some(dir));
        if !connects
            && (view.is_conductor(neighbour)
                || !Self::should_connect_to(
                    view.get_block(neighbour.offset(Direction::Down)),
                    None,
                ))
        {
            "none"
        } else {
            "side"
        }
    }

    fn wire_missing_connections<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockStateId {
        let above_not_conductor = !view.is_conductor(pos.offset(Direction::Up));
        let mut state = state;
        for dir in Direction::HORIZONTAL {
            if !side_connected(state, dir) {
                let side = self.wire_connecting_side(view, pos, dir, above_not_conductor);
                state = set_side(state, dir, side);
            }
        }
        state
    }

    /// `RedStoneWireBlock.getConnectionState`, reduced to the shape.
    fn wire_connection_state<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockStateId {
        let was_dot = is_dot(state);
        let mut state = state;
        for dir in Direction::HORIZONTAL {
            state = set_side(state, dir, "none");
        }
        state = self.wire_missing_connections(view, pos, state);
        if was_dot && is_dot(state) {
            return state;
        }
        let north = side_connected(state, Direction::North);
        let south = side_connected(state, Direction::South);
        let east = side_connected(state, Direction::East);
        let west = side_connected(state, Direction::West);
        let ns_none = !north && !south;
        let ew_none = !east && !west;
        if !west && ns_none {
            state = set_side(state, Direction::West, "side");
        }
        if !east && ns_none {
            state = set_side(state, Direction::East, "side");
        }
        if !north && ew_none {
            state = set_side(state, Direction::North, "side");
        }
        if !south && ew_none {
            state = set_side(state, Direction::South, "side");
        }
        state
    }

    // -----------------------------------------------------------------------
    // Wire power
    // -----------------------------------------------------------------------

    /// `RedstoneWireEvaluator.getIncomingWireSignal`: the strongest neighbouring
    /// wire minus one, including the up-and-over and down-around cases.
    fn incoming_wire_signal<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> u8 {
        let mut best = 0;
        let above = pos.offset(Direction::Up);
        for dir in Direction::HORIZONTAL {
            let neighbour = pos.offset(dir);
            let state = view.get_block(neighbour);
            best = best.max(wire_power(state));
            if view.is_conductor(neighbour) && !view.is_conductor(above) {
                best = best.max(wire_power(view.get_block(neighbour.offset(Direction::Up))));
            } else if !view.is_conductor(neighbour) {
                best = best.max(wire_power(
                    view.get_block(neighbour.offset(Direction::Down)),
                ));
            }
        }
        best.saturating_sub(1)
    }

    fn wire_target_strength<V: RedstoneView>(&self, view: &V, pos: BlockPos) -> u8 {
        // A wire ignores every other wire while computing its own input, so a
        // block powered by wire cannot re-power wire next to it. Sources and
        // strong power still count.
        let block_signal = self.best_neighbor(view, pos, true);
        if block_signal == 15 {
            return 15;
        }
        block_signal.max(self.incoming_wire_signal(view, pos))
    }

    fn wire_update<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let shaped = self.wire_connection_state(view, pos, state);
        let target = self.wire_target_strength(view, pos);
        let new = props::with(shaped, "power", &target.to_string()).unwrap_or(shaped);
        if new != state {
            view.set_block(pos, new);
            self.notify_around(queue, pos);
        }
    }

    // -----------------------------------------------------------------------
    // Repeater
    // -----------------------------------------------------------------------

    fn diode_delay(state: BlockStateId) -> u32 {
        u32::from(delay(state)) * 2
    }

    fn diode_input_signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> u8 {
        let dir = facing(state);
        let input = pos.offset(dir);
        let signal = self.signal(view, input, dir, false);
        if signal >= 15 {
            return signal;
        }
        let neighbour = view.get_block(input);
        signal.max(wire_power(neighbour))
    }

    fn diode_should_turn_on<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> bool {
        self.diode_input_signal(view, pos, state) > 0
    }

    fn diode_alternate_signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> u8 {
        let f = facing(state);
        let cw = f.clockwise();
        let ccw = f.counter_clockwise();
        self.control_input(view, pos.offset(cw), cw, true)
            .max(self.control_input(view, pos.offset(ccw), ccw, true))
    }

    fn diode_is_locked<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> bool {
        self.diode_alternate_signal(view, pos, state) > 0
    }

    /// Keep the repeater's `locked` property in step with its side inputs, the
    /// way vanilla's `updateShape` does.
    fn diode_update_locked<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockStateId {
        let locked = self.diode_is_locked(view, pos, state);
        let current = props::bool_or(state, "locked", false);
        if locked == current {
            return state;
        }
        let value = if locked { "true" } else { "false" };
        let new = props::with(state, "locked", value).unwrap_or(state);
        view.set_block(pos, new);
        self.update_neighbors_at(queue, pos);
        new
    }

    fn diode_should_prioritize<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> bool {
        let opposite = facing(state).opposite();
        let neighbour = view.get_block(pos.offset(opposite));
        is_diode(neighbour) && facing(neighbour) != opposite
    }

    fn diode_check_tick<V: RedstoneView>(
        &self,
        view: &mut V,
        _queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        if self.diode_is_locked(view, pos, state) {
            return;
        }
        let is_on = powered(state);
        let should = self.diode_should_turn_on(view, pos, state);
        if is_on != should && !view.will_tick_this_tick(pos) {
            let priority = if self.diode_should_prioritize(view, pos, state) {
                TickPriority::ExtremelyHigh
            } else if is_on {
                TickPriority::VeryHigh
            } else {
                TickPriority::High
            };
            view.schedule_block_tick(pos, Self::diode_delay(state), priority);
        }
    }

    fn diode_tick<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        if self.diode_is_locked(view, pos, state) {
            return;
        }
        let is_on = powered(state);
        let should = self.diode_should_turn_on(view, pos, state);
        if is_on && !should {
            let new = props::with(state, "powered", "false").unwrap_or(state);
            self.apply(view, queue, pos, new);
        } else if !is_on {
            let new = props::with(state, "powered", "true").unwrap_or(state);
            self.apply(view, queue, pos, new);
            // Pulse extension: the input may already be gone, in which case the
            // repeater holds for one more delay before dropping.
            if !should {
                view.schedule_block_tick(pos, Self::diode_delay(state), TickPriority::VeryHigh);
            }
        }
    }

    fn diode_update_neighbors_in_front<V: RedstoneView>(
        &self,
        _view: &V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let f = facing(state);
        let output = pos.offset(f.opposite());
        queue.push_back(Update(output));
        self.update_neighbors_except(queue, output, f);
    }

    fn repeater_use<V: RedstoneView>(
        &self,
        view: &mut V,
        _queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let next = if delay(state) >= 4 {
            1
        } else {
            delay(state) + 1
        };
        let new = props::with(state, "delay", &next.to_string()).unwrap_or(state);
        view.set_block(pos, new);
    }

    // -----------------------------------------------------------------------
    // Redstone lamp
    // -----------------------------------------------------------------------

    fn lamp_neighbor_changed<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let is_lit = lit(state);
        if is_lit != self.has_neighbor_signal(view, pos) {
            if is_lit {
                view.schedule_block_tick(pos, 4, TickPriority::Normal);
            } else {
                let new = props::with(state, "lit", "true").unwrap_or(state);
                self.apply(view, queue, pos, new);
            }
        }
    }

    fn lamp_tick<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        if lit(state) && !self.has_neighbor_signal(view, pos) {
            let new = props::with(state, "lit", "false").unwrap_or(state);
            self.apply(view, queue, pos, new);
        }
    }

    // -----------------------------------------------------------------------
    // Update plumbing
    // -----------------------------------------------------------------------

    /// `Level.updateNeighborsAt`: notify a cell's six neighbours.
    fn update_neighbors_at(&self, queue: &mut VecDeque<Update>, pos: BlockPos) {
        for dir in Direction::UPDATE_ORDER {
            queue.push_back(Update(pos.offset(dir)));
        }
    }

    fn update_neighbors_except(
        &self,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        skip: Direction,
    ) {
        for dir in Direction::UPDATE_ORDER {
            if dir != skip {
                queue.push_back(Update(pos.offset(dir)));
            }
        }
    }

    /// The 7-cell blast a wire uses when its power changes: the wire itself and
    /// its six neighbours, each then notifying its own neighbours.
    fn notify_around(&self, queue: &mut VecDeque<Update>, pos: BlockPos) {
        let mut cells = vec![pos];
        for dir in Direction::ALL {
            let p = pos.offset(dir);
            if !cells.contains(&p) {
                cells.push(p);
            }
        }
        for p in cells {
            self.update_neighbors_at(queue, p);
        }
    }

    /// `updateNeighborsOfNeighboringWires`.
    fn update_neighbors_of_neighboring_wires<V: RedstoneView>(
        &self,
        view: &V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
    ) {
        for dir in Direction::HORIZONTAL {
            self.check_corner_change_at(view, queue, pos.offset(dir));
        }
        for dir in Direction::HORIZONTAL {
            let neighbour = pos.offset(dir);
            let target = if view.is_conductor(neighbour) {
                neighbour.offset(Direction::Up)
            } else {
                neighbour.offset(Direction::Down)
            };
            self.check_corner_change_at(view, queue, target);
        }
    }

    fn check_corner_change_at<V: RedstoneView>(
        &self,
        view: &V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
    ) {
        if kind_of(view.get_block(pos)) == Kind::Wire {
            self.update_neighbors_at(queue, pos);
            for dir in Direction::ALL {
                self.update_neighbors_at(queue, pos.offset(dir));
            }
        }
    }

    fn drain<V: RedstoneView>(&self, view: &mut V, mut queue: VecDeque<Update>) {
        let mut guard = 0usize;
        while let Some(Update(pos)) = queue.pop_front() {
            guard += 1;
            if guard > MAX_UPDATES {
                break;
            }
            self.dispatch(view, &mut queue, pos);
        }
    }

    fn dispatch<V: RedstoneView>(&self, view: &mut V, queue: &mut VecDeque<Update>, pos: BlockPos) {
        let state = view.get_block(pos);
        match kind_of(state) {
            Kind::Wire => self.wire_update(view, queue, pos, state),
            Kind::Repeater => {
                let state = self.diode_update_locked(view, queue, pos, state);
                self.diode_check_tick(view, queue, pos, state);
            }
            Kind::Torch | Kind::WallTorch => self.torch_neighbor_changed(view, queue, pos, state),
            Kind::Lamp => self.lamp_neighbor_changed(view, queue, pos, state),
            _ => {}
        }
    }

    /// Write `new` at `pos` if it changed, waking the neighbours the way vanilla
    /// `setBlock(..., 2)` does.
    fn apply<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        new: BlockStateId,
    ) {
        if view.get_block(pos) != new {
            view.set_block(pos, new);
            self.update_neighbors_at(queue, pos);
        }
    }

    // -----------------------------------------------------------------------
    // Entry points
    // -----------------------------------------------------------------------

    /// A block was placed at `pos`. The host writes it first, then calls this.
    pub fn on_placed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut queue = VecDeque::new();
        self.update_neighbors_at(&mut queue, pos);
        queue.push_back(Update(pos));
        let state = view.get_block(pos);
        if is_diode(state) {
            self.diode_update_neighbors_in_front(view, &mut queue, pos, state);
        }
        self.drain(view, queue);
    }

    /// A block was removed from `pos`, whose old state was `old`.
    pub fn on_removed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos, old: BlockStateId) {
        let mut queue = VecDeque::new();
        match kind_of(old) {
            Kind::Wire => {
                self.update_neighbors_at(&mut queue, pos);
                self.update_neighbors_of_neighboring_wires(view, &mut queue, pos);
            }
            Kind::Torch | Kind::WallTorch => self.torch_notify_neighbors(&mut queue, pos),
            Kind::Repeater => self.diode_update_neighbors_in_front(view, &mut queue, pos, old),
            Kind::Lever | Kind::Button => {
                if powered(old) {
                    self.update_powered_source_neighbours(&mut queue, pos, old);
                }
            }
            _ => self.update_neighbors_at(&mut queue, pos),
        }
        self.drain(view, queue);
    }

    /// A neighbour of the block at `pos` (the block at `from`) changed.
    pub fn on_neighbour_changed<V: RedstoneView>(
        &self,
        view: &mut V,
        pos: BlockPos,
        from: BlockPos,
    ) {
        let _ = from;
        let mut queue = VecDeque::new();
        queue.push_back(Update(pos));
        self.drain(view, queue);
    }

    /// A block tick scheduled for `pos` has come due.
    pub fn on_scheduled_tick<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut queue = VecDeque::new();
        let state = view.get_block(pos);
        match kind_of(state) {
            Kind::Repeater => self.diode_tick(view, &mut queue, pos, state),
            Kind::Torch | Kind::WallTorch => self.torch_tick(view, &mut queue, pos, state),
            Kind::Lamp => self.lamp_tick(view, &mut queue, pos, state),
            Kind::Button => self.button_tick(view, &mut queue, pos, state),
            _ => {}
        }
        self.drain(view, queue);
    }

    /// A player used the block at `pos`: a lever pulls, a button presses, a
    /// repeater cycles its delay, a wire toggles cross/dot.
    pub fn on_use<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let mut queue = VecDeque::new();
        let state = view.get_block(pos);
        match kind_of(state) {
            Kind::Lever => self.lever_use(view, &mut queue, pos, state),
            Kind::Button => self.button_use(view, &mut queue, pos, state),
            Kind::Repeater => self.repeater_use(view, &mut queue, pos, state),
            Kind::Wire => self.wire_use(view, &mut queue, pos, state),
            _ => {}
        }
        self.drain(view, queue);
    }

    // -----------------------------------------------------------------------
    // Levers and buttons
    // -----------------------------------------------------------------------

    fn update_powered_source_neighbours(
        &self,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let connected = connected_direction(state);
        self.update_neighbors_at(queue, pos);
        self.update_neighbors_at(queue, pos.offset(connected.opposite()));
    }

    fn lever_use<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let value = if powered(state) { "false" } else { "true" };
        let new = props::with(state, "powered", value).unwrap_or(state);
        view.set_block(pos, new);
        self.update_powered_source_neighbours(queue, pos, new);
    }

    fn button_ticks(state: BlockStateId) -> u32 {
        // Stone is 20 ticks, wood 30 — vanilla's BlockSetType.
        let name = props::name_of(state).unwrap_or("");
        let wood = [
            "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry",
            "bamboo", "crimson", "warped", "pale_oak",
        ];
        if wood.iter().any(|w| name.contains(w)) {
            30
        } else {
            20
        }
    }

    fn button_use<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        if powered(state) {
            return;
        }
        let new = props::with(state, "powered", "true").unwrap_or(state);
        view.set_block(pos, new);
        self.update_powered_source_neighbours(queue, pos, new);
        view.schedule_block_tick(pos, Self::button_ticks(state), TickPriority::Normal);
    }

    fn button_tick<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        if powered(state) {
            let new = props::with(state, "powered", "false").unwrap_or(state);
            view.set_block(pos, new);
            self.update_powered_source_neighbours(queue, pos, new);
        }
    }

    // -----------------------------------------------------------------------
    // Redstone torch
    // -----------------------------------------------------------------------

    fn torch_has_neighbor_signal<V: RedstoneView>(
        &self,
        view: &V,
        pos: BlockPos,
        state: BlockStateId,
    ) -> bool {
        match kind_of(state) {
            Kind::Torch => {
                self.signal(view, pos.offset(Direction::Down), Direction::Down, false) > 0
            }
            Kind::WallTorch => {
                let dir = facing(state).opposite();
                self.signal(view, pos.offset(dir), dir, false) > 0
            }
            _ => false,
        }
    }

    fn torch_toggle_count<V: RedstoneView>(&self, view: &V, pos: BlockPos, now: u64) -> u32 {
        view.toggles_since(pos, now.saturating_sub(60))
    }

    fn torch_neighbor_changed<V: RedstoneView>(
        &self,
        view: &mut V,
        _queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        if lit(state) == self.torch_has_neighbor_signal(view, pos, state)
            && !view.will_tick_this_tick(pos)
        {
            view.schedule_block_tick(pos, 2, TickPriority::Normal);
        }
    }

    fn torch_tick<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let signal = self.torch_has_neighbor_signal(view, pos, state);
        if lit(state) {
            if signal {
                let new = props::with(state, "lit", "false").unwrap_or(state);
                self.apply(view, queue, pos, new);
                let now = view.game_time();
                view.push_toggle(pos, now);
                if self.torch_toggle_count(view, pos, now) >= 8 {
                    view.schedule_block_tick(pos, 160, TickPriority::Normal);
                }
            }
        } else if !signal {
            let now = view.game_time();
            if self.torch_toggle_count(view, pos, now) < 8 {
                let new = props::with(state, "lit", "true").unwrap_or(state);
                self.apply(view, queue, pos, new);
            }
        }
    }

    fn torch_notify_neighbors(&self, queue: &mut VecDeque<Update>, pos: BlockPos) {
        for dir in Direction::ALL {
            self.update_neighbors_at(queue, pos.offset(dir));
        }
    }

    // -----------------------------------------------------------------------
    // Wire interaction
    // -----------------------------------------------------------------------

    fn wire_use<V: RedstoneView>(
        &self,
        view: &mut V,
        queue: &mut VecDeque<Update>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let mut toggled = state;
        if is_cross(state) {
            for dir in Direction::HORIZONTAL {
                toggled = set_side(toggled, dir, "none");
            }
        } else if is_dot(state) {
            for dir in Direction::HORIZONTAL {
                toggled = set_side(toggled, dir, "side");
            }
        } else {
            return;
        }
        let shaped = self.wire_connection_state(view, pos, toggled);
        if shaped != state {
            view.set_block(pos, shaped);
            self.notify_around(queue, pos);
        }
    }
}
