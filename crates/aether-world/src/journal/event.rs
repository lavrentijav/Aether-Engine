//! The journal's record type and its wire encoding.
//!
//! Every change the world can undergo is one [`Event`]. An event is written
//! once, never edited, and carries **both sides of the change** — the block
//! that was there and the block that replaced it, the slot an item left and
//! the slot it arrived in. That is what makes the log invertible: undoing an
//! event needs no other state, so a rollback is a backwards walk and not a
//! replay from the beginning of the world.

use crate::registry::items::ItemTypeId;
use crate::BlockStateId;

// ---------------------------------------------------------------------------
// Code space
// ---------------------------------------------------------------------------
//
// The journal carries compact `u8` codes, never strings. Each code space is
// split: a core range this release defines, and a custom range a plugin may
// register through the plugin API later (P3). The core never reuses a custom
// code, and the decoder keeps an unknown custom code as an opaque payload
// rather than failing the whole event.

/// Lowest core event tag. `0` is reserved as "no tag".
pub const TAG_CORE_MIN: u8 = 1;
/// Highest core event tag. Custom tags start at [`TAG_CUSTOM_MIN`].
pub const TAG_CORE_MAX: u8 = 63;
/// Lowest custom event tag. A tag here this release does not know decodes as
/// [`EventBody::Custom`] with a length-prefixed payload.
pub const TAG_CUSTOM_MIN: u8 = 64;
/// Highest custom event tag.
pub const TAG_CUSTOM_MAX: u8 = 255;

/// Lowest core destroy reason ([`DestroyReason::Other`]).
pub const REASON_CORE_MIN: u8 = 0;
/// Highest core destroy reason. Custom reasons start at
/// [`REASON_CUSTOM_MIN`].
pub const REASON_CORE_MAX: u8 = 127;
/// Lowest custom destroy reason.
pub const REASON_CUSTOM_MIN: u8 = 128;
/// Highest custom destroy reason.
pub const REASON_CUSTOM_MAX: u8 = 255;

/// Whether `tag` is in the custom range and so may be an opaque
/// [`EventBody::Custom`].
pub fn is_custom_tag(tag: u8) -> bool {
    (TAG_CUSTOM_MIN..=TAG_CUSTOM_MAX).contains(&tag)
}

/// Who caused an event.
///
/// A player's UUID, or [`ActorId::SERVER`] for changes the engine made on its
/// own (world generation fixes, scheduled ticks, an operator command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorId(pub u128);

impl ActorId {
    /// Changes with no player behind them.
    pub const SERVER: ActorId = ActorId(0);
}

/// What kind of player text an [`EventBody::ChatMessage`] carries.
///
/// The kind is part of the event's tag (`TAG_CHAT`, `TAG_PM`, `TAG_CMD`), so
/// the history mirror's `kind` column already tells the three apart without
/// reading the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    /// A public chat line, seen by the server.
    Public,
    /// A private message to one other player (the body's `peer`).
    Private,
    /// A command line the player typed, leading slash included.
    Command,
}

/// A single item *instance*.
///
/// Not an item type: two stacks of 64 stone are two different uids, and
/// splitting a stack mints a new uid for the half that moved. Identity is what
/// makes duplication visible — see [`super::ledger`].
///
/// Sixty-four bits, handed out once by the allocator in [`super::ledger`] and
/// never reused. Unlike a random id it orders: a smaller uid was born before a
/// larger one, which "the last ten minutes" reasoning already leans on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemUid(pub u64);

