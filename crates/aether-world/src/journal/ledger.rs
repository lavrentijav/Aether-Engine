//! Where every item instance is, derived from the journal — and what it means
//! when the answer is "two places at once".
//!
//! # Why identity, and not counting
//!
//! A server that tracks item *types* can only ever say "there is more stone
//! than there should be". A server that tracks item *instances* can say
//! *which* stone, *when* it appeared, and *what the player did immediately
//! before*. Duplication bugs are found by the second kind and not the first,
//! because a dupe is usually a legal-looking sequence of legal-looking moves
//! that happens to end with one uid in two places.
//!
//! So every stack carries a [`ItemUid`], minted once, and the journal records
//! each hop it makes. Replaying those hops gives the ledger below, and any
//! disagreement it finds is an [`Anomaly`].
//!
//! # What the ledger does not do
//!
//! It reports; it does not police. A detected anomaly is a lead for a human or
//! for a rollback, not grounds for the engine to delete someone's inventory —
//! a false positive from a missed event would be indistinguishable from a real
//! dupe, and destroying items on a guess is worse than the dupe.

use super::event::{Event, EventBody, ItemUid, Place};
use crate::storage::{KvBackend, StorageError};
use crate::ItemTypeId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Something the history says that cannot be true.
#[derive(Debug, Clone, PartialEq)]
pub enum Anomaly {
    /// A uid moved out of a place it was not in. Either an event is missing,
    /// or the same stack was acted on twice from two different places — the
    /// usual shape of a duplication.
    MovedFromElsewhere {
        uid: ItemUid,
        /// Where the ledger believed it was.
        believed: Place,
        /// Where the event claimed it was.
        claimed: Place,
        seq: u64,
    },
    /// A uid was moved, destroyed or re-minted after it had already been
    /// destroyed.
    UsedAfterDestroyed { uid: ItemUid, seq: u64 },
    /// A uid was minted a second time. A uid is minted once by definition, so
    /// this is either a generator collision or a replayed packet.
    MintedTwice { uid: ItemUid, seq: u64 },
    /// A uid was moved without ever having been minted.
    NeverMinted { uid: ItemUid, seq: u64 },
}

/// What the ledger knows about one item instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// The item type, by the world's registry id.
    pub item: ItemTypeId,
    pub count: u8,
    /// Where it is now, or [`Place::Nowhere`] once destroyed.
    pub at: Place,
    /// The sequence number that minted it.
    pub minted_at: u64,
    /// Every place it has been, oldest first — the provenance trail a human
    /// actually reads when investigating.
    pub trail: Vec<(u64, Place)>,
    /// The instances it was made from (a craft's inputs, the stack it was
    /// split off); empty for one minted from nothing.
    pub derived_from: Vec<ItemUid>,
    destroyed: bool,
}

/// The current whereabouts of every item instance, plus everything the history
/// got wrong.
#[derive(Debug, Default)]
pub struct Ledger {
    items: HashMap<ItemUid, Instance>,
    anomalies: Vec<Anomaly>,
}

