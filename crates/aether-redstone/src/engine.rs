//! The redstone engine, and the world interface it drives.
//!
//! One [`Redstone`] value drives any [`RedstoneView`]. It owns no world state,
//! so every method is a pure function of the view it is handed: call it twice
//! on the same view and you get the same result. The transient update queue a
//! call needs lives on the stack for the duration of that call.
//!
//! This module holds the update plumbing — the neighbour-notification cascade
//! vanilla calls `CollectingNeighborUpdater` — and the five entry points the
//! host uses. The per-block behaviour lives in the sibling modules.

use std::collections::VecDeque;

use aether_world::BlockStateId;

use crate::pos::BlockPos;
use crate::view::RedstoneView;

/// A hard cap on the update cascade, so a malformed circuit cannot spin a call
/// forever. A real cascade is a few hundred updates; this is far above that.
const MAX_UPDATES: usize = 1_000_000;

/// A scheduled re-evaluation of one cell.
#[derive(Debug, Clone, Copy)]
struct Update(BlockPos);

/// The redstone engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct Redstone;

impl Redstone {
    /// A new engine. It holds nothing; this is a constructor for symmetry.
    pub const fn new() -> Self {
        Redstone
    }

    // -----------------------------------------------------------------------
    // Update plumbing
    // -----------------------------------------------------------------------

    /// `Level.updateNeighborsAt`: notify a cell's six neighbours.
    fn update_neighbors_at(&self, queue: &mut VecDeque<Update>, pos: BlockPos) {
        for dir in crate::dir::Direction::UPDATE_ORDER {
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

    fn dispatch<V: RedstoneView>(
        &self,
        _view: &mut V,
        _queue: &mut VecDeque<Update>,
        _pos: BlockPos,
    ) {
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
        let _ = old;
        let mut queue = VecDeque::new();
        self.update_neighbors_at(&mut queue, pos);
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
        let queue = VecDeque::new();
        let _ = pos;
        self.drain(view, queue);
    }

    /// A player used the block at `pos`: a lever pulls, a button presses, a
    /// repeater cycles its delay, a wire toggles cross/dot.
    pub fn on_use<V: RedstoneView>(&self, view: &mut V, pos: BlockPos) {
        let queue = VecDeque::new();
        let _ = pos;
        self.drain(view, queue);
    }
}
