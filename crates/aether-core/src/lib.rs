//! # aether-core
//!
//! Foundational primitives shared across the Aether Engine crates:
//!
//! * [`simd`] — a runtime SIMD **dispatch** scaffold. Every hot bit-mask
//!   operation has a portable scalar fallback and an accelerated path
//!   (SSE4.2 / AVX2 / AVX-512) selected once, at start-up, from the CPU's
//!   reported features.
//! * [`morton`] — Z-order (Morton) encode/decode for the 16×16×16 sub-chunk
//!   used by the memory model.
//!
//! The crate is `no_std`-friendly in spirit (only `alloc`/`std` slices are
//! touched) and has **zero third-party dependencies** so it stays trivially
//! buildable on any target during Phase 0.

pub mod morton;
pub mod simd;

pub use morton::{morton_decode_16, morton_encode_16};
pub use simd::{dispatch, Backend, MaskOps};
