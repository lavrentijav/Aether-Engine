//! The comparator — a **placeholder** for the later comparator stage.
//!
//! No comparison behaviour is implemented yet. The one thing this registers is
//! `isSignalSource`, because `RedStoneWireBlock.shouldConnectTo` special-cases
//! only the *repeater*: a comparator is an ordinary signal source, so redstone
//! wire connects to it on every horizontal side. Getting that wrong stored a
//! wire shape vanilla never produces (review finding 3).

use aether_world::BlockStateId;

use crate::blocks::BlockBehaviour;

/// The redstone comparator.
pub struct Comparator;

impl BlockBehaviour for Comparator {
    fn is_signal_source(&self, _state: BlockStateId) -> bool {
        true
    }

    fn is_diode(&self) -> bool {
        true
    }
}
