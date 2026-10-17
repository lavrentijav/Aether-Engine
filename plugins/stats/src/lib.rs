//! The Aether **stats** plugin (CMD-47).
//!
//! `/stats`, `/leaderboard` (`/lb`) and `/playtime` over the journal history
//! API (PA2). The journal is the single source of truth for what players did;
//! the plugin folds the events it cares about into a small per-player cache on
//! a schedule (P1) and answers every command from that cache, so a request
//! never walks the journal and an offline player is answered from the stored
//! aggregate just the same.
//!
//! # Shape
//!
//! * [`model`] — the pure data: identities, journal classification, the cached
//!   aggregates, leaderboard ordering and the playtime ledger. No host, no
//!   clock: the caller passes world ticks in and KV bytes out, which is what
//!   makes the awkward cases testable.
//! * [`engine`] — the model wired to a history source, a KV store and a
//!   permission checker; this is where the commands are answered.
//! * `component` (WASM only) — the `wasm32-wasip2` guest that adapts the SDK
//!   services to the engine's traits. Built with `make`; see the README.
//!
//! # What the engine cannot derive
//!
//! The journal records block edits and item moves, not movement, damage or
//! death, so **distance, deaths and kills are always reported as `0`**. They
//! are kept in the model so a future task that produces them can fill them in
//! without changing the command surface. Playtime is tracked by the plugin
//! itself from join/leave events, in world ticks, and checkpointed to KV.
//!
//! # Not a hot path
//!
//! `refresh` runs from the SDK scheduler; `on_join`/`on_leave` are the only
//! other writes. Commands are read-only against the cache.

pub mod engine;
pub mod model;

#[cfg(target_arch = "wasm32")]
mod component;

#[cfg(test)]
mod tests;
