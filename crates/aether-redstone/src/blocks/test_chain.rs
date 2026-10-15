//! A **test-only** block whose `updateShape` never settles, so a shape-update
//! chain runs until the recursion limit stops it. Used to pin the shape-update
//! budget (QR-review-3 finding D4).
//!
//! It only reacts to a `West` shape update, so a line of them propagates in one
//! direction with no back-branching and the cascade length is exactly the
//! recursion budget.

use std::cell::Cell;

use aether_world::BlockStateId;

use crate::blocks::BlockBehaviour;
use crate::dir::Direction;
use crate::pos::BlockPos;
use crate::props;

/// A block that flips `has_record` on every westward shape update, so it never
/// reaches a fixed point.
pub struct Chain;

thread_local! {
    static CALLS: Cell<u32> = const { Cell::new(0) };
}

impl BlockBehaviour for Chain {
    fn update_shape(
        &self,
        _ctx: &mut crate::behaviour::Context,
        state: BlockStateId,
        _pos: BlockPos,
        dir: Direction,
    ) -> Option<BlockStateId> {
        if dir != Direction::West {
            return None;
        }
        CALLS.with(|c| c.set(c.get() + 1));
        let current = props::bool_or(state, "has_record", false);
        let next = if current { "false" } else { "true" };
        props::with(state, "has_record", next)
    }
}

/// How many shape updates the chain block has received on this thread.
pub fn calls() -> u32 {
    CALLS.with(|c| c.get())
}

/// Reset the counter.
pub fn reset() {
    CALLS.with(|c| c.set(0));
}
