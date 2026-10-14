//! Per-block redstone behaviour, and the dispatch that reaches it.
//!
//! R1 dispatched on the block's *name* inside the engine: a `match` over
//! `&str`, a `Kind` enum, and a `block_signal` `match` that every new component
//! would have to edit. That cannot be extended without rewriting the core.
//!
//! Here a block implements [`BlockBehaviour`], and a [`BlockStateId`]-indexed
//! table maps a state to its behaviour. The table is built **once**, on first
//! use; on the hot path a lookup is one array index, with no string comparison.
//! Adding comparator, observer or piston later is "write a module and register
//! it" — the engine and this file do not change.
//!
//! [`Context`] is the mutable view a behaviour acts through: it reads and
//! writes blocks and runs the neighbour-update cascade, a port of vanilla's
//! depth-first `CollectingNeighborUpdater`. [`SignalCtx`] is the read-only view
//! the two signal methods use, so a power query cannot mutate the world.

use std::sync::OnceLock;

use aether_world::registry::{blocks as world_blocks, ids};
use aether_world::BlockStateId;

use crate::blocks::{self, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::view::{Orientation, RedstoneView, SetBlockFlags, TickPriority};

/// The block that caused a neighbour update, with its position.
#[derive(Debug, Clone, Copy)]
pub struct NeighborSource {
    /// Where the change happened.
    pub pos: BlockPos,
    /// The source block's state at the time of the notification.
    pub state: BlockStateId,
    /// The redstone orientation of the update, when there is one.
    pub orientation: Option<Orientation>,
}

impl NeighborSource {
    /// A source with no orientation.
    pub const fn plain(pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            pos,
            state,
            orientation: None,
        }
    }
}

/// The chain limit for the neighbour-update cascade — vanilla's
/// `maxChainedNeighborUpdates`. Beyond it the rest is dropped with an error.
const MAX_CHAINED: usize = 512;

// ---------------------------------------------------------------------------
// Read-only signal context
// ---------------------------------------------------------------------------

/// Read access to the world for signal queries.
///
/// `suppress_wire` is vanilla's `RedStoneWireBlock.shouldSignal = false`: while
/// a wire computes its own input, every wire's own output is suppressed, so a
/// block powered by wire cannot re-power wire beside it.
pub struct SignalCtx<'a> {
    pub(crate) view: &'a dyn RedstoneView,
    pub(crate) suppress_wire: bool,
}

impl SignalCtx<'_> {
    /// The state at `pos`.
    pub fn get_block(&self, pos: BlockPos) -> BlockStateId {
        self.view.get_block(pos)
    }

    /// The behaviour for a state.
    pub fn behaviour(&self, state: BlockStateId) -> &'static dyn BlockBehaviour {
        behaviour(state)
    }

    /// Whether `pos` is a redstone conductor.
    pub fn is_conductor(&self, pos: BlockPos) -> bool {
        let state = self.get_block(pos);
        self.behaviour(state).is_conductor(state)
    }

    /// `getSignal(pos, dir)` — weak power emitted toward the neighbour in `dir`.
    pub fn get_signal(&self, pos: BlockPos, dir: Direction) -> u8 {
        let state = self.get_block(pos);
        let behaviour = self.behaviour(state);
        let own = behaviour.get_signal(self, state, pos, dir);
        if behaviour.is_conductor(state) {
            own.max(self.direct_signal_to(pos))
        } else {
            own
        }
    }

    /// `getDirectSignal(pos, dir)` — strong power provided to `dir`.
    pub fn get_direct_signal(&self, pos: BlockPos, dir: Direction) -> u8 {
        let state = self.get_block(pos);
        self.behaviour(state)
            .get_direct_signal(self, state, pos, dir)
    }

    /// `getDirectSignalTo(pos)` — the strongest strong signal arriving from any
    /// of the six neighbours.
    pub fn direct_signal_to(&self, pos: BlockPos) -> u8 {
        let mut best = 0;
        for dir in Direction::ALL {
            let signal = self.get_direct_signal(pos.offset(dir), dir);
            if signal >= 15 {
                return 15;
            }
            best = best.max(signal);
        }
        best
    }

    /// `getBestNeighborSignal(pos)`.
    pub fn get_best_neighbor_signal(&self, pos: BlockPos) -> u8 {
        let mut best = 0;
        for dir in Direction::ALL {
            let signal = self.get_signal(pos.offset(dir), dir);
            if signal >= 15 {
                return 15;
            }
            best = best.max(signal);
        }
        best
    }

    /// `hasNeighborSignal(pos)`.
    pub fn has_neighbor_signal(&self, pos: BlockPos) -> bool {
        self.get_best_neighbor_signal(pos) > 0
    }

    /// A read context with wire output suppressed, for computing wire input.
    pub(crate) fn suppressed(&self) -> SignalCtx<'_> {
        SignalCtx {
            view: self.view,
            suppress_wire: true,
        }
    }
}