impl Ledger {
    /// Build a ledger by replaying `events` in order.
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut l = Ledger::default();
        for e in events {
            l.apply(e);
        }
        l
    }

    /// Fold one event in.
    pub fn apply(&mut self, e: &Event) {
        match &e.body {
            EventBody::BlockSet { .. } => {}
            EventBody::ItemMint {
                uid,
                item,
                count,
                to,
            } => self.mint(e.seq, *uid, *item, *count, *to, Vec::new()),
            EventBody::ItemDerive {
                uid,
                item,
                count,
                to,
                from,
            } => self.mint(e.seq, *uid, *item, *count, *to, from.clone()),
            EventBody::ItemMove {
                uid,
                from,
                to,
                count,
            } => {
                let Some(inst) = self.items.get_mut(uid) else {
                    self.anomalies.push(Anomaly::NeverMinted {
                        uid: *uid,
                        seq: e.seq,
                    });
                    return;
                };
                if inst.destroyed {
                    self.anomalies.push(Anomaly::UsedAfterDestroyed {
                        uid: *uid,
                        seq: e.seq,
                    });
                    return;
                }
                if inst.at != *from {
                    // Recorded, then applied anyway: the move is the newer
                    // information, and refusing it would make every later
                    // event about this uid look wrong too, burying the one
                    // anomaly that matters under a hundred that do not.
                    self.anomalies.push(Anomaly::MovedFromElsewhere {
                        uid: *uid,
                        believed: inst.at,
                        claimed: *from,
                        seq: e.seq,
                    });
                }
                inst.at = *to;
                inst.count = *count;
                inst.trail.push((e.seq, *to));
            }
            EventBody::ItemDestroy { uid, from } => {
                let Some(inst) = self.items.get_mut(uid) else {
                    self.anomalies.push(Anomaly::NeverMinted {
                        uid: *uid,
                        seq: e.seq,
                    });
                    return;
                };
                if inst.destroyed {
                    self.anomalies.push(Anomaly::UsedAfterDestroyed {
                        uid: *uid,
                        seq: e.seq,
                    });
                    return;
                }
                if inst.at != *from {
                    self.anomalies.push(Anomaly::MovedFromElsewhere {
                        uid: *uid,
                        believed: inst.at,
                        claimed: *from,
                        seq: e.seq,
                    });
                }
                inst.destroyed = true;
                inst.at = Place::Nowhere;
                inst.trail.push((e.seq, Place::Nowhere));
            }
        }
    }

    fn mint(
        &mut self,
        seq: u64,
        uid: ItemUid,
        item: ItemTypeId,
        count: u8,
        to: Place,
        derived_from: Vec<ItemUid>,
    ) {
        if self.items.contains_key(&uid) {
            self.anomalies.push(Anomaly::MintedTwice { uid, seq });
            return;
        }
        self.items.insert(
            uid,
            Instance {
                item,
                count,
                at: to,
                minted_at: seq,
                trail: vec![(seq, to)],
                derived_from,
                destroyed: false,
            },
        );
    }

    /// What is known about one uid.
    pub fn get(&self, uid: ItemUid) -> Option<&Instance> {
        self.items.get(&uid)
    }

    /// Everything the replay found wrong, oldest first.
    pub fn anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }

    /// Every live instance in `owner`'s inventory, by slot.
    pub fn inventory_of(&self, owner: super::ActorId) -> Vec<(i16, ItemUid, &Instance)> {
        let mut out: Vec<_> = self
            .items
            .iter()
            .filter_map(|(uid, i)| match i.at {
                Place::Inventory { owner: o, slot } if o == owner && !i.destroyed => {
                    Some((slot, *uid, i))
                }
                _ => None,
            })
            .collect();
        out.sort_by_key(|(slot, uid, _)| (*slot, uid.0));
        out
    }

    /// How many instances the ledger tracks, live and destroyed.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the ledger is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Mint a fresh item uid from the process-wide allocator.
///
/// See [`install_allocator`] and [`UidAllocator`] for why ids are allocated
/// rather than random: they order, they fit in eight bytes, and the reservation
/// bound makes reuse impossible across a restart rather than merely unlikely.
///
/// # Panics
///
/// If an allocator is installed and its backend cannot be written, this panics.
/// A server that cannot persist the next reservation bound cannot hand out an
/// id without risking a duplicate later, and there is no safe value to return
/// instead — a wrong uid corrupts the ledger. Failing loudly is the bug
/// response; the test-only fallback below is not, because silently switching
/// to it on a real server would reintroduce the collision this design removes.
pub fn mint_uid() -> ItemUid {
    match ALLOCATOR.get() {
        Some(alloc) => alloc.next().expect(
            "the item uid allocator could not persist its next reservation block; \
             the storage backend is failing and the uid counter cannot continue safely",
        ),
        None => ItemUid(TEST_UID.fetch_add(1, Ordering::SeqCst)),
    }
}

