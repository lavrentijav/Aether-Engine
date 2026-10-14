//! The per-block behaviour trait and the implementations that are in scope.
//!
//! Each submodule is one redstone component family. A component is added by
//! writing a module here and registering it in [`crate::behaviour`]; the engine
//! and the dispatch never change.

mod button;
mod comparator;
mod default;
pub(crate) mod diode;
mod lamp;
mod lever;
mod observer;
mod redstone_block;
mod repeater;
mod torch;
mod wire;

#[cfg(test)]
pub mod test_chain;
#[cfg(test)]
mod test_observer;
#[cfg(test)]
pub mod test_spy;

pub use button::{StoneButton, WoodButton};
pub use comparator::Comparator;
pub use default::DefaultBehaviour;
pub use lamp::Lamp;
pub use lever::Lever;
pub use observer::Observer;
pub use redstone_block::RedstoneBlock;
pub use repeater::Repeater;
#[cfg(test)]
pub use test_observer::TestObserver;
pub use torch::{Torch, WallTorch};
pub use wire::Wire;

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource, SignalCtx};
use crate::dir::Direction;

/// The number of block states the registry covers.
pub const STATE_COUNT: usize = aether_world::registry::blocks::STATE_COUNT;

/// The behaviour used for every state that has no redstone behaviour — air,
/// stone, and everything else. Its defaults are all "does nothing".
pub static DEFAULT: DefaultBehaviour = DefaultBehaviour;

/// One block's redstone behaviour, dispatched from a state-indexed table.
///
/// The default implementations make a block inert: no signal, no reaction to
/// neighbours, no tick. A component overrides only what it needs.
///
/// Kept object-safe on purpose so the registry can be a `&'static dyn`.
pub trait BlockBehaviour: Sync {
    /// `isSignalSource`. Wire, repeaters, torches, levers, buttons and the
    /// block of redstone are sources; whether they emit anything is decided by
    /// [`BlockBehaviour::get_signal`].
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        false
    }

    /// Whether this is the redstone-wire family. Used by connection logic.
    fn is_wire(&self) -> bool {
        false
    }

    /// Whether this is the diode family (repeater and comparator).
    fn is_diode(&self) -> bool {
        false
    }

    /// `RedStoneWireBlock.shouldConnectTo`: whether redstone wire draws a
    /// connection toward this state.
    ///
    /// `dir` is the direction from the wire to this block, or `None` for
    /// vanilla's direction-less query (used when checking the cell above/below
    /// a wire). A block that needs a bespoke rule — the observer, a piston —
    /// overrides this in its own module; the wire core never changes. The
    /// default is vanilla's fallback: an ordinary signal source connects on any
    /// horizontal side, and nothing connects for the `None` query.
    fn connects_to_wire(&self, state: BlockStateId, dir: Option<Direction>) -> bool {
        self.is_signal_source(state) && dir.is_some()
    }

    /// Whether this is the block of redstone.
    fn is_redstone_block(&self) -> bool {
        false
    }

    /// `isRedstoneConductor`: whether the state passes strong power on as weak.
    /// Defaults to the generic per-state full-collision test; the block of
    /// redstone overrides it (vanilla makes it a non-conductor).
    fn is_conductor(&self, state: BlockStateId) -> bool {
        crate::shapes::full_collision(state)
    }

    /// `getSignal(state, pos, dir)` — the block's own weak emission, before any
    /// conductor pass-through.
    fn get_signal(
        &self,
        _ctx: &SignalCtx,
        _state: BlockStateId,
        _pos: crate::pos::BlockPos,
        _dir: Direction,
    ) -> u8 {
        0
    }

    /// `getDirectSignal(state, pos, dir)` — the block's strong emission to `dir`.
    fn get_direct_signal(
        &self,
        _ctx: &SignalCtx,
        _state: BlockStateId,
        _pos: crate::pos::BlockPos,
        _dir: Direction,
    ) -> u8 {
        0
    }

    /// `neighborChanged`: a neighbour changed, `from` says which.
    fn neighbor_changed(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: crate::pos::BlockPos,
        _from: Option<NeighborSource>,
    ) {
    }

    /// `onPlace`: the block was written. `old` is the previous state.
    fn on_place(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: crate::pos::BlockPos,
        _old: BlockStateId,
    ) {
    }

    /// `setPlacedBy`: a player placed the block. Runs after [`Self::on_place`].
    /// A diode schedules a one-tick tick here when it is placed onto a powered
    /// input, regardless of its delay.
    fn set_placed_by(&self, _ctx: &mut Context, _state: BlockStateId, _pos: crate::pos::BlockPos) {}

    /// `affectNeighborsAfterRemoval`: the block is being replaced.
    ///
    /// The default does nothing, as vanilla's `BlockBehaviour` does; the sole
    /// removal fan-out is the `UPDATE_NEIGHBORS` pass in `Level.setBlock`. A
    /// component overrides this only when it needs its own removal notification
    /// (a wire's corner update, a torch's neighbour notify).
    fn on_remove(&self, _ctx: &mut Context, _state: BlockStateId, _pos: crate::pos::BlockPos) {}

    /// `tick`: a scheduled block tick came due.
    fn tick(&self, _ctx: &mut Context, _state: BlockStateId, _pos: crate::pos::BlockPos) {}

    /// `updateShape`: a neighbour's shape changed; return the new state, or
    /// `None` to leave this block alone.
    fn update_shape(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: crate::pos::BlockPos,
        _dir: Direction,
    ) -> Option<BlockStateId> {
        None
    }

    /// `updateIndirectNeighbourShapes`: reach cells that a direct shape update
    /// cannot. Only redstone wire overrides it (to wake the diagonal wires of an
    /// up-and-over); the default does nothing.
    fn update_indirect_neighbour_shapes(
        &self,
        _ctx: &mut Context,
        _state: BlockStateId,
        _pos: crate::pos::BlockPos,
        _flags: crate::view::SetBlockFlags,
        _limit: u32,
    ) {
    }

    /// `useWithoutItem`: a player interacted.
    fn use_block(&self, _ctx: &mut Context, _state: BlockStateId, _pos: crate::pos::BlockPos) {}
}

/// `LeverBlock`/`ButtonBlock.updateNeighbours`: notify the block itself and the
/// block it is attached to.
pub(crate) fn update_powered_source_neighbours(
    ctx: &mut Context,
    pos: crate::pos::BlockPos,
    state: BlockStateId,
) {
    let connected = crate::props::connected_direction(state);
    ctx.update_neighbors_at(pos);
    ctx.update_neighbors_at(pos.offset(connected.opposite()));
}

/// `FaceAttachedHorizontalDirectionalBlock.updateShape`: a lever or button pops
/// off when the face it is attached to loses its support.
pub(crate) fn face_attached_support(
    ctx: &Context,
    state: BlockStateId,
    pos: crate::pos::BlockPos,
    dir: Direction,
) -> Option<BlockStateId> {
    let connected = crate::props::connected_direction(state);
    if dir == connected.opposite() {
        let support = ctx.get_block(pos.offset(connected.opposite()));
        if !crate::shapes::face_full(support, connected) {
            return Some(crate::behaviour::AIR);
        }
    }
    None
}
