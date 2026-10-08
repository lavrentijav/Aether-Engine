//! The journal's record type and its wire encoding.
//!
//! Every change the world can undergo is one [`Event`]. An event is written
//! once, never edited, and carries **both sides of the change** — the block
//! that was there and the block that replaced it, the slot an item left and
//! the slot it arrived in. That is what makes the log invertible: undoing an
//! event needs no other state, so a rollback is a backwards walk and not a
//! replay from the beginning of the world.

use crate::BlockStateId;

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

/// A single item *instance*.
///
/// Not an item type: two stacks of 64 stone are two different uids, and
/// splitting a stack mints a new uid for the half that moved. Identity is what
/// makes duplication visible — see [`super::ledger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemUid(pub u128);

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
        /// Registry name of the item, e.g. `minecraft:stone`.
        item: String,
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
    /// An item instance left the world (used up, burnt, despawned).
    ItemDestroy { uid: ItemUid, from: Place },
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
pub const FORMAT_VERSION: u8 = 1;

/// Errors from [`Event::decode`].
#[derive(Debug, PartialEq)]
pub enum DecodeError {
    /// The blob does not start with [`FORMAT_VERSION`].
    Version(u8),
    /// The blob ended in the middle of a field.
    Truncated,
    /// A tag byte names no known variant.
    UnknownTag(u8),
    /// A string field was not UTF-8.
    NotUtf8,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Version(v) => write!(f, "unsupported journal format version {v}"),
            DecodeError::Truncated => write!(f, "journal entry ends mid-field"),
            DecodeError::UnknownTag(t) => write!(f, "unknown journal tag {t}"),
            DecodeError::NotUtf8 => write!(f, "journal string field is not UTF-8"),
        }
    }
}

impl std::error::Error for DecodeError {}

const TAG_BLOCK_SET: u8 = 1;
const TAG_ITEM_MINT: u8 = 2;
const TAG_ITEM_MOVE: u8 = 3;
const TAG_ITEM_DESTROY: u8 = 4;

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
    fn string(&mut self) -> Result<String, DecodeError> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        std::str::from_utf8(s)
            .map(str::to_owned)
            .map_err(|_| DecodeError::NotUtf8)
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
                out.extend_from_slice(&(item.len() as u32).to_be_bytes());
                out.extend_from_slice(item.as_bytes());
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
            EventBody::ItemDestroy { uid, from } => {
                out.push(TAG_ITEM_DESTROY);
                out.extend_from_slice(&uid.0.to_be_bytes());
                put_place(&mut out, from);
            }
        }
        out
    }

    /// Inverse of [`Event::encode`].
    pub fn decode(blob: &[u8]) -> Result<Event, DecodeError> {
        let mut c = Cur { b: blob, at: 0 };
        let v = c.u8()?;
        if v != FORMAT_VERSION {
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
                uid: ItemUid(c.u128()?),
                item: c.string()?,
                count: c.u8()?,
                to: c.place()?,
            },
            TAG_ITEM_MOVE => EventBody::ItemMove {
                uid: ItemUid(c.u128()?),
                count: c.u8()?,
                from: c.place()?,
                to: c.place()?,
            },
            TAG_ITEM_DESTROY => EventBody::ItemDestroy {
                uid: ItemUid(c.u128()?),
                from: c.place()?,
            },
            t => return Err(DecodeError::UnknownTag(t)),
        };
        Ok(Event {
            seq,
            at_ms,
            actor,
            body,
        })
    }
}