/// Why an item instance left the world.
///
/// A destroy used to be silent about its cause, which told a reader that a
/// stack was gone but not whether it had been eaten, burnt or replaced. The
/// byte is the whole of it: no string reaches the journal.
///
/// Codes `0..=127` are the core's; [`DestroyReason::Custom`] holds a plugin's
/// code unchanged so a reason the core does not know still round-trips
/// (decode then re-encode, or a mirror backfill) instead of collapsing to
/// [`DestroyReason::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DestroyReason {
    /// No better reason is known.
    #[default]
    Other,
    /// Eaten or drunk.
    Consumed,
    /// An item entity's five minutes ran out.
    Despawn,
    /// Destroyed by fire or lava.
    Burned,
    /// Fell out of the world.
    Void,
    /// Caught in an explosion.
    Explosion,
    /// An ingredient used up by a craft, smelt or brew.
    Crafted,
    /// A creative client replaced the slot's contents.
    CreativeReplace,
    /// Removed by a command (`/clear`, a `/kill` drop that is not kept).
    Command,
    /// Cactus or other contact damage.
    Cactus,
    /// Merged into another stack when the merge path had no way to keep the
    /// source alive.
    MergedAway,
    /// A reason code this release does not define, kept verbatim so it
    /// survives a decode/re-encode. Set by a plugin in the custom range
    /// (`REASON_CUSTOM_MIN..=REASON_CUSTOM_MAX`), or by any newer writer whose
    /// code is not in this build's core list.
    Custom(u8),
}

impl DestroyReason {
    /// The reason a byte names. A code this release does not define — a
    /// plugin's custom reason, or a core code a newer build added — is kept as
    /// [`DestroyReason::Custom`] rather than folded into
    /// [`DestroyReason::Other`], so the byte round-trips.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => DestroyReason::Other,
            1 => DestroyReason::Consumed,
            2 => DestroyReason::Despawn,
            3 => DestroyReason::Burned,
            4 => DestroyReason::Void,
            5 => DestroyReason::Explosion,
            6 => DestroyReason::Crafted,
            7 => DestroyReason::CreativeReplace,
            8 => DestroyReason::Command,
            9 => DestroyReason::Cactus,
            10 => DestroyReason::MergedAway,
            other => DestroyReason::Custom(other),
        }
    }

    /// The wire code for this reason.
    pub fn code(self) -> u8 {
        match self {
            DestroyReason::Other => 0,
            DestroyReason::Consumed => 1,
            DestroyReason::Despawn => 2,
            DestroyReason::Burned => 3,
            DestroyReason::Void => 4,
            DestroyReason::Explosion => 5,
            DestroyReason::Crafted => 6,
            DestroyReason::CreativeReplace => 7,
            DestroyReason::Command => 8,
            DestroyReason::Cactus => 9,
            DestroyReason::MergedAway => 10,
            DestroyReason::Custom(code) => code,
        }
    }

    /// The stable name of this reason, as the PostgreSQL code table and the
    /// chat display both spell it. One source of truth: nothing else hardcodes
    /// these strings. A custom reason has no name in this build — the plugin
    /// that owns its code names it — so it reads as `custom`.
    pub fn name(self) -> &'static str {
        match self {
            DestroyReason::Other => "other",
            DestroyReason::Consumed => "consumed",
            DestroyReason::Despawn => "despawn",
            DestroyReason::Burned => "burned",
            DestroyReason::Void => "void",
            DestroyReason::Explosion => "explosion",
            DestroyReason::Crafted => "crafted",
            DestroyReason::CreativeReplace => "creative_replace",
            DestroyReason::Command => "command",
            DestroyReason::Cactus => "cactus",
            DestroyReason::MergedAway => "merged_away",
            DestroyReason::Custom(_) => "custom",
        }
    }
}

/// Every core destroy reason, in code order. The PostgreSQL mirror upserts a
/// `aether_destroy_reasons` row per entry from this list, so the table and the
/// code cannot drift.
pub const DESTROY_REASONS: &[DestroyReason] = &[
    DestroyReason::Other,
    DestroyReason::Consumed,
    DestroyReason::Despawn,
    DestroyReason::Burned,
    DestroyReason::Void,
    DestroyReason::Explosion,
    DestroyReason::Crafted,
    DestroyReason::CreativeReplace,
    DestroyReason::Command,
    DestroyReason::Cactus,
    DestroyReason::MergedAway,
];