/// Key holding the item-uid reservation bound: every uid below it may have
/// been handed out, none at or above it has. Written *before* any id from the
/// block it covers is returned, so a restart resumes at the bound and never
/// reuses an id — even after a crash between an id and its event.
const K_UID: &[u8] = b"U";
/// How many ids one reservation write covers. The bound is written once per
/// block rather than once per id; the price is a gap of up to this many unused
/// ids after a crash, which nothing depends on. Matches the journal's own
/// sequence reservation in spirit (see [`super`]).
pub const UID_BLOCK: u64 = 65_536;

/// The process-wide allocator, installed once at startup.
///
/// `None` in unit tests, where [`mint_uid`] falls back to a process-local
/// counter (see [`TEST_UID`]).
static ALLOCATOR: OnceLock<UidAllocator> = OnceLock::new();

/// A process-local counter used only when no allocator is installed.
///
/// Unit tests and short-lived tools call [`mint_uid`] with no world behind
/// them: there is no backend to reserve against and no restart to survive, so
/// a plain `fetch_add` is enough. Starting at `1 << 62` keeps these ids clear
/// of any real world's numbering (which begins at 1) if both live in one
/// process. The server always installs an allocator and never takes this path.
static TEST_UID: AtomicU64 = AtomicU64::new(1 << 62);

/// Hand out item instance ids, monotonically, reserving blocks so that no id
/// is ever reused.
///
/// The hot path is one `fetch_add`; the backend is touched only when the
/// counter reaches the persisted bound, and the raised bound is durable before
/// any id from the new block is returned. This is the journal's own sequence
/// reservation ([`super::Journal`]) applied to item identity. The backend is
/// stored boxed so the process-wide instance has one concrete type while
/// [`UidAllocator::open`] still accepts whatever backend a world uses.
pub struct UidAllocator {
    backend: Box<dyn KvBackend>,
    /// The next id to hand out.
    next: AtomicU64,
    /// The persisted reservation bound ([`K_UID`]).
    reserved: super::Reservation,
}

impl UidAllocator {
    /// Open an allocator over `backend`, resuming its numbering.
    ///
    /// Absent bound: a fresh world, start at 1 (id 0 means "no id"). A bound
    /// that is present but malformed is an error, not "absent": starting
    /// again at 1 would hand out every id a second time.
    pub fn open<B: KvBackend + 'static>(backend: B) -> Result<Self, StorageError> {
        let next = super::read_bound(&backend, K_UID)?.unwrap_or(1).max(1);
        Ok(Self {
            backend: Box::new(backend),
            next: AtomicU64::new(next),
            reserved: super::Reservation::new(K_UID, UID_BLOCK, next),
        })
    }

    /// The id the next call to [`Self::next`] would return.
    pub fn head(&self) -> u64 {
        self.next.load(Ordering::SeqCst)
    }

    /// Mint the next id.
    pub fn next(&self) -> Result<ItemUid, StorageError> {
        let v = self.next.fetch_add(1, Ordering::SeqCst);
        self.reserve(v)?;
        Ok(ItemUid(v))
    }

    /// Make sure `v` lies below the persisted reservation bound, raising the
    /// bound by whole blocks if it does not. The bound is durable before the
    /// id that needs it is returned.
    fn reserve(&self, v: u64) -> Result<(), StorageError> {
        self.reserved.ensure(&*self.backend, v)
    }
}

/// Install the process-wide allocator used by [`mint_uid`].
///
/// Called once by the server before it accepts connections. A second call is
/// ignored: the first allocator owns the reservation bound, and swapping it
/// out under already-minted ids would break the one guarantee ids make.
pub fn install_allocator(alloc: UidAllocator) {
    let _ = ALLOCATOR.set(alloc);
}
