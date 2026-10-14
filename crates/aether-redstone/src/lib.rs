//! # aether-redstone
//!
//! A standalone model of **Minecraft 1.21.11 redstone**, with no storage and no
//! connection to the game loop. It is the foundation a later stage wires into
//! `aether-server`; this stage exists so that foundation is correct and tested
//! before anything depends on it.
//!
//! ## Shape
//!
//! The engine reads and writes the world only through [`RedstoneView`], so the
//! same code runs against the real engine world, a generated chunk, or the
//! in-memory [`MemWorld`] the tests use. [`Redstone`] itself holds nothing.
//!
//! Block states are [`aether_world::BlockStateId`], and every property is read
//! and written through `aether-world`'s existing state schema (see [`props`]) —
//! there is no second property system.
//!
//! ## Power model
//!
//! Two channels, exactly as vanilla:
//!
//! * **weak** power — [`Redstone::get_signal`]. Read by wire, repeaters,
//!   torches, lamps and mechanisms.
//! * **strong** power — [`Redstone::get_direct_signal`]. What a lever, button,
//!   torch or repeater pushes into the block it is attached to. A redstone
//!   conductor ([`RedstoneView::is_conductor`]) turns the strong power it holds
//!   into weak power for its other neighbours, which is how a lever on a block
//!   powers wire on the far side.
//!
//! Redstone wire deliberately reads its neighbours with every *other* wire
//! suppressed (vanilla's `shouldSignal = false`): a block powered by wire is
//! only weakly powered and cannot power further wire. Repeaters and comparators,
//! which read strong power directly, still see it.
//!
//! ## Update order
//!
//! This implements the **1.21.11 default** evaluator — the experimental
//! "Eigencraft" wire evaluator is not used. Neighbour notifications are
//! generated in vanilla's `UPDATE_ORDER` (west, east, down, up, north, south),
//! but the engine drains its update queue **breadth-first** rather than
//! depth-first. Every rule is a monotone fixed point over power, so the steady
//! state is identical and the result is deterministic; only a race between two
//! updates scheduled at the same priority within one tick could differ. See
//! [`Redstone`] and `docs/tasks/QR-notes.md` for the full list of differences.
//!
//! ## Not in this stage
//!
//! Comparators, observers, pistons, hoppers, droppers and dispensers, doors and
//! trapdoors reacting to power, rails, quasi-connectivity, the dependency-graph
//! compiler, and wiring the engine into the server tick loop. Each is listed
//! here so a later stage extends this crate instead of rewriting it.

#![forbid(unsafe_code)]

pub mod dir;
pub mod engine;
pub mod pos;
pub mod props;
pub mod view;

pub use dir::Direction;
pub use engine::Redstone;
pub use pos::BlockPos;
pub use view::{MemWorld, RedstoneView, TickPriority, TraceWorld};