/// Every core event tag and its stable name, in tag order. The PostgreSQL
/// mirror upserts `aether_event_kinds` from this list, so the table and the
/// wire tags cannot drift. The name is the same string the mirror used to
/// store in its `kind` *text* column before it became a `smallint`.
pub const EVENT_KINDS: &[(u8, &str)] = &[
    (TAG_BLOCK_SET, "block_set"),
    (TAG_ITEM_MINT, "item_mint"),
    (TAG_ITEM_MOVE, "item_move"),
    (TAG_ITEM_DESTROY, "item_destroy"),
    (TAG_ITEM_DERIVE, "item_derive"),
    (TAG_ITEM_MERGE, "item_merge"),
    (TAG_CHAT, "chat"),
    (TAG_PM, "pm"),
    (TAG_CMD, "command"),
];

/// The `aether_event_kinds` row name used while migrating old text rows whose
/// name no core tag claims. Tag `0` is reserved and means "no tag".
pub const EVENT_KIND_UNKNOWN: (u8, &str) = (0, "unknown");

/// Where an item instance sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Slot `slot` of `owner`'s inventory.
    Inventory { owner: ActorId, slot: i16 },
    /// Lying in the world at these block coordinates.
    Ground { x: i32, y: i32, z: i32 },
    /// A container at these block coordinates.
    Container { x: i32, y: i32, z: i32, slot: i16 },
    /// Outside the world: destroyed, or not yet created.
    Nowhere,
}

/// What happened.
#[derive(Debug, Clone, PartialEq)]
pub enum EventBody {
    /// One block changed. `from` is what stood there, which is the whole of
    /// the undo.
    BlockSet {
        x: i32,
        y: i32,
        z: i32,
        from: BlockStateId,
        to: BlockStateId,
    },
    /// An item instance came into existence — creative give, a block broken
    /// into a drop, a craft. Every uid in the world must have exactly one of
    /// these; a uid that appears without one is the signature of a dupe.
    ItemMint {
        uid: ItemUid,
        /// The item type, by the world's registry id. Names live at the edges
        /// (commands, the PostgreSQL mirror), never in the log.
        item: ItemTypeId,
        count: u8,
        to: Place,
    },
    /// An item instance moved. Covers taking, dropping and picking up: they
    /// differ only in the two places.
    ItemMove {
        uid: ItemUid,
        from: Place,
        to: Place,
        count: u8,
    },
    /// An item instance left the world (used up, burnt, despawned). `reason`
    /// says why; without it a destroy could not be told from a bug.
    ItemDestroy {
        uid: ItemUid,
        from: Place,
        reason: DestroyReason,
    },
    /// `count` units of stack `from` joined stack `into`.
    ///
    /// The merge exists so that topping up a stack is not journalled as a move
    /// of the whole stack: the destination's count grew from somewhere, and a
    /// movement event with the same `from` and `to` said nothing about where.
    /// `from` ceases to exist when it is fully merged; there is deliberately
    /// no separate destroy for it.
    ItemMerge {
        into: ItemUid,
        from: ItemUid,
        count: u8,
    },
    /// An item instance came into existence *out of* others by a real
    /// transformation: the output of a craft, smelt, brew, stonecut or
    /// smithing from its consumed ingredients. A mint with a pedigree — the
    /// edge provenance follows backwards.
    ///
    /// Deliberately not a split, and not a pickup: the half split off a stack
    /// is a new identity for units that already existed (an [`ItemMint`],
    /// with the source moving the other way), and a pickup is an
    /// [`ItemMove`] or an [`ItemMerge`]. Reserve this for a change of item.
    ItemDerive {
        uid: ItemUid,
        item: ItemTypeId,
        count: u8,
        to: Place,
        /// The instances it was made from, as they were consumed or reduced
        /// in the same action.
        from: Vec<ItemUid>,
    },
    /// Player text: a public chat line, a private message or a command.
    ///
    /// The journal otherwise carries only compact codes, but chat *is* text,
    /// so the bytes are kept UTF-8 with a length prefix. `x`/`y`/`z` is where
    /// the speaker stood, so "everything said near here" is answerable;
    /// `peer` is the recipient of a private message and [`ActorId::SERVER`]
    /// for public chat and commands. A command records the line as typed,
    /// after a secret argument has been redacted. None of these change the
    /// world, so a rollback ignores them.
    ChatMessage {
        kind: ChatKind,
        x: i32,
        y: i32,
        z: i32,
        peer: ActorId,
        text: String,
    },
    /// An event a plugin defined, under a tag in the custom range
    /// (`>= TAG_CUSTOM_MIN`). Its payload is opaque to the core and stored
    /// length-prefixed, so a journal written with plugins still decodes on a
    /// build without them, and a plugin the reader does not have is a payload
    /// it skips rather than an error that hides the rest of the history.
    Custom {
        /// The custom tag, in `TAG_CUSTOM_MIN..=TAG_CUSTOM_MAX`. The core
        /// never assigns one of these.
        tag: u8,
        /// The plugin's opaque payload.
        bytes: Vec<u8>,
    },
}

