//! The top-level key space of a world's KV store.
//!
//! One [`KvBackend`](aether_world::KvBackend) holds the sub-chunks, the
//! journal and its indexes, the registries, and every server-level blob
//! written through [`World::put_meta`](crate::World::put_meta). Each is told
//! apart by the leading byte(s) of its keys, so two subsystems that choose the
//! same byte do not merely "share a namespace" — one's prefix scans will read
//! the other's keys. The journal's actor index (`P` + a 16-byte actor) and a
//! saved player (once also `P` + a 16-byte uuid) were exactly that: the same
//! shape under the same byte.
//!
//! [`KEY_PREFIXES`] is the one list of who owns what, and the test below
//! refuses a prefix that is equal to, or a prefix of, another. To claim a new
//! one: add it here, pick bytes no other owner uses, and keep the test green.
//!
//! # Two shapes of key
//!
//! * **Prefixed keys** start with one of the prefixes below. The rest of the
//!   key is the owner's business; the prefix is what a scan selects on.
//! * **Sub-chunk keys** have no fixed prefix: the 9-byte key starts with the
//!   high byte of a biased chunk coordinate, so it can begin with any byte.
//!   They are safe because they are exactly nine bytes and are matched by
//!   exact length as well as value. A prefixed key must never *be* nine bytes
//!   long with the same content — none is.

/// A subsystem's claim on a top-level key prefix.
pub struct KeyClaim {
    /// What owns the prefix, for the error message a collision would produce.
    pub owner: &'static str,
    /// The leading byte(s). Scanning for exactly this selects only this
    /// owner's keys, provided no other prefix is a prefix of it.
    pub prefix: &'static [u8],
}

// World-owned prefixes. These mirror the definitions in `aether_world`
// (`storage/mod.rs`, `journal/mod.rs`, `journal/ledger.rs`); the world crate
// cannot import from this one, so they are repeated here rather than shared.
// Changing one means changing both.

/// Key holding the world's block-name table.
pub const BLOCK_REGISTRY: &[u8] = b"REG";
/// Key holding the world's item-name table.
pub const ITEM_REGISTRY: &[u8] = b"ITM";
/// Per-column "this chunk was generated" marker.
pub const COLUMN_MARKER: &[u8] = b"C";
/// Per-sub-chunk snapshot's generator revision.
pub const SNAPSHOT_REVISION: &[u8] = b"V";
/// Per-column journal checkpoint.
pub const JOURNAL_CHECKPOINT: &[u8] = b"K";
/// One journal event, by sequence number.
pub const JOURNAL_EVENT: &[u8] = b"E";
/// The journal's per-column index.
pub const JOURNAL_COLUMN: &[u8] = b"X";
/// The journal's per-actor index.
pub const JOURNAL_ACTOR: &[u8] = b"P";
/// The journal's legacy head key, read but no longer written.
pub const JOURNAL_HEAD: &[u8] = b"H";
/// The journal's sequence-number reservation bound.
pub const JOURNAL_RESERVATION: &[u8] = b"N";
/// The item-uid reservation bound.
pub const ITEM_UID_RESERVATION: &[u8] = b"U";

// Server-owned prefixes, written through `put_meta`.

/// Durable scheduled block ticks, per chunk.
pub const SCHEDULED_BLOCK_TICKS: &[u8] = b"BT";
/// Durable scheduled fluid ticks, per chunk.
pub const SCHEDULED_FLUID_TICKS: &[u8] = b"BF";
/// The world's time-of-day and total ticks.
pub const WORLD_TIME: &[u8] = b"Wtime";
/// One player's saved state (position, health, effects, …).
pub const PLAYER_SAVE: &[u8] = b"D";
/// One player's inventory.
pub const PLAYER_INVENTORY: &[u8] = b"F";
/// A block container's contents.
pub const BLOCK_CONTAINER: &[u8] = b"O";
/// Premium-name locks: which Mojang uuid a name that completed online auth
/// belongs to. Sub-keys `AUTHn`/`AUTHu`.
pub const AUTH_LOCK: &[u8] = b"AUTH";
/// Per-player web-admin profile: first/last seen and playtime, keyed by the
/// player's 16-byte uuid (see `aether_server::web`).
pub const WEB_PROFILE: &[u8] = b"WB";
/// The world's game rules, e.g. `sendCommandFeedback`. One blob, owned by the
/// server's command layer (CMD-05 introduced the store; CMD-13 owns
/// `/gamerule`).
pub const GAME_RULES: &[u8] = b"GR";

