//! A **test-only** recording block, registered over the note-block states.
//!
//! Behaviour is invisible to a test unless something records it. This spy
//! captures every `neighbor_changed` notification: which cell was notified, the
//! `from` source state, and the state the source cell holds at notification
//! time. That is what pins findings N3 (removal notifications come after the
//! write and carry the old state) and N6 (the except-facing fan-out carries the
//! diode itself, not the notified cell).

use std::cell::RefCell;

use aether_world::BlockStateId;

use crate::behaviour::{Context, NeighborSource};
use crate::blocks::BlockBehaviour;
use crate::pos::BlockPos;

/// One recorded notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notified {
    /// The cell that was notified.
    pub pos: BlockPos,
    /// The source block's cell, as passed to `neighbor_changed`.
    pub from_pos: BlockPos,
    /// The source block's state, as passed to `neighbor_changed`.
    pub from: BlockStateId,
    /// The state at the source position at notification time.
    pub source_now: BlockStateId,
}

thread_local! {
    static LOG: RefCell<Vec<Notified>> = const { RefCell::new(Vec::new()) };
}

/// A recording block.
pub struct Spy;

impl BlockBehaviour for Spy {
    fn neighbor_changed(
        &self,
        ctx: &mut Context,
        _state: BlockStateId,
        pos: BlockPos,
        from: Option<NeighborSource>,
    ) {
        if let Some(from) = from {
            let source_now = ctx.get_block(from.pos);
            LOG.with(|log| {
                log.borrow_mut().push(Notified {
                    pos,
                    from_pos: from.pos,
                    from: from.state,
                    source_now,
                });
            });
        }
    }
}

/// Take everything recorded on this thread since the last call.
pub fn take() -> Vec<Notified> {
    LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}