// ---------------------------------------------------------------------------
// The neighbour-update cascade
// ---------------------------------------------------------------------------

/// One pending neighbour update — the records vanilla's `CollectingNeighborUpdater`
/// keeps.
enum NeighborUpdate {
    /// `SimpleNeighborUpdate`: notify a single cell.
    Simple {
        pos: BlockPos,
        source: BlockStateId,
        orientation: Option<Orientation>,
    },
    /// `MultiNeighborUpdate`: notify a cell's neighbours in `UPDATE_ORDER`,
    /// optionally skipping one face, yielding between each.
    Multi {
        source_pos: BlockPos,
        source: BlockStateId,
        skip: Option<Direction>,
        orientation: Option<Orientation>,
        idx: usize,
    },
    /// `ShapeUpdate`: ask a neighbour for its `updateShape`.
    Shape {
        direction: Direction,
        neighbor: BlockPos,
        flags: SetBlockFlags,
    },
}

// ---------------------------------------------------------------------------
// Mutable context
// ---------------------------------------------------------------------------

/// The mutable world a behaviour acts through, plus the update cascade.
pub struct Context<'a> {
    pub(crate) view: &'a mut dyn RedstoneView,
    stack: Vec<NeighborUpdate>,
    added: Vec<NeighborUpdate>,
    count: usize,
    max_chained: usize,
}

impl<'a> Context<'a> {
    /// Wrap a world for the duration of one engine call.
    pub fn new(view: &'a mut dyn RedstoneView) -> Self {
        Self {
            view,
            stack: Vec::new(),
            added: Vec::new(),
            count: 0,
            max_chained: MAX_CHAINED,
        }
    }

    // -- reads --------------------------------------------------------------

    /// The state at `pos`.
    pub fn get_block(&self, pos: BlockPos) -> BlockStateId {
        self.view.get_block(pos)
    }

    /// Whether `pos` is a redstone conductor.
    pub fn is_conductor(&self, pos: BlockPos) -> bool {
        let state = self.get_block(pos);
        behaviour(state).is_conductor(state)
    }

