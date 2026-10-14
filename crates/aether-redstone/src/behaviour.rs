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
//! writes blocks and pushes neighbour updates. [`SignalCtx`] is the read-only
//! view the two signal methods use, so a power query cannot mutate the world.

use std::collections::VecDeque;
use std::sync::OnceLock;

use aether_world::registry::{blocks as world_blocks, ids};
use aether_world::BlockStateId;

use crate::blocks::{self, BlockBehaviour};
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::view::{RedstoneView, TickPriority};

/// The block that caused a neighbour update, with its position.
#[derive(Debug, Clone, Copy)]
pub struct NeighborSource {
    /// Where the change happened.
    pub pos: BlockPos,
    /// The source block's state at the time of the notification.
    pub state: BlockStateId,
}

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
// Mutable context
// ---------------------------------------------------------------------------

/// The mutable world a behaviour acts through.
///
/// It owns the pending neighbour updates for the current call. Later stages
/// replace the queue with vanilla's depth-first `CollectingNeighborUpdater`;
/// the behaviour-facing methods stay the same.
pub struct Context<'a> {
    pub(crate) view: &'a mut dyn RedstoneView,
    pub(crate) queue: VecDeque<BlockPos>,
}

impl<'a> Context<'a> {
    /// Wrap a world for the duration of one engine call.
    pub fn new(view: &'a mut dyn RedstoneView) -> Self {
        Self {
            view,
            queue: VecDeque::new(),
        }
    }

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

    /// The raw block write. It does not notify anyone; callers push the update
    /// themselves, as vanilla's flag-driven `Level.setBlock` does.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.view.set_block(pos, state);
    }

    /// Write `new` at `pos` if it differs, and notify the neighbours.
    pub fn apply(&mut self, pos: BlockPos, new: BlockStateId) {
        if self.get_block(pos) != new {
            self.set_block(pos, new);
            self.update_neighbors_at(pos);
        }
    }

    /// The current game time in ticks.
    pub fn game_time(&self) -> u64 {
        self.view.game_time()
    }

    /// Schedule a block tick (position-keyed this stage; block-keyed in R8).
    pub fn schedule_tick(&mut self, pos: BlockPos, delay: u32, priority: TickPriority) {
        self.view.schedule_block_tick(pos, delay, priority);
    }

    /// Whether a tick is already pending for `pos` this game tick.
    pub fn will_tick_this_tick(&self, pos: BlockPos) -> bool {
        self.view.will_tick_this_tick(pos)
    }

    /// Record a redstone-torch toggle for the burnout clock.
    pub fn push_toggle(&mut self, pos: BlockPos, when: u64) {
        self.view.push_toggle(pos, when);
    }

    /// Toggles recorded at or after `since`.
    pub fn toggles_since(&self, pos: BlockPos, since: u64) -> u32 {
        self.view.toggles_since(pos, since)
    }

    /// `Level.updateNeighborsAt`: notify a cell's six neighbours.
    pub fn update_neighbors_at(&mut self, pos: BlockPos) {
        for dir in Direction::UPDATE_ORDER {
            self.queue.push_back(pos.offset(dir));
        }
    }

    /// Notify a cell's neighbours, skipping the face in `skip`.
    pub fn update_neighbors_except(&mut self, pos: BlockPos, skip: Direction) {
        for dir in Direction::UPDATE_ORDER {
            if dir != skip {
                self.queue.push_back(pos.offset(dir));
            }
        }
    }

    /// The seven-cell blast a wire uses when its power changes: the wire and
    /// its six neighbours, each then notifying its own neighbours.
    pub fn notify_around(&mut self, pos: BlockPos) {
        let mut cells = vec![pos];
        for dir in Direction::ALL {
            let p = pos.offset(dir);
            if !cells.contains(&p) {
                cells.push(p);
            }
        }
        for cell in cells {
            self.update_neighbors_at(cell);
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

    /// Dispatch a neighbour change to the block's behaviour, the way
    /// `ServerLevel.tickBlock` / `handleNeighborChanged` dispatch.
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
