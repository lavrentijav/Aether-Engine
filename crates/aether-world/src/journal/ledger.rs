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
use std::collections::HashMap;

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
    /// Registry name, e.g. `minecraft:diamond`.
    pub item: String,
    pub count: u8,
    /// Where it is now, or [`Place::Nowhere`] once destroyed.
    pub at: Place,
    /// The sequence number that minted it.
    pub minted_at: u64,
    /// Every place it has been, oldest first — the provenance trail a human
    /// actually reads when investigating.
    pub trail: Vec<(u64, Place)>,
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
            } => {
                if self.items.contains_key(uid) {
                    self.anomalies.push(Anomaly::MintedTwice {
                        uid: *uid,
                        seq: e.seq,
                    });
                    return;
                }
                self.items.insert(
                    *uid,
                    Instance {
                        item: item.clone(),
                        count: *count,
                        at: *to,
                        minted_at: e.seq,
                        trail: vec![(e.seq, *to)],
                        destroyed: false,
                    },
                );
            }
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

/// Mint a fresh, unguessable item uid.
///
/// Randomness matters here for a reason that is easy to miss: uids leave the
/// server inside no packet, but they *do* end up in logs and rollback reports.
/// A counter would let anyone who saw two uids infer how much had been created
/// in between, and — worse — would collide across a restart that lost its
/// counter. 128 random bits collide never and infer nothing.
pub fn mint_uid() -> ItemUid {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    // Two independently-seeded hashers, each contributing 64 bits. `RandomState`
    // is seeded from the OS on first use, which is the only entropy source
    // available without adding a dependency to this crate.
    let hi = {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(super::now_ms());
        h.finish()
    };
    let lo = {
        let mut h = RandomState::new().build_hasher();
        h.write_usize(&hi as *const u64 as usize);
        h.finish()
    };
    ItemUid(((hi as u128) << 64) | lo as u128)
}
