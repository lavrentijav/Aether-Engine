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

/// The initial recursion limit for shape updates — vanilla's default `j` of
/// `512` in `Block.updateOrDestroy` / `Level.setBlock`. Each nested shape
/// update decrements it, so a malformed shape loop terminates.
pub(crate) const SHAPE_UPDATE_LIMIT: u32 = 512;

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

    /// The analog output the block at `pos` reports, if any.
    pub fn analog_output(&self, pos: BlockPos) -> Option<u8> {
        self.view.analog_output(pos)
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
///
/// `changed` is the cell whose change caused the update (the source block's
/// position, which becomes [`NeighborSource::pos`]); `center` is the cell the
/// fan-out is measured from. They differ for the diode's except-facing update
/// and for a wire's seven-cell blast, where the source block is elsewhere.
enum NeighborUpdate {
    /// `SimpleNeighborUpdate`: notify a single cell.
    Simple {
        pos: BlockPos,
        changed: BlockPos,
        source: BlockStateId,
        orientation: Option<Orientation>,
    },
    /// `MultiNeighborUpdate`: notify a cell's neighbours in `UPDATE_ORDER`,
    /// optionally skipping one face, yielding between each.
    Multi {
        center: BlockPos,
        changed: BlockPos,
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
        limit: u32,
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
    /// Wrap a world for the duration of one engine call. The chain limit is
    /// the world's (vanilla's `maxChainedNeighborUpdates`, default 1,000,000).
    pub fn new(view: &'a mut dyn RedstoneView) -> Self {
        let max_chained = view.max_chained_neighbor_updates();
        Self {
            view,
            stack: Vec::new(),
            added: Vec::new(),
            count: 0,
            max_chained,
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

    /// A daylight detector's raw daylight at `pos`.
    pub fn sky_light(&self, pos: BlockPos) -> i32 {
        self.view.sky_light(pos)
    }

    /// The sun angle at `pos` in radians.
    pub fn sun_angle(&self, pos: BlockPos) -> f32 {
        self.view.sun_angle(pos)
    }

    /// The entity count a pressure plate reads at `pos`.
    pub fn entity_count(&self, pos: BlockPos) -> usize {
        self.view.entity_count(pos)
    }

    // -- mutation -----------------------------------------------------------

    /// Vanilla's `Level.setBlock(pos, state, flags)`.
    ///
    /// The raw write is the host's; this runs the side effects the flags select,
    /// in vanilla's order: the write, the removal hook of the old block, the
    /// placement hook of the new block, the neighbour fan-out under
    /// `UPDATE_NEIGHBORS`, then the shape updates unless `UPDATE_KNOWN_SHAPE`.
    ///
    /// Returns `false` if the state was already `state`.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockStateId, flags: SetBlockFlags) -> bool {
        self.set_block_at(pos, state, flags, SHAPE_UPDATE_LIMIT)
    }

    /// `Level.setBlock(pos, state, flags, limit)`.
    fn set_block_at(
        &mut self,
        pos: BlockPos,
        state: BlockStateId,
        flags: SetBlockFlags,
        limit: u32,
    ) -> bool {
        let old = self.get_block(pos);
        if old == state {
            return false;
        }
        let old_block = world_blocks::block_of_state(old).map(|(b, _)| b);
        let new_block = world_blocks::block_of_state(state).map(|(b, _)| b);
        let moved = flags.contains(SetBlockFlags::UPDATE_MOVE_BY_PISTON);

        // The write happens first, so a removal hook reads the new world — as
        // `LevelChunk.setBlockState` does before `affectNeighborsAfterRemoval`.
        self.view.set_block(pos, state);

        if old_block != new_block && (flags.contains(SetBlockFlags::UPDATE_NEIGHBORS) || moved) {
            behaviour(old).on_remove(self, old, pos);
        }

        if !flags.contains(SetBlockFlags::UPDATE_SKIP_ON_PLACE) {
            behaviour(state).on_place(self, state, pos, old);
        }

        if flags.contains(SetBlockFlags::UPDATE_NEIGHBORS) {
            self.fan_out(pos, pos, old, None, None);
        }

        if !flags.contains(SetBlockFlags::UPDATE_KNOWN_SHAPE) && limit > 0 {
            self.update_shapes(pos, old, state, flags, limit - 1);
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

    /// `Level.updateNeighborsAt`: notify a cell's six neighbours because that
    /// cell itself changed.
    pub fn update_neighbors_at(&mut self, pos: BlockPos) {
        let source = self.get_block(pos);
        self.fan_out(pos, pos, source, None, None);
    }

    /// Notify `center`'s neighbours, reporting `changed` as the source cell —
    /// a wire's seven-cell blast re-centres without moving the source.
    pub fn fan_out(
        &mut self,
        center: BlockPos,
        changed: BlockPos,
        source: BlockStateId,
        skip: Option<Direction>,
        orientation: Option<Orientation>,
    ) {
        self.add_and_run(NeighborUpdate::Multi {
            center,
            changed,
            source,
            skip,
            orientation,
            idx: 0,
        });
    }

    /// `Level.updateNeighborsAtExceptFromFacing`: notify `center`'s neighbours
    /// (skipping `skip`), with `source` at `changed` as the source block.
    pub fn update_neighbors_at_except_from_facing(
        &mut self,
        center: BlockPos,
        changed: BlockPos,
        source: BlockStateId,
        skip: Direction,
        orientation: Option<Orientation>,
    ) {
        self.add_and_run(NeighborUpdate::Multi {
            center,
            changed,
            source,
            skip: Some(skip),
            orientation,
            idx: 0,
        });
    }

    /// `Level.neighborChanged(pos, block)`: notify one cell, with `source` at
    /// `changed` as the source block.
    pub fn neighbor_changed_at(&mut self, pos: BlockPos, changed: BlockPos, source: BlockStateId) {
        self.add_and_run(NeighborUpdate::Simple {
            pos,
            changed,
            source,
            orientation: None,
        });
    }

    /// The seven-cell blast a wire uses when its power changes: the wire and
    /// its six neighbours, each then notifying its own neighbours. The source
    /// cell is always the wire at `pos`.
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
            self.fan_out(cell, pos, source, None, None);
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

    /// Dispatch an "entity inside" event to the block's behaviour.
    pub fn dispatch_entity_inside(&mut self, pos: BlockPos) {
        let state = self.get_block(pos);
        behaviour(state).entity_inside(self, state, pos);
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
                changed,
                source,
                orientation,
            } => {
                let source = NeighborSource {
                    pos: *changed,
                    state: *source,
                    orientation: *orientation,
                };
                self.dispatch_neighbor_changed(*pos, Some(source));
                false
            }
            NeighborUpdate::Multi {
                center,
                changed,
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
                let target = center.offset(dir);
                let source = NeighborSource {
                    pos: *changed,
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
                limit,
            } => {
                self.execute_shape_update(*neighbor, *direction, *flags, *limit);
                false
            }
        }
    }

    /// The three shape-update passes vanilla runs around a write: the old
    /// block's indirect pass, the new block's direct pass, then the new block's
    /// indirect pass (`Level.setBlock`).
    pub(crate) fn update_shapes(
        &mut self,
        pos: BlockPos,
        old: BlockStateId,
        new: BlockStateId,
        flags: SetBlockFlags,
        limit: u32,
    ) {
        let k = flags
            .without(SetBlockFlags::UPDATE_NEIGHBORS)
            .without(SetBlockFlags::UPDATE_SUPPRESS_DROPS);
        behaviour(old).update_indirect_neighbour_shapes(self, old, pos, k, limit);
        self.update_neighbour_shapes(pos, new, k, limit);
        behaviour(new).update_indirect_neighbour_shapes(self, new, pos, k, limit);
    }

    /// `BlockState.updateNeighbourShapes`: ask each neighbour for its
    /// `updateShape`, in `UPDATE_SHAPE_ORDER`.
    pub(crate) fn update_neighbour_shapes(
        &mut self,
        pos: BlockPos,
        _state: BlockStateId,
        flags: SetBlockFlags,
        limit: u32,
    ) {
        for dir in Direction::UPDATE_SHAPE_ORDER {
            self.add_and_run(NeighborUpdate::Shape {
                direction: dir.opposite(),
                neighbor: pos.offset(dir),
                flags,
                limit,
            });
        }
    }

    /// Run a single shape update on `neighbor` directly — used by a block's
    /// `updateIndirectNeighbourShapes` to reach a non-adjacent cell.
    pub(crate) fn shape_update_at(
        &mut self,
        neighbor: BlockPos,
        direction: Direction,
        flags: SetBlockFlags,
        limit: u32,
    ) {
        self.execute_shape_update(neighbor, direction, flags, limit);
    }

    /// `NeighborUpdater.executeShapeUpdate`.
    fn execute_shape_update(
        &mut self,
        neighbor: BlockPos,
        direction: Direction,
        flags: SetBlockFlags,
        limit: u32,
    ) {
        let state = self.get_block(neighbor);
        // `(i & 128) == 0 || !blockState2.is(REDSTONE_WIRE)`.
        if flags.contains(SetBlockFlags::UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE)
            && behaviour(state).is_wire()
        {
            return;
        }
        if let Some(new) = behaviour(state).update_shape(self, state, neighbor, direction) {
            if new != state {
                if new == AIR {
                    // `Block.updateOrDestroy`: an air result destroys the block
                    // (dropping its item unless `UPDATE_SUPPRESS_DROPS`), then
                    // writes air with `setBlock(..., 3)` so the removed block's
                    // hook and the neighbour fan-out run — a wire that pops off
                    // also wakes its corners.
                    if !flags.contains(SetBlockFlags::UPDATE_SUPPRESS_DROPS) {
                        self.view.drop_block(neighbor, state);
                    }
                    self.set_block_at(neighbor, AIR, SetBlockFlags::PLACE, limit);
                } else {
                    // The non-air write clears `UPDATE_NEIGHBORS` and
                    // `UPDATE_SUPPRESS_DROPS`, and recurses with this limit.
                    let nested = flags
                        .without(SetBlockFlags::UPDATE_NEIGHBORS)
                        .without(SetBlockFlags::UPDATE_SUPPRESS_DROPS);
                    self.set_block_at(neighbor, new, nested, limit);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The compiled behaviour table: one slot per block state.
///
/// Built once, then mutated in place by [`register`] under the write lock, so a
/// late registration is race-free and costs no rebuild and no extra allocation.
struct Registry {
    by_state: Box<[&'static dyn BlockBehaviour]>,
}

/// The registry, initialised on first use.
static REGISTRY: std::sync::OnceLock<std::sync::RwLock<Registry>> = std::sync::OnceLock::new();

fn registry() -> &'static std::sync::RwLock<Registry> {
    REGISTRY.get_or_init(|| std::sync::RwLock::new(Registry::build()))
}

/// Register `behaviour` for the block named `name` (e.g. `minecraft:observer`).
///
/// This is the extension point: an external crate adds a component by writing a
/// [`BlockBehaviour`] and calling this, with **no edit to this file**. A later
/// registration wins over an earlier one and over the built-ins; it is applied
/// under the write lock, so it is never lost to a concurrent lookup.
pub fn register(name: &'static str, behaviour: &'static dyn BlockBehaviour) {
    registry().write().unwrap().set(name, behaviour);
}

/// The behaviour of a block state. One array index; no string comparison.
pub fn behaviour(state: BlockStateId) -> &'static dyn BlockBehaviour {
    registry().read().unwrap().get(state)
}

impl Registry {
    fn build() -> Self {
        let mut by_state: Box<[&'static dyn BlockBehaviour]> =
            vec![&blocks::DEFAULT as &'static dyn BlockBehaviour; blocks::STATE_COUNT]
                .into_boxed_slice();

        put(&mut by_state, "minecraft:redstone_wire", &blocks::Wire);
        put(&mut by_state, "minecraft:repeater", &blocks::Repeater);
        put(&mut by_state, "minecraft:comparator", &blocks::Comparator);
        put(&mut by_state, "minecraft:observer", &blocks::Observer);
        put(
            &mut by_state,
            "minecraft:daylight_detector",
            &blocks::DaylightDetector,
        );
        put(&mut by_state, "minecraft:redstone_torch", &blocks::Torch);
        put(
            &mut by_state,
            "minecraft:redstone_wall_torch",
            &blocks::WallTorch,
        );
        put(&mut by_state, "minecraft:lever", &blocks::Lever);
        put(
            &mut by_state,
            "minecraft:redstone_block",
            &blocks::RedstoneBlock,
        );
        put(&mut by_state, "minecraft:redstone_lamp", &blocks::Lamp);
        put(
            &mut by_state,
            "minecraft:stone_button",
            &blocks::StoneButton,
        );
        put(
            &mut by_state,
            "minecraft:polished_blackstone_button",
            &blocks::StoneButton,
        );
        put(&mut by_state, "minecraft:oak_button", &blocks::WoodButton);
        put(
            &mut by_state,
            "minecraft:spruce_button",
            &blocks::WoodButton,
        );
        put(&mut by_state, "minecraft:birch_button", &blocks::WoodButton);
        put(
            &mut by_state,
            "minecraft:jungle_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:acacia_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:dark_oak_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:mangrove_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:cherry_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:bamboo_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:crimson_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:warped_button",
            &blocks::WoodButton,
        );
        put(
            &mut by_state,
            "minecraft:pale_oak_button",
            &blocks::WoodButton,
        );
        for name in [
            "minecraft:oak_pressure_plate",
            "minecraft:spruce_pressure_plate",
            "minecraft:birch_pressure_plate",
            "minecraft:jungle_pressure_plate",
            "minecraft:acacia_pressure_plate",
            "minecraft:dark_oak_pressure_plate",
            "minecraft:mangrove_pressure_plate",
            "minecraft:cherry_pressure_plate",
            "minecraft:bamboo_pressure_plate",
            "minecraft:crimson_pressure_plate",
            "minecraft:warped_pressure_plate",
            "minecraft:pale_oak_pressure_plate",
            "minecraft:stone_pressure_plate",
            "minecraft:polished_blackstone_pressure_plate",
        ] {
            put(&mut by_state, name, &blocks::PressurePlate);
        }
        put(
            &mut by_state,
            "minecraft:light_weighted_pressure_plate",
            &blocks::LightWeighted,
        );
        put(
            &mut by_state,
            "minecraft:heavy_weighted_pressure_plate",
            &blocks::HeavyWeighted,
        );
        put(&mut by_state, "minecraft:tripwire", &blocks::TripWire);
        put(
            &mut by_state,
            "minecraft:tripwire_hook",
            &blocks::TripWireHook,
        );

        // A test-only block registered over the piston's states, proving that a
        // new component decides its own wire connection without a core edit.
        #[cfg(test)]
        put(&mut by_state, "minecraft:piston", &blocks::TestObserver);
        // A test-only recording block for the notification-order findings.
        #[cfg(test)]
        put(
            &mut by_state,
            "minecraft:note_block",
            &blocks::test_spy::Spy,
        );
        // A test-only block that never settles, for the shape-update budget.
        #[cfg(test)]
        put(
            &mut by_state,
            "minecraft:jukebox",
            &blocks::test_chain::Chain,
        );

        Registry { by_state }
    }

    fn get(&self, state: BlockStateId) -> &'static dyn BlockBehaviour {
        self.by_state
            .get(state.raw() as usize)
            .copied()
            .unwrap_or(&blocks::DEFAULT)
    }

    fn set(&mut self, name: &str, behaviour: &'static dyn BlockBehaviour) {
        put(&mut self.by_state, name, behaviour);
    }
}

fn put(by_state: &mut [&'static dyn BlockBehaviour], name: &str, b: &'static dyn BlockBehaviour) {
    if let Some(range) = world_blocks::states_of(name) {
        for state in range {
            by_state[state as usize] = b;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::test_spy;
    use crate::{MemWorld, Redstone};

    fn p(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn removal_notification_runs_after_the_write_and_carries_the_old_state() {
        // N3: `LevelChunk` writes first, then `affectNeighborsAfterRemoval`, and
        // `Level.setBlock`'s `UPDATE_NEIGHBORS` pass (which supplies the source)
        // uses the OLD block. The default removal hook does not fan out itself.
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let spy = p(0, 0, 0);
        let stone = p(1, 0, 0);
        w.put(spy, "minecraft:note_block");
        w.put(stone, "minecraft:stone");
        engine.on_placed(&mut w, spy);
        test_spy::take();

        let stone_state = w.state(stone);
        engine.set_block(&mut w, stone, ids::AIR, SetBlockFlags::PLACE);

        let log = test_spy::take();
        let n = log
            .iter()
            .find(|n| n.pos == spy)
            .expect("the spy must be notified of the removal");
        assert_eq!(n.from, stone_state, "the source is the old block");
        assert_eq!(
            n.source_now,
            ids::AIR,
            "the write must have happened before the notification"
        );
    }

    #[test]
    fn except_facing_update_carries_the_diode_as_source() {
        // N6: `DiodeBlock.updateNeighborsInFront` passes `this` (the repeater)
        // as the source to `updateNeighborsAtExceptFromFacing`, not the block
        // that happens to sit at the notified position.
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let repeater = p(1, 0, 0);
        let output = p(2, 0, 0);
        let spy = p(3, 0, 0);
        w.put(repeater, "minecraft:repeater[facing=west,delay=1]");
        w.put(output, "minecraft:stone");
        w.put(spy, "minecraft:note_block");
        let repeater_state = w.state(repeater);
        test_spy::take();

        engine.on_placed(&mut w, repeater);

        let log = test_spy::take();
        let to_spy: Vec<_> = log.iter().filter(|n| n.pos == spy).collect();
        assert!(
            !to_spy.is_empty(),
            "the zone in front of the repeater must be notified"
        );
        for n in to_spy {
            assert_eq!(
                n.from_pos, repeater,
                "the source cell must be the repeater, not the output cell"
            );
            assert_eq!(
                n.from, repeater_state,
                "the source must be the repeater, not the stone at the output"
            );
        }
    }

    #[test]
    fn a_wire_blast_reports_the_wire_as_source() {
        // D2: `notify_around` re-centres on each satellite but the source block
        // is the wire that changed, so `NeighborSource::pos` is the wire.
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let lever = p(0, 0, 0);
        let wire = p(1, 0, 0);
        let spy = p(2, 0, 0);
        for x in 0..=2 {
            w.put(p(x, -1, 0), "minecraft:stone");
        }
        w.put(lever, "minecraft:lever[face=floor,facing=east]");
        w.put(wire, "minecraft:redstone_wire");
        w.put(spy, "minecraft:note_block");
        for x in 0..=2 {
            engine.on_placed(&mut w, p(x, -1, 0));
        }
        engine.on_placed(&mut w, lever);
        engine.on_placed(&mut w, wire);
        test_spy::take();

        engine.on_use(&mut w, lever);

        let log = test_spy::take();
        let to_spy: Vec<_> = log.iter().filter(|n| n.pos == spy).collect();
        assert!(!to_spy.is_empty(), "the wire must notify its neighbour");
        for n in to_spy {
            assert_eq!(
                n.from_pos, wire,
                "the source cell must be the wire, not the notified satellite"
            );
        }
    }

    #[test]
    fn a_bare_block_removal_notifies_each_neighbour_once() {
        // N3: with the default removal hook empty, the only fan-out for a plain
        // block is the `UPDATE_NEIGHBORS` pass — exactly once per neighbour.
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let spy = p(0, 0, 0);
        let block = p(1, 0, 0);
        w.put(spy, "minecraft:note_block");
        w.put(block, "minecraft:redstone_block");
        engine.on_placed(&mut w, spy);
        test_spy::take();

        engine.set_block(&mut w, block, ids::AIR, SetBlockFlags::PLACE);
        let count = test_spy::take().iter().filter(|n| n.pos == spy).count();
        assert_eq!(count, 1, "the neighbour must be notified exactly once");
    }

    #[test]
    fn the_shape_update_budget_matches_vanilla() {
        // D4: `Level.setBlock` runs the shape passes at `j - 1` with the default
        // j = 512, so a placement's first nested shape pass carries 511 and the
        // cascade reaches 512 cells before the budget hits 0. A line of
        // never-settling chain blocks measures the recursion budget exactly; the
        // pre-fix code started at 512 and reached 513.
        use crate::blocks::test_chain;
        let engine = Redstone::new();
        let mut w = MemWorld::new();
        let seed = p(0, 0, 0);
        w.put(seed, "minecraft:stone");
        for x in 1..=600 {
            w.put(p(x, 0, 0), "minecraft:jukebox");
        }
        test_chain::reset();

        engine.on_placed(&mut w, seed);

        assert_eq!(
            test_chain::calls(),
            512,
            "a placement's shape cascade must run the j-1 = 511 budget"
        );
    }
}
