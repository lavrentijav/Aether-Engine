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
//! in-memory [`MemWorld`] the tests use. [`Redstone`] itself holds nothing:
//! world-owned state the engine needs (scheduled ticks, the torch burnout log,
//! item drops) lives on the view. Dropping an item for a block the engine
//! destroys is **delegated**: the crate has no item model, so it calls
//! [`RedstoneView::drop_block`] and the host runs the loot table.
//!
//! ```
//! use aether_redstone::{BlockPos, MemWorld, Redstone};
//!
//! let engine = Redstone::new();
//! let mut world = MemWorld::new();
//! world.put(BlockPos::new(0, 0, 0), "minecraft:lever[face=floor,facing=east]");
//! world.put(BlockPos::new(1, 0, 0), "minecraft:redstone_wire");
//! engine.on_placed(&mut world, BlockPos::new(0, 0, 0));
//! engine.on_placed(&mut world, BlockPos::new(1, 0, 0));
//! engine.on_use(&mut world, BlockPos::new(0, 0, 0));
//! assert_eq!(
//!     aether_redstone::props::get(world.state(BlockPos::new(1, 0, 0)), "power"),
//!     Some("15")
//! );
//! ```
//!
//! Block states are [`aether_world::BlockStateId`], and every property is read
//! and written through `aether-world`'s existing state schema (see [`props`]) —
//! there is no second property system.
//!
//! ## Extending it
//!
//! A redstone component is a type implementing [`blocks::BlockBehaviour`],
//! registered against a block name with [`behaviour::register`]. The registry
//! compiles that into a `BlockStateId`-indexed table, so the hot path is an
//! array index with no string comparison, and the engine never matches on a
//! block name. The registration API is **public**: an external crate adds a
//! component — comparator, observer, piston — with no edit to this crate, and a
//! block decides its own redstone-wire connection through
//! [`blocks::BlockBehaviour::connects_to_wire`].
//!
//! ## Power model
//!
//! Two channels, exactly as vanilla:
//!
//! * **weak** power — [`Redstone::get_signal`]. Read by wire, repeaters,
//!   torches, lamps and mechanisms.
//! * **strong** power — [`Redstone::get_direct_signal`]. What a lever, button,
//!   torch or repeater pushes into the block it is attached to. A redstone
//!   conductor turns the strong power it holds into weak power for its other
//!   neighbours, which is how a lever on a block powers wire on the far side.
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
//! generated in vanilla's `UPDATE_ORDER` (west, east, down, up, north, south)
//! and run by a port of vanilla's depth-first `CollectingNeighborUpdater`: a
//! LIFO stack where a multi-neighbour update yields to freshly generated
//! updates before resuming, with the chain limit. State changes go through
//! `set_block(pos, state, flags)` with vanilla's `Block.UpdateFlags`, so the
//! placement/removal hooks and the recursive shape updates (including
//! `updateIndirectNeighbourShapes`) run where vanilla runs them.
//! See [`Redstone`] and `docs/KNOWN_ISSUES.md` for the full list of differences.
//!
//! ## Components
//!
//! Wire, repeater, comparator, observer, redstone torch and wall torch, lever,
//! button, block of redstone, lamp, daylight detector, pressure plates
//! (wood/stone and weighted), tripwire and hook, and the target block.
//!
//! ## Not in this stage
//!
//! Pistons (and quasi-connectivity), hoppers, droppers and dispensers, doors and
//! trapdoors reacting to power, rails, note block, TNT priming, the
//! dependency-graph compiler, and wiring the engine into the server tick loop.
//! Each is listed here so a later stage extends this crate instead of rewriting
//! it.

#![forbid(unsafe_code)]

pub mod behaviour;
pub mod blocks;
pub mod dir;
pub mod engine;
pub mod pos;
pub mod props;
pub mod shapes;
pub mod view;

pub use dir::Direction;
pub use engine::Redstone;
pub use pos::BlockPos;
pub use view::{MemWorld, Orientation, RedstoneView, SetBlockFlags, TickPriority, TraceWorld};
