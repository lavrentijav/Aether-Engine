//! # aether-world
//!
//! The Phase 1 world model and persistence layer.
//!
//! ## Memory model (Structure-of-Arrays)
//!
//! ```text
//! World → Region → Chunk (16×16) → Sub-Chunk (16×16×16) → AVX-Cell (4×4×2)
//! ```
//!
//! * [`AvxCell`] — 32 blocks × `u16` = 64 bytes = one cache line.
//! * [`SubChunk`] — 16³ blocks stored as a palette-compressed id array plus
//!   SoA bit [`Mask`]s (`solid`, `collision`, `redstone`), all Morton-indexed.
//! * [`Palette`] — `u4 → u8 → u16` auto-expanding index compression.
//!
//! ## Storage
//!
//! [`storage`] persists sub-chunks as Zstandard-compressed blobs in a KV store
//! ([`MemStore`] in-memory, or the persistent Fjall backend behind the `fjall`
//! feature) keyed by [`SubChunkKey`].

pub mod block;
pub mod block_entity;
pub mod cell;
pub mod light;
pub mod palette;
pub mod registry;
pub mod storage;
pub mod subchunk;

pub use block::{BlockProperties, BlockStateId};
pub use block_entity::{BlockEntity, BlockEntityArena};
pub use cell::AvxCell;
pub use light::{compute_light, FullBright, LightGrid, LightMedium, LightView, MAX_LIGHT};
pub use palette::{PackedArray, Palette};
pub use registry::BlockRegistry;
pub use storage::format::{FormatError, SubChunkKey};
pub use storage::{KvBackend, MemStore, StorageError, WorldStorage};
pub use subchunk::{Mask, SubChunk, DIM, MASK_WORDS, VOLUME};

#[cfg(feature = "fjall")]
pub use storage::FjallStore;
