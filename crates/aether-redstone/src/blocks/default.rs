//! The default, inert behaviour: everything that is not a redstone component.

/// A block with no redstone behaviour.
pub struct DefaultBehaviour;

impl crate::blocks::BlockBehaviour for DefaultBehaviour {}
