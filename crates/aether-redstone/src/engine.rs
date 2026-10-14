//! The redstone engine and the vanilla behaviour it models.
//!
//! One [`Redstone`] value drives any [`RedstoneView`]. It owns no world state,
//! so every method is a pure function of the view it is handed: call it twice
//! on the same view and you get the same result. The transient update queue a
//! call needs lives on the stack for the duration of that call.
//!
//! This module holds the **power model** — weak and strong signal, conductor
//! pass-through, and the update plumbing — plus the simple sources: levers,
//! buttons and redstone torches. Wire, repeaters and lamps are added by later
//! stages of the same crate.

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

#[allow(dead_code)]
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
    #[allow(dead_code)]
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
    // Update plumbing
    // -----------------------------------------------------------------------

    /// `Level.updateNeighborsAt`: notify a cell's six neighbours.
    fn update_neighbors_at(&self, queue: &mut VecDeque<Update>, pos: BlockPos) {
        for dir in Direction::UPDATE_ORDER {
            queue.push_back(Update(pos.offset(dir)));
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
            Kind::Torch | Kind::WallTorch => self.torch_neighbor_changed(view, queue, pos, state),
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
        self.drain(view, queue);
    }

    /// A block was removed from `pos`, whose old state was `old`.
    pub fn on_removed<V: RedstoneView>(&self, view: &mut V, pos: BlockPos, old: BlockStateId) {
        let mut queue = VecDeque::new();
        match kind_of(old) {
            Kind::Torch | Kind::WallTorch => self.torch_notify_neighbors(&mut queue, pos),
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
            Kind::Torch | Kind::WallTorch => self.torch_tick(view, &mut queue, pos, state),
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
}
