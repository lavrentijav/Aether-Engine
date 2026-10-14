//! The version compatibility layer.
//!
//! The engine's world is 1.21.11: [`BlockStateId`]s are that release's
//! flattened state ids and item ids are its item ids. A client on another
//! release has its own numbering, and — for anything added since — no block or
//! item with that name at all. This layer answers the one question every codec
//! asks: *what does this engine object look like on that client?*
//!
//! It is data-driven and shared by every codec, generated ahead of time so the
//! wire path stays a single array read. A substitute is a disguise: it changes
//! only what the client sees, never the server's world.
//!
//! # Layout
//!
//! * [`blocks`] — block translation and substitution (J1, J2). A "block item"
//!   follows its block's substitution, so the item layer reuses
//!   `tools/gen/compat/blocks.json` and the resolver in
//!   `tools/gen/compat/gen.js`.
//! * `gen` — the generated per-release tables, one module per release. Shared
//!   by every table below.

use aether_world::BlockStateId;

pub mod blocks;

#[allow(clippy::all)]
#[rustfmt::skip]
pub mod gen {
    //! Generated per-version tables; see `tools/gen/compat/gen.js`.
    //!
    //! Every release has one module. A module carries only the tables that
    //! release needs; an unused symbol here is normal, so dead code is allowed
    //! rather than silenced field by field.
    #![allow(dead_code)]

    pub mod v1_8;
    pub mod v1_17;
    pub mod v1_17_1;
    pub mod v1_18;
    pub mod v1_18_2;
    pub mod v1_19;
    pub mod v1_19_2;
    pub mod v1_19_3;
    pub mod v1_19_4;
    pub mod v1_20_1;
    pub mod v1_20_5;
    pub mod v1_21_1;
    pub mod v1_21_3;
    pub mod v1_21_4;
    pub mod v1_21_5;
    pub mod v1_21_6;
    pub mod v1_21_8;
    pub mod v1_21_9;
}

pub use blocks::{
    BlockTable, V1_17, V1_17_1, V1_18, V1_18_2, V1_19, V1_19_2, V1_19_3, V1_19_4, V1_20_1, V1_20_5,
    V1_21_1, V1_21_3, V1_21_4, V1_21_5, V1_21_6, V1_21_8, V1_21_9, V1_8,
};

/// The block table for a codec's protocol id, if this layer knows it.
///
/// Convenience for a codec that wants to translate without naming the static.
/// `None` for 1.8.9 (which is pre-flattening and uses [`V1_8`]) and for any
/// protocol this layer does not cover.
//
// Part of the compat API for consumers (the item layer, new codecs); the
// codecs that exist name their static directly, so only tests call it today.
#[allow(dead_code)]
pub fn block_table(protocol: i32) -> Option<&'static BlockTable> {
    let t = match protocol {
        755 => &V1_17,
        756 => &V1_17_1,
        757 => &V1_18,
        758 => &V1_18_2,
        759 => &V1_19,
        760 => &V1_19_2,
        761 => &V1_19_3,
        762 => &V1_19_4,
        763 => &V1_20_1,
        766 => &V1_20_5,
        767 => &V1_21_1,
        768 => &V1_21_3,
        769 => &V1_21_4,
        770 => &V1_21_5,
        771 => &V1_21_6,
        772 => &V1_21_8,
        773 => &V1_21_9,
        _ => return None,
    };
    Some(t)
}

/// The client's state id for an engine block on the given protocol, using the
/// per-version table. `None` for a protocol this layer does not cover.
#[allow(dead_code)]
pub fn state_for(protocol: i32, id: BlockStateId) -> Option<u32> {
    block_table(protocol).map(|t| t.state(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_convenience_lookups_agree_with_the_named_tables() {
        use aether_api::block_ids as b;
        assert_eq!(state_for(767, b::STONE), Some(V1_21_1.state(b::STONE)));
        assert_eq!(block_table(773).map(|t| t.stone), Some(V1_21_9.stone));
        // 1.8.9 is handled by `V1_8`, not a `BlockTable`.
        assert!(block_table(47).is_none());
        assert!(state_for(999, b::STONE).is_none());
    }
}