/// Every top-level key prefix in a world's KV store, and its owner.
///
/// This is the authority for picking a new one: the test below rejects any two
/// entries whose prefixes are equal or one a prefix of the other, which is the
/// condition under which a scan for one would return the other's keys.
pub const KEY_PREFIXES: &[KeyClaim] = &[
    KeyClaim {
        owner: "block registry (REG)",
        prefix: BLOCK_REGISTRY,
    },
    KeyClaim {
        owner: "item registry (ITM)",
        prefix: ITEM_REGISTRY,
    },
    KeyClaim {
        owner: "column-generated marker",
        prefix: COLUMN_MARKER,
    },
    KeyClaim {
        owner: "snapshot generator revision",
        prefix: SNAPSHOT_REVISION,
    },
    KeyClaim {
        owner: "journal per-column checkpoint",
        prefix: JOURNAL_CHECKPOINT,
    },
    KeyClaim {
        owner: "journal event",
        prefix: JOURNAL_EVENT,
    },
    KeyClaim {
        owner: "journal per-column index",
        prefix: JOURNAL_COLUMN,
    },
    KeyClaim {
        owner: "journal per-actor index",
        prefix: JOURNAL_ACTOR,
    },
    KeyClaim {
        owner: "journal legacy head",
        prefix: JOURNAL_HEAD,
    },
    KeyClaim {
        owner: "journal sequence reservation",
        prefix: JOURNAL_RESERVATION,
    },
    KeyClaim {
        owner: "item uid reservation",
        prefix: ITEM_UID_RESERVATION,
    },
    KeyClaim {
        owner: "scheduled block ticks",
        prefix: SCHEDULED_BLOCK_TICKS,
    },
    KeyClaim {
        owner: "scheduled fluid ticks",
        prefix: SCHEDULED_FLUID_TICKS,
    },
    KeyClaim {
        owner: "world time",
        prefix: WORLD_TIME,
    },
    KeyClaim {
        owner: "player save",
        prefix: PLAYER_SAVE,
    },
    KeyClaim {
        owner: "player inventory",
        prefix: PLAYER_INVENTORY,
    },
    KeyClaim {
        owner: "block container",
        prefix: BLOCK_CONTAINER,
    },
    KeyClaim {
        owner: "premium name lock",
        prefix: AUTH_LOCK,
    },
    KeyClaim {
        owner: "web-admin player profile",
        prefix: WEB_PROFILE,
    },
    KeyClaim {
        owner: "game rules",
        prefix: GAME_RULES,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_subsystems_claim_the_same_prefix() {
        // A collision is not only equal bytes: a prefix that is a prefix of
        // another makes a scan for the shorter one return the longer one's
        // keys too (the journal's `P` actor scan and a `P` player key were
        // exactly this). Reject both.
        for (i, a) in KEY_PREFIXES.iter().enumerate() {
            assert!(!a.prefix.is_empty(), "{}: empty prefix", a.owner);
            for b in &KEY_PREFIXES[i + 1..] {
                assert!(
                    !a.prefix.starts_with(b.prefix) && !b.prefix.starts_with(a.prefix),
                    "prefixes collide: `{}` ({:?}) and `{}` ({:?})",
                    a.owner,
                    String::from_utf8_lossy(a.prefix),
                    b.owner,
                    String::from_utf8_lossy(b.prefix),
                );
            }
        }
    }

    #[test]
    fn owner_names_are_distinct_too() {
        // A duplicated owner name means two entries were copy-pasted without
        // thinking about the byte; the byte test would likely pass only by
        // accident, so this catches the mistake earlier and more clearly.
        for (i, a) in KEY_PREFIXES.iter().enumerate() {
            for b in &KEY_PREFIXES[i + 1..] {
                assert_ne!(a.owner, b.owner, "two claims name the same owner");
            }
        }
    }
}