impl EventBody {
    /// The wire tag of this body. For [`EventBody::Custom`] this is the
    /// plugin's own tag.
    pub fn tag(&self) -> u8 {
        match self {
            EventBody::BlockSet { .. } => TAG_BLOCK_SET,
            EventBody::ItemMint { .. } => TAG_ITEM_MINT,
            EventBody::ItemMove { .. } => TAG_ITEM_MOVE,
            EventBody::ItemDestroy { .. } => TAG_ITEM_DESTROY,
            EventBody::ItemDerive { .. } => TAG_ITEM_DERIVE,
            EventBody::ItemMerge { .. } => TAG_ITEM_MERGE,
            EventBody::ChatMessage { kind, .. } => match kind {
                ChatKind::Public => TAG_CHAT,
                ChatKind::Private => TAG_PM,
                ChatKind::Command => TAG_CMD,
            },
            EventBody::Custom { tag, .. } => *tag,
        }
    }
}

/// One immutable entry in the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Position in the global order. Assigned by the journal, never reused.
    pub seq: u64,
    /// Wall-clock time, milliseconds since the Unix epoch. Only ever used to
    /// resolve a human's "the last ten minutes" into a sequence number.
    pub at_ms: u64,
    pub actor: ActorId,
    pub body: EventBody,
    /// The event this one happened because of, if any — the block break a
    /// drop came out of, say. Lets history be followed by cause as well as by
    /// time.
    pub cause: Option<u64>,
}

impl Event {
    /// The block coordinates this event concerns, if it concerns any.
    ///
    /// Drives the radius filter: an event with no position is global and is
    /// only ever swept up by an unrestricted rollback.
    pub fn position(&self) -> Option<(i32, i32, i32)> {
        match &self.body {
            EventBody::BlockSet { x, y, z, .. } => Some((*x, *y, *z)),
            EventBody::ItemMint { to, .. } => to.position(),
            EventBody::ItemMove { to, from, .. } => to.position().or_else(|| from.position()),
            EventBody::ItemDestroy { from, .. } => from.position(),
            EventBody::ItemMerge { .. } => None,
            EventBody::ItemDerive { to, .. } => to.position(),
            // Where the speaker stood, so a radius filter can find what was
            // said nearby.
            EventBody::ChatMessage { x, y, z, .. } => Some((*x, *y, *z)),
            // A plugin event carries no position the core understands.
            EventBody::Custom { .. } => None,
        }
    }
}