    /// A read-only signal context (wire output active).
    pub fn signals(&self) -> SignalCtx<'_> {
        SignalCtx {
            view: self.view,
            suppress_wire: false,
        }
    }

    /// A read-only signal context with wire output suppressed.
    pub fn signals_suppressed(&self) -> SignalCtx<'_> {
        SignalCtx {
            view: self.view,
            suppress_wire: true,
        }
    }

    /// The current game time in ticks.
    pub fn game_time(&self) -> u64 {
        self.view.game_time()
    }

    // -- mutation -----------------------------------------------------------

    /// Vanilla's `Level.setBlock(pos, state, flags)`.
    ///
    /// The raw write is the host's; this runs the side effects the flags select:
    /// the removal hook when the block changes and `UPDATE_NEIGHBORS` is set,
    /// the placement hook unless `UPDATE_SKIP_ON_PLACE`, the neighbour fan-out
    /// when `UPDATE_NEIGHBORS` is set, and the shape updates unless
    /// `UPDATE_KNOWN_SHAPE`.
    ///
    /// Returns `false` if the state was already `state`.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockStateId, flags: SetBlockFlags) -> bool {
        let old = self.get_block(pos);
        if old == state {
            return false;
        }
        let old_block = world_blocks::block_of_state(old).map(|(b, _)| b);
        let new_block = world_blocks::block_of_state(state).map(|(b, _)| b);
        let moved = flags.contains(SetBlockFlags::UPDATE_MOVE_BY_PISTON);

        if old_block != new_block && (flags.contains(SetBlockFlags::UPDATE_NEIGHBORS) || moved) {
            behaviour(old).on_remove(self, old, pos);
        }

        self.view.set_block(pos, state);

        if !flags.contains(SetBlockFlags::UPDATE_SKIP_ON_PLACE) {
            behaviour(state).on_place(self, state, pos, old);
        }

        if flags.contains(SetBlockFlags::UPDATE_NEIGHBORS) {
            self.update_neighbors_at_with(pos, old, None);
        }

        if !flags.contains(SetBlockFlags::UPDATE_KNOWN_SHAPE) {
            self.update_neighbour_shapes(pos, flags);
        }
        true
    }

    /// Write `new` at `pos` if it differs, using the flags a block state change
    /// normally carries (`UPDATE_CLIENTS`): the placement hook runs, but the
    /// neighbours are notified by the hook, not by a blanket fan-out.
    pub fn apply(&mut self, pos: BlockPos, new: BlockStateId) {
        self.set_block(pos, new, SetBlockFlags::CLIENTS);
    }

    // -- scheduling ---------------------------------------------------------

    /// Schedule a block tick, keyed by the block currently at `pos`.
    pub fn schedule_tick(&mut self, pos: BlockPos, delay: u32, priority: TickPriority) {
        if let Some(block) = block_id_of(self.get_block(pos)) {
            self.view.schedule_block_tick(block, pos, delay, priority);
        }
    }

    /// Whether a tick for the block currently at `pos` is pending this game tick.
    pub fn will_tick_this_tick(&self, pos: BlockPos) -> bool {
        match block_id_of(self.get_block(pos)) {
            Some(block) => self.view.will_tick_this_tick(block, pos),
            None => false,
        }
    }

    /// Record a redstone-torch toggle on the world's burnout log.
    pub fn push_toggle(&mut self, pos: BlockPos, when: u64) {
        self.view.record_toggle(pos, when);
    }

    /// Toggles recorded at or after `since`, from the world's burnout log.
    pub fn toggles_since(&self, pos: BlockPos, since: u64) -> u32 {
        self.view.toggle_count(pos, since)
    }

    // -- neighbour updates --------------------------------------------------

    /// `Level.updateNeighborsAt`: notify a cell's six neighbours.
    pub fn update_neighbors_at(&mut self, pos: BlockPos) {
        let source = self.get_block(pos);
        self.update_neighbors_at_with(pos, source, None);
    }

    /// Notify a cell's neighbours, with an explicit source block.
    pub fn update_neighbors_at_with(
        &mut self,
        pos: BlockPos,
        source: BlockStateId,
        orientation: Option<Orientation>,
    ) {
        self.add_and_run(NeighborUpdate::Multi {
            source_pos: pos,
            source,
            skip: None,
            orientation,
            idx: 0,
        });
    }

    /// `Level.updateNeighborsAtExceptFromFacing`.
    pub fn update_neighbors_at_except_from_facing(
        &mut self,
        pos: BlockPos,
        source: BlockStateId,
        skip: Direction,
        orientation: Option<Orientation>,
    ) {
        self.add_and_run(NeighborUpdate::Multi {
            source_pos: pos,
            source,
            skip: Some(skip),
            orientation,
            idx: 0,
        });
    }

    /// Notify a cell's neighbours, skipping the face in `skip`.
    pub fn update_neighbors_except(&mut self, pos: BlockPos, skip: Direction) {
        let source = self.get_block(pos);
        self.update_neighbors_at_except_from_facing(pos, source, skip, None);
    }

    /// `Level.neighborChanged(pos, block)`: notify one cell.
    pub fn neighbor_changed_at(&mut self, pos: BlockPos, source: BlockStateId) {
        self.add_and_run(NeighborUpdate::Simple {
            pos,
            source,
            orientation: None,
        });
    }

    /// The seven-cell blast a wire uses when its power changes: the wire and
    /// its six neighbours, each then notifying its own neighbours.
    pub fn notify_around(&mut self, pos: BlockPos) {
        let source = self.get_block(pos);
        let mut cells = vec![pos];
        for dir in Direction::ALL {
            let p = pos.offset(dir);
            if !cells.contains(&p) {
                cells.push(p);
            }
        }
        for cell in cells {
            self.update_neighbors_at_with(cell, source, None);
        }
    }

    /// `updateNeighborsOfNeighboringWires`.
    pub fn update_neighbors_of_neighboring_wires(&mut self, pos: BlockPos) {
        for dir in Direction::HORIZONTAL {
            self.check_corner_change_at(pos.offset(dir));
        }
        for dir in Direction::HORIZONTAL {
            let neighbour = pos.offset(dir);
            let target = if self.is_conductor(neighbour) {
                neighbour.offset(Direction::Up)
            } else {
                neighbour.offset(Direction::Down)
            };
            self.check_corner_change_at(target);
        }
    }

    fn check_corner_change_at(&mut self, pos: BlockPos) {
        let state = self.get_block(pos);
        if behaviour(state).is_wire() {
            self.update_neighbors_at(pos);
            for dir in Direction::ALL {
                self.update_neighbors_at(pos.offset(dir));
            }
        }
    }

    // -- dispatch -----------------------------------------------------------

    /// Dispatch a neighbour change to the block's behaviour.
    pub fn dispatch_neighbor_changed(&mut self, pos: BlockPos, from: Option<NeighborSource>) {
        let state = self.get_block(pos);
        behaviour(state).neighbor_changed(self, state, pos, from);
    }

    /// Dispatch a scheduled tick to the block's behaviour.
    pub fn dispatch_tick(&mut self, pos: BlockPos) {
        let state = self.get_block(pos);
        behaviour(state).tick(self, state, pos);
    }

    /// Dispatch a player interaction to the block's behaviour.
    pub fn dispatch_use(&mut self, pos: BlockPos) {
        let state = self.get_block(pos);
        behaviour(state).use_block(self, state, pos);
    }

    // -- the cascade --------------------------------------------------------

    /// `CollectingNeighborUpdater.addAndRun`.
    fn add_and_run(&mut self, update: NeighborUpdate) {
        let running = self.count > 0;
        let too_many = self.count >= self.max_chained;
        self.count += 1;
        if !too_many {
            if running {
                self.added.push(update);
            } else {
                self.stack.push(update);
            }
        }
        if !running {
            self.run_updates();
        }
    }

    /// `CollectingNeighborUpdater.runUpdates`: depth-first, LIFO, yielding to
    /// freshly generated updates before resuming a multi-neighbour update.
    fn run_updates(&mut self) {
        while !self.stack.is_empty() || !self.added.is_empty() {
            while let Some(update) = self.added.pop() {
                self.stack.push(update);
            }
            while let Some(mut current) = self.stack.pop() {
                let more = self.run_next(&mut current);
                if more {
                    self.stack.push(current);
                }
                if !self.added.is_empty() {
                    break;
                }
            }
        }
        self.stack.clear();
        self.added.clear();
        self.count = 0;
    }

    fn run_next(&mut self, update: &mut NeighborUpdate) -> bool {
        match update {
            NeighborUpdate::Simple {
                pos,
                source,
                orientation,
            } => {
                let source = NeighborSource {
                    pos: *pos,
                    state: *source,
                    orientation: *orientation,
                };
                self.dispatch_neighbor_changed(*pos, Some(source));
                false
            }
            NeighborUpdate::Multi {
                source_pos,
                source,
                skip,
                orientation,
                idx,
            } => {
                if *idx >= Direction::UPDATE_ORDER.len() {
                    return false;
                }
                let dir = Direction::UPDATE_ORDER[*idx];
                *idx += 1;
                if Some(dir) == *skip {
                    if *idx < Direction::UPDATE_ORDER.len()
                        && Some(Direction::UPDATE_ORDER[*idx]) == *skip
                    {
                        *idx += 1;
                    }
                    return *idx < Direction::UPDATE_ORDER.len();
                }
                let target = source_pos.offset(dir);
                let source = NeighborSource {
                    pos: *source_pos,
                    state: *source,
                    orientation: *orientation,
                };
                self.dispatch_neighbor_changed(target, Some(source));
                if *idx < Direction::UPDATE_ORDER.len()
                    && Some(Direction::UPDATE_ORDER[*idx]) == *skip
                {
                    *idx += 1;
                }
                *idx < Direction::UPDATE_ORDER.len()
            }
            NeighborUpdate::Shape {
                direction,
                neighbor,
                flags,
            } => {
                self.execute_shape_update(*neighbor, *direction, *flags);
                false
            }
        }
    }

    pub(crate) fn update_neighbour_shapes(&mut self, pos: BlockPos, flags: SetBlockFlags) {
        let shape_flags = flags
            .without(SetBlockFlags::UPDATE_NEIGHBORS)
            .without(SetBlockFlags::UPDATE_SUPPRESS_DROPS);
        for dir in Direction::UPDATE_SHAPE_ORDER {
            self.add_and_run(NeighborUpdate::Shape {
                direction: dir.opposite(),
                neighbor: pos.offset(dir),
                flags: shape_flags,
            });
        }
    }

    /// `NeighborUpdater.executeShapeUpdate`.
    fn execute_shape_update(
        &mut self,
        neighbor: BlockPos,
        direction: Direction,
        flags: SetBlockFlags,
    ) {
        let state = self.get_block(neighbor);
        if let Some(new) = behaviour(state).update_shape(self, state, neighbor, direction) {
            if new != state {
                let nested = flags
                    .without(SetBlockFlags::UPDATE_NEIGHBORS)
                    .without(SetBlockFlags::UPDATE_SUPPRESS_DROPS)
                    .union(SetBlockFlags::UPDATE_KNOWN_SHAPE);
                self.set_block(neighbor, new, nested);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

struct Registry {
    by_state: Vec<&'static dyn BlockBehaviour>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// The behaviour of a block state. One array index; no string comparison.
pub fn behaviour(state: BlockStateId) -> &'static dyn BlockBehaviour {
    let registry = REGISTRY.get_or_init(build);
    registry
        .by_state
        .get(state.raw() as usize)
        .copied()
        .unwrap_or(&blocks::DEFAULT)
}

fn build() -> Registry {
    let mut by_state: Vec<&'static dyn BlockBehaviour> =
        vec![&blocks::DEFAULT; blocks::STATE_COUNT];

    fn register(
        by_state: &mut [&'static dyn BlockBehaviour],
        name: &str,
        b: &'static dyn BlockBehaviour,
    ) {
        if let Some(range) = world_blocks::states_of(name) {
            for state in range {
                by_state[state as usize] = b;
            }
        }
    }

    register(&mut by_state, "minecraft:redstone_wire", &blocks::Wire);
    register(&mut by_state, "minecraft:repeater", &blocks::Repeater);
    register(&mut by_state, "minecraft:comparator", &blocks::Comparator);
    register(&mut by_state, "minecraft:redstone_torch", &blocks::Torch);
    register(
        &mut by_state,
        "minecraft:redstone_wall_torch",
        &blocks::WallTorch,
    );
    register(&mut by_state, "minecraft:lever", &blocks::Lever);
    register(
        &mut by_state,
        "minecraft:redstone_block",
        &blocks::RedstoneBlock,
    );
    register(&mut by_state, "minecraft:redstone_lamp", &blocks::Lamp);
    register(
        &mut by_state,
        "minecraft:stone_button",
        &blocks::StoneButton,
    );
    register(
        &mut by_state,
        "minecraft:polished_blackstone_button",
        &blocks::StoneButton,
    );
    register(&mut by_state, "minecraft:oak_button", &blocks::WoodButton);
    register(
        &mut by_state,
        "minecraft:spruce_button",
        &blocks::WoodButton,
    );
    register(&mut by_state, "minecraft:birch_button", &blocks::WoodButton);
    register(
        &mut by_state,
        "minecraft:jungle_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:acacia_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:dark_oak_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:mangrove_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:cherry_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:bamboo_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:crimson_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:warped_button",
        &blocks::WoodButton,
    );
    register(
        &mut by_state,
        "minecraft:pale_oak_button",
        &blocks::WoodButton,
    );

    // A test-only block registered over the observer's states, proving that a
    // new component decides its own wire connection without a core edit.
    #[cfg(test)]
    register(&mut by_state, "minecraft:observer", &blocks::TestObserver);

    Registry { by_state }
}

/// Default state ids for the blocks the engine refers to by name.
pub mod block_ids {
    pub use aether_world::registry::ids;
}

/// A convenience predicate: the default-state id of a named block.
pub fn default_state(name: &str) -> Option<BlockStateId> {
    aether_world::registry::blocks::default_state(name)
}

/// Air, for behaviours that remove their block.
pub const AIR: BlockStateId = ids::AIR;

/// The block id of a state, for scheduled-tick identity.
pub fn block_id_of(state: BlockStateId) -> Option<u16> {
    world_blocks::block_of_state(state).map(|(b, _)| b)
}
