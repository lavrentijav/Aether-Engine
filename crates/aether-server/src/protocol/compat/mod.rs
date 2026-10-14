//! The version compatibility layer (queue J).
//!
//! The engine's world is 1.21.11: [`aether_world::ItemTypeId`]s are that
//! release's item ids. A client on another release has its own numbering, and
//! — for anything added since — no item with that name at all. This layer
//! answers the one question every codec asks: *what does this engine object
//! look like on that client?*
//!
//! This branch owns the **item** half ([`items`], task J3) and the
//! behaviour-preserving stand-ins ([`behaviour`], task J4). The **block** half
//! (block-state translation and block substitution, tasks J1/J2) is a sibling
//! branch and joins here as `compat::blocks` at the merge. Until then the
//! generated item tables substitute *block items* a version lacks with a
//! generic placeholder; see the TODO in `tools/gen/compat/gen_items.js`.
//!
//! A substitute is a disguise: it changes only what the client sees. The
//! server's own world, and every action the client sends, still refer to the
//! real engine object.

#[allow(clippy::all)]
#[rustfmt::skip]
pub mod item_gen {
    //! Generated per-release item tables; see `tools/gen/compat/gen_items.js`.
    #![allow(dead_code)]

    pub mod english;
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

/// The behaviour half: what the *real* item does when a client acts on the
/// substitute it was shown (task J4).
pub mod behaviour;
/// The item half of the layer: per-release item tables and substitution.
pub mod items;