impl Place {
    /// The block coordinates of a place that has any.
    pub fn position(&self) -> Option<(i32, i32, i32)> {
        match self {
            Place::Ground { x, y, z } | Place::Container { x, y, z, .. } => Some((*x, *y, *z)),
            Place::Inventory { .. } | Place::Nowhere => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Bumped whenever the layout below changes. A blob whose first byte is not
/// this is refused rather than guessed at: a misread journal would undo the
/// wrong blocks, which is worse than refusing to undo anything.
///
/// Version 3 replaced the item type's `String` name with a two-byte
/// [`ItemTypeId`]. Version 4 added the [`EventBody::ItemMerge`] body (tag 6)
/// and a `reason` byte to [`EventBody::ItemDestroy`]; a v1–v3 destroy reads
/// back as [`DestroyReason::Other`]. Version 4 also reserved the custom tag
/// range: a tag this release does not know but that lies in
/// `TAG_CUSTOM_MIN..=TAG_CUSTOM_MAX` decodes as [`EventBody::Custom`] with a
/// length-prefixed payload, so a plugin's events are skipped, not fatal.
/// Version 5 added the player-text bodies ([`EventBody::ChatMessage`]: public
/// chat, private message and command, tags 7–9), each carrying a position, an
/// optional peer, and UTF-8 text.
pub const FORMAT_VERSION: u8 = 5;

/// Errors from [`Event::decode`].
#[derive(Debug, PartialEq)]
pub enum DecodeError {
    /// The blob does not start with [`FORMAT_VERSION`].
    Version(u8),
    /// The blob ended in the middle of a field.
    Truncated,
    /// A tag byte names no known variant.
    UnknownTag(u8),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Version(v) => write!(f, "unsupported journal format version {v}"),
            DecodeError::Truncated => write!(f, "journal entry ends mid-field"),
            DecodeError::UnknownTag(t) => write!(f, "unknown journal tag {t}"),
        }
    }
}

impl std::error::Error for DecodeError {}

const TAG_BLOCK_SET: u8 = 1;
const TAG_ITEM_MINT: u8 = 2;
const TAG_ITEM_MOVE: u8 = 3;
const TAG_ITEM_DESTROY: u8 = 4;
const TAG_ITEM_DERIVE: u8 = 5;
const TAG_ITEM_MERGE: u8 = 6;
/// Player text: public chat, private message and command. Three tags so the
/// history mirror's `kind` column tells them apart on its own.
const TAG_CHAT: u8 = 7;
const TAG_PM: u8 = 8;
const TAG_CMD: u8 = 9;
/// Optional trailer after the body: the cause. Absent in entries written
/// before causes existed, which therefore decode with none.
const TRAILER_CAUSE: u8 = 1;

const PLACE_INVENTORY: u8 = 1;
const PLACE_GROUND: u8 = 2;
const PLACE_CONTAINER: u8 = 3;
const PLACE_NOWHERE: u8 = 4;

fn put_place(out: &mut Vec<u8>, p: &Place) {
    match p {
        Place::Inventory { owner, slot } => {
            out.push(PLACE_INVENTORY);
            out.extend_from_slice(&owner.0.to_be_bytes());
            out.extend_from_slice(&slot.to_be_bytes());
        }
        Place::Ground { x, y, z } => {
            out.push(PLACE_GROUND);
            out.extend_from_slice(&x.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
            out.extend_from_slice(&z.to_be_bytes());
        }
        Place::Container { x, y, z, slot } => {
            out.push(PLACE_CONTAINER);
            out.extend_from_slice(&x.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
            out.extend_from_slice(&z.to_be_bytes());
            out.extend_from_slice(&slot.to_be_bytes());
        }
        Place::Nowhere => out.push(PLACE_NOWHERE),
    }
}

/// A cursor over a byte slice that refuses to read past the end.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at.checked_add(n).ok_or(DecodeError::Truncated)?;
        if end > self.b.len() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, DecodeError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u128(&mut self) -> Result<u128, DecodeError> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }
    /// An item uid in format `v`: version 1 wrote the old random 16-byte uids,
    /// read back as their low 64 bits (history only: no live stack carries a
    /// version-1 uid, so the narrowing cannot alias a current item).
    fn uid(&mut self, v: u8) -> Result<ItemUid, DecodeError> {
        Ok(ItemUid(if v == 1 {
            self.u128()? as u64
        } else {
            self.u64()?
        }))
    }
    /// An item type in format `v`: versions 1–2 wrote the registry name, a
    /// `u32` length then UTF-8; a name this release does not know reads as air
    /// rather than failing the whole event, so old history stays readable.
    fn item(&mut self, v: u8) -> Result<ItemTypeId, DecodeError> {
        if v >= 3 {
            return Ok(ItemTypeId(self.u16()?));
        }
        let len = self.u32()? as usize;
        let name = std::str::from_utf8(self.take(len)?).map_err(|_| DecodeError::Truncated)?;
        Ok(ItemTypeId::vanilla(name).unwrap_or(ItemTypeId::AIR))
    }
    /// Length-prefixed UTF-8 player text. Bad UTF-8 is refused as a truncation:
    /// a blob this release wrote is always valid, and guessing at a damaged
    /// one would put mojibake in the history rather than dropping the event.
    fn text(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| DecodeError::Truncated)
    }
    fn place(&mut self) -> Result<Place, DecodeError> {
        match self.u8()? {
            PLACE_INVENTORY => Ok(Place::Inventory {
                owner: ActorId(self.u128()?),
                slot: self.i16()?,
            }),
            PLACE_GROUND => Ok(Place::Ground {
                x: self.i32()?,
                y: self.i32()?,
                z: self.i32()?,
            }),
            PLACE_CONTAINER => Ok(Place::Container {
                x: self.i32()?,
                y: self.i32()?,
                z: self.i32()?,
                slot: self.i16()?,
            }),
            PLACE_NOWHERE => Ok(Place::Nowhere),
            t => Err(DecodeError::UnknownTag(t)),
        }
    }
}

