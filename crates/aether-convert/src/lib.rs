//! # aether-convert
//!
//! Migrates a legacy **Anvil** world (`region/*.mca`) into the Aether Engine's
//! KV world store.
//!
//! Pipeline:
//! 1. [`anvil`] reads region files and decompresses each chunk's NBT.
//! 2. [`nbt`] parses the chunk tree.
//! 3. [`block_map`] maps Anvil block names to dense engine ids + properties.
//! 4. [`convert`] rebuilds engine [`aether_world::SubChunk`]s and writes them to
//!    a [`aether_world::WorldStorage`], in parallel across region files, with an
//!    audit [`convert::ConversionReport`].
//!
//! The crate is usable as a library (drive [`convert::convert_world`] yourself)
//! or through the bundled `aether-convert` CLI.

pub mod anvil;
pub mod block_map;
pub mod convert;
pub mod nbt;

pub use block_map::{BlockRegistry, Interner};
pub use convert::{convert_world, ConversionReport};