impl Event {
    /// Serialize for storage.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.at_ms.to_be_bytes());
        out.extend_from_slice(&self.actor.0.to_be_bytes());
        match &self.body {
            EventBody::BlockSet { x, y, z, from, to } => {
                out.push(TAG_BLOCK_SET);
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
                out.extend_from_slice(&z.to_be_bytes());
                out.extend_from_slice(&from.0.to_be_bytes());
                out.extend_from_slice(&to.0.to_be_bytes());
            }
            EventBody::ItemMint {
                uid,
                item,
                count,
                to,
            } => {
                out.push(TAG_ITEM_MINT);
                out.extend_from_slice(&uid.0.to_be_bytes());
                out.extend_from_slice(&item.0.to_be_bytes());
                out.push(*count);
                put_place(&mut out, to);
            }
            EventBody::ItemMove {
                uid,
                from,
                to,
                count,
            } => {
                out.push(TAG_ITEM_MOVE);
                out.extend_from_slice(&uid.0.to_be_bytes());
                out.push(*count);
                put_place(&mut out, from);
                put_place(&mut out, to);
            }
            EventBody::ItemDestroy { uid, from, reason } => {
                out.push(TAG_ITEM_DESTROY);
                out.extend_from_slice(&uid.0.to_be_bytes());
                put_place(&mut out, from);
                out.push(reason.code());
            }
            EventBody::ItemMerge { into, from, count } => {
                out.push(TAG_ITEM_MERGE);
                out.extend_from_slice(&into.0.to_be_bytes());
                out.extend_from_slice(&from.0.to_be_bytes());
                out.push(*count);
            }
            EventBody::ItemDerive {
                uid,
                item,
                count,
                to,
                from,
            } => {
                out.push(TAG_ITEM_DERIVE);
                out.extend_from_slice(&uid.0.to_be_bytes());
                out.extend_from_slice(&item.0.to_be_bytes());
                out.push(*count);
                put_place(&mut out, to);
                out.extend_from_slice(&(from.len() as u16).to_be_bytes());
                for f in from {
                    out.extend_from_slice(&f.0.to_be_bytes());
                }
            }
            EventBody::ChatMessage {
                kind,
                x,
                y,
                z,
                peer,
                text,
            } => {
                out.push(match kind {
                    ChatKind::Public => TAG_CHAT,
                    ChatKind::Private => TAG_PM,
                    ChatKind::Command => TAG_CMD,
                });
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
                out.extend_from_slice(&z.to_be_bytes());
                out.extend_from_slice(&peer.0.to_be_bytes());
                out.extend_from_slice(&(text.len() as u32).to_be_bytes());
                out.extend_from_slice(text.as_bytes());
            }
            EventBody::Custom { tag, bytes } => {
                debug_assert!(
                    is_custom_tag(*tag),
                    "custom body tag {tag} is in the core range"
                );
                out.push(*tag);
                // Length-prefixed so an unknown payload can be skipped whole:
                // the core may not understand a plugin's bytes, but it must
                // still be able to read past them to the next field or event.
                out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                out.extend_from_slice(bytes);
            }
        }
        if let Some(cause) = self.cause {
            out.push(TRAILER_CAUSE);
            out.extend_from_slice(&cause.to_be_bytes());
        }
        out
    }

    /// Inverse of [`Event::encode`].
    pub fn decode(blob: &[u8]) -> Result<Event, DecodeError> {
        let mut c = Cur { b: blob, at: 0 };
        // Older formats stay readable: history written by an earlier build must
        // not turn `/inspect` and rollback into errors after an upgrade.
        let v = c.u8()?;
        if !(1..=FORMAT_VERSION).contains(&v) {
            return Err(DecodeError::Version(v));
        }
        let seq = c.u64()?;
        let at_ms = c.u64()?;
        let actor = ActorId(c.u128()?);
        let body = match c.u8()? {
            TAG_BLOCK_SET => EventBody::BlockSet {
                x: c.i32()?,
                y: c.i32()?,
                z: c.i32()?,
                from: BlockStateId(c.u32()?),
                to: BlockStateId(c.u32()?),
            },
            TAG_ITEM_MINT => EventBody::ItemMint {
                uid: c.uid(v)?,
                item: c.item(v)?,
                count: c.u8()?,
                to: c.place()?,
            },
            TAG_ITEM_MOVE => EventBody::ItemMove {
                uid: c.uid(v)?,
                count: c.u8()?,
                from: c.place()?,
                to: c.place()?,
            },
            TAG_ITEM_DESTROY => {
                let uid = c.uid(v)?;
                let from = c.place()?;
                // The reason byte is new in v4; older entries simply had none.
                let reason = if v >= 4 {
                    DestroyReason::from_u8(c.u8()?)
                } else {
                    DestroyReason::Other
                };
                EventBody::ItemDestroy { uid, from, reason }
            }
            TAG_ITEM_MERGE if v >= 4 => EventBody::ItemMerge {
                into: c.uid(v)?,
                from: c.uid(v)?,
                count: c.u8()?,
            },
            TAG_ITEM_DERIVE => {
                let uid = c.uid(v)?;
                let item = c.item(v)?;
                let count = c.u8()?;
                let to = c.place()?;
                let n = c.u16()?;
                let mut from = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    from.push(c.uid(v)?);
                }
                EventBody::ItemDerive {
                    uid,
                    item,
                    count,
                    to,
                    from,
                }
            }
            // A tag in the custom range is a plugin's: keep its payload
            // opaque and length-prefixed rather than failing. Gated on v4, the
            // version that reserved the range.
            t if v >= 4 && is_custom_tag(t) => {
                let len = c.u32()? as usize;
                let bytes = c.take(len)?.to_vec();
                EventBody::Custom { tag: t, bytes }
            }
            // Player text: public chat, private message, command. New in v5.
            t @ (TAG_CHAT | TAG_PM | TAG_CMD) if v >= 5 => EventBody::ChatMessage {
                kind: match t {
                    TAG_PM => ChatKind::Private,
                    TAG_CMD => ChatKind::Command,
                    _ => ChatKind::Public,
                },
                x: c.i32()?,
                y: c.i32()?,
                z: c.i32()?,
                peer: ActorId(c.u128()?),
                text: c.text()?,
            },
            t => return Err(DecodeError::UnknownTag(t)),
        };
        let cause = if c.at < c.b.len() {
            match c.u8()? {
                TRAILER_CAUSE => Some(c.u64()?),
                t => return Err(DecodeError::UnknownTag(t)),
            }
        } else {
            None
        };
        Ok(Event {
            seq,
            at_ms,
            actor,
            body,
            cause,
        })
    }
}
