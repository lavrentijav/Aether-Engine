//! Tests for the journal and the rollback.
//!
//! The decoders here are written from the layout described in [`super::event`]
//! rather than by calling the encoder, so an encoder bug cannot hide behind a
//! test that shares it.

use super::*;
use crate::storage::{KvBackend, MemStore, StorageError};
use crate::{BlockStateId, ItemTypeId};

const DIRT: BlockStateId = BlockStateId(3);
const STONE: BlockStateId = BlockStateId(1);
const DIAMOND: BlockStateId = BlockStateId(74);

fn alice() -> ActorId {
    ActorId(0x1111_1111_1111_1111_1111_1111_1111_1111)
}
fn bob() -> ActorId {
    ActorId(0x2222_2222_2222_2222_2222_2222_2222_2222)
}

fn set(x: i32, y: i32, z: i32, from: BlockStateId, to: BlockStateId) -> EventBody {
    EventBody::BlockSet { x, y, z, from, to }
}

/// The id of a vanilla item, for test events: a name here is a literal the
/// game owns, so an unknown one is a typo and panics.
fn it(name: &str) -> ItemTypeId {
    ItemTypeId::vanilla(name).unwrap_or_else(|| panic!("{name} is not an item"))
}

#[test]
fn an_event_survives_a_round_trip_through_storage() {
    let e = Event {
        seq: 7,
        at_ms: 1_700_000_000_000,
        actor: alice(),
        cause: None,
        body: set(-5, 70, 300, DIRT, STONE),
    };
    assert_eq!(Event::decode(&e.encode()), Ok(e));
}

#[test]
fn the_encoded_header_is_the_layout_the_docs_describe() {
    // Read by hand: version, then seq, at_ms, actor — big-endian, no padding.
    let e = Event {
        seq: 0x0102_0304_0506_0708,
        at_ms: 0x1112_1314_1516_1718,
        actor: ActorId(0xAB),
        cause: None,
        body: set(1, 2, 3, DIRT, STONE),
    };
    let b = e.encode();
    assert_eq!(b[0], event::FORMAT_VERSION);
    assert_eq!(&b[1..9], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(&b[9..17], &[0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);
    assert_eq!(u128::from_be_bytes(b[17..33].try_into().unwrap()), 0xAB);
}

#[test]
fn an_item_uid_is_eight_bytes_after_the_header() {
    // Header: version(1) + seq(8) + at_ms(8) + actor(16) = 33 bytes, then the
    // mint tag and the uid. Read by hand so an encoder and decoder that shared
    // a wrong width could not agree with each other and still pass.
    let e = Event {
        seq: 1,
        at_ms: 2,
        actor: alice(),
        cause: None,
        body: EventBody::ItemMint {
            uid: ItemUid(0x0102_0304_0506_0708),
            item: ItemTypeId::AIR,
            count: 0,
            to: Place::Nowhere,
        },
    };
    let b = e.encode();
    assert_eq!(b[34..42], [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
}

/// The common prefix of every format: version, seq, time, actor.
fn header(version: u8, actor: u128) -> Vec<u8> {
    let mut b = vec![version];
    b.extend_from_slice(&7u64.to_be_bytes());
    b.extend_from_slice(&9u64.to_be_bytes());
    b.extend_from_slice(&actor.to_be_bytes());
    b
}

#[test]
fn a_version_1_or_2_block_event_still_decodes() {
    // Block events did not change shape across versions; an upgraded world's
    // history must stay readable for `/inspect` and rollback.
    for version in [1u8, 2] {
        let mut b = header(version, alice().0);
        b.push(1); // TAG_BLOCK_SET
        for v in [4i32, 5, 6] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&DIRT.0.to_be_bytes());
        b.extend_from_slice(&STONE.0.to_be_bytes());
        let e = Event::decode(&b).unwrap_or_else(|err| panic!("v{version}: {err}"));
        assert_eq!((e.seq, e.at_ms, e.actor), (7, 9, alice()));
        assert_eq!(e.body, set(4, 5, 6, DIRT, STONE), "v{version}");
    }
}

#[test]
fn old_item_events_read_their_uid_width_and_item_name() {
    let name = b"minecraft:diamond";
    // v1: 16-byte uid (low 64 bits kept), name; v2: 8-byte uid, name.
    for (version, uid) in [(1u8, (0xAAu128 << 64) | 0x1234), (2, 0x1234)] {
        let mut b = header(version, alice().0);
        b.push(2); // TAG_ITEM_MINT
        if version == 1 {
            b.extend_from_slice(&uid.to_be_bytes());
        } else {
            b.extend_from_slice(&(uid as u64).to_be_bytes());
        }
        b.extend_from_slice(&(name.len() as u32).to_be_bytes());
        b.extend_from_slice(name);
        b.push(3); // count
        b.push(4); // Place::Nowhere
        let e = Event::decode(&b).unwrap_or_else(|err| panic!("v{version}: {err}"));
        match e.body {
            EventBody::ItemMint {
                uid, item, count, ..
            } => {
                assert_eq!(uid, ItemUid(0x1234), "v{version}");
                assert_eq!(item, ItemTypeId::vanilla("minecraft:diamond").unwrap());
                assert_eq!(count, 3);
            }
            other => panic!("v{version}: {other:?}"),
        }
    }
}

#[test]
fn a_blob_from_another_format_version_is_refused_not_guessed_at() {
    let mut b = Event {
        seq: 1,
        at_ms: 1,
        actor: alice(),
        cause: None,
        body: set(0, 0, 0, DIRT, STONE),
    }
    .encode();
    b[0] = event::FORMAT_VERSION.wrapping_add(1);
    assert!(matches!(Event::decode(&b), Err(DecodeError::Version(_))));
}

#[test]
fn a_truncated_blob_is_refused_at_every_length() {
    let full = Event {
        seq: 1,
        at_ms: 1,
        actor: alice(),
        cause: None,
        body: EventBody::ItemMint {
            uid: ItemUid(9),
            item: it("minecraft:diamond"),
            count: 64,
            to: Place::Inventory {
                owner: alice(),
                slot: 3,
            },
        },
    }
    .encode();
    for n in 0..full.len() {
        assert!(
            Event::decode(&full[..n]).is_err(),
            "prefix of {n} bytes decoded"
        );
    }
    assert!(Event::decode(&full).is_ok());
}

#[test]
fn every_place_variant_round_trips() {
    for place in [
        Place::Inventory {
            owner: bob(),
            slot: -1,
        },
        Place::Ground {
            x: -1,
            y: -64,
            z: 1_000_000,
        },
        Place::Container {
            x: 5,
            y: 6,
            z: 7,
            slot: 26,
        },
        Place::Nowhere,
    ] {
        let e = Event {
            seq: 1,
            at_ms: 2,
            actor: bob(),
            cause: None,
            body: EventBody::ItemDestroy {
                uid: ItemUid(1),
                from: place,
                reason: DestroyReason::Other,
            },
        };
        assert_eq!(Event::decode(&e.encode()), Ok(e));
    }
}

// ---------------------------------------------------------------------------

#[test]
fn sequence_numbers_continue_across_a_reopen() {
    // The failure this guards against is silent: a journal that restarted its
    // numbering would overwrite the first events of the previous session, and
    // nothing would report an error.
    let store = MemStore::new();
    {
        let j = Journal::open(&store).unwrap();
        j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap();
        j.append(alice(), set(1, 64, 0, DIRT, STONE)).unwrap();
    }
    let j = Journal::open(&store).unwrap();
    let seq = j.append(alice(), set(2, 64, 0, DIRT, STONE)).unwrap();
    // Numbering resumes at the reservation bound, so it may skip ahead; what
    // matters is that it moves forward and nothing was overwritten.
    assert!(seq > 2, "resumed at {seq}, at or below an existing event");
    assert_eq!(j.all_events().unwrap().len(), 3);
}

#[test]
fn a_lost_reservation_still_resumes_above_the_last_event() {
    let store = MemStore::new();
    {
        let j = Journal::open(&store).unwrap();
        for i in 0..5 {
            j.append(alice(), set(i, 64, 0, DIRT, STONE)).unwrap();
        }
    }
    store.delete(b"N").unwrap();
    let j = Journal::open(&store).unwrap();
    assert_eq!(j.head(), 6, "must resume above the highest stored event");
}

#[test]
fn an_old_head_marker_behind_a_stored_event_does_not_cause_an_overwrite() {
    // A journal from before reservations crashed after writing event 3 but
    // before moving its head past 2. Resuming at head + 1 would overwrite 3.
    let store = MemStore::new();
    {
        let j = Journal::open(&store).unwrap();
        for i in 0..3 {
            j.append(alice(), set(i, 64, 0, DIRT, STONE)).unwrap();
        }
    }
    store.delete(b"N").unwrap();
    store.put(b"H", &2u64.to_be_bytes()).unwrap();
    let j = Journal::open(&store).unwrap();
    assert_eq!(j.head(), 4);
    j.append(bob(), set(9, 64, 0, DIRT, STONE)).unwrap();
    assert_eq!(
        j.get(3).unwrap().unwrap().actor,
        alice(),
        "event 3 was overwritten"
    );
}

#[test]
fn the_reservation_is_written_once_per_block_not_per_event() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    for i in 0..10 {
        j.append(alice(), set(i, 64, 0, DIRT, STONE)).unwrap();
    }
    let bound = u64::from_be_bytes(store.get(b"N").unwrap().unwrap().try_into().unwrap());
    assert_eq!(bound, SEQ_BLOCK, "ten events fit in the first block");
}

#[test]
fn concurrent_appends_then_a_reopen_never_reuse_a_sequence_number() {
    // Appends that finish out of order used to be able to write the head
    // marker backwards, so a restart resumed below events already stored.
    let store = std::sync::Arc::new(MemStore::new());
    let total = 8 * 600;
    {
        let j = Journal::open(std::sync::Arc::clone(&store)).unwrap();
        std::thread::scope(|s| {
            for t in 0..8u128 {
                let j = &j;
                s.spawn(move || {
                    for i in 0..600 {
                        j.append(ActorId(t), set(i, 64, t as i32, DIRT, STONE))
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(j.all_events().unwrap().len(), total);
    }
    let j = Journal::open(std::sync::Arc::clone(&store)).unwrap();
    let max = j.all_events().unwrap().iter().map(|e| e.seq).max().unwrap();
    assert!(
        j.head() > max,
        "resumed at {} with event {max} stored",
        j.head()
    );
    j.append(alice(), set(0, 0, 0, DIRT, STONE)).unwrap();
    assert_eq!(
        j.all_events().unwrap().len(),
        total + 1,
        "an append overwrote history"
    );
}

#[test]
fn column_lookup_returns_only_that_columns_events() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(3, 64, 3, DIRT, STONE)).unwrap(); // column 0,0
    j.append(alice(), set(-1, 64, 3, DIRT, STONE)).unwrap(); // column -1,0
    j.append(alice(), set(31, 64, 3, DIRT, STONE)).unwrap(); // column 1,0

    let c00 = j.column_events(0, 0).unwrap();
    assert_eq!(c00.len(), 1);
    assert_eq!(c00[0].position(), Some((3, 64, 3)));
    assert_eq!(j.column_events(-1, 0).unwrap().len(), 1);
    assert_eq!(j.column_events(1, 0).unwrap().len(), 1);
    assert!(j.column_events(9, 9).unwrap().is_empty());
}

#[test]
fn column_events_come_back_in_the_order_they_happened() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    for i in 0..300u64 {
        j.append(alice(), set(1, i as i32, 1, DIRT, STONE)).unwrap();
    }
    let seqs: Vec<u64> = j
        .column_events(0, 0)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    // 300 crosses the single-byte boundary in the key's sequence suffix, which
    // is exactly where a little-endian encoding would start returning them out
    // of order.
    assert_eq!(seqs, sorted);
}

// ---------------------------------------------------------------------------

#[test]
fn rolling_back_two_edits_to_one_block_restores_the_original() {
    // The case that distinguishes a correct rollback from a plausible one.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap();
    j.append(alice(), set(0, 64, 0, STONE, DIAMOND)).unwrap();

    let plan = j.plan_rollback(&Filter::everything()).unwrap();
    assert_eq!(plan.len(), 1, "one block, one restoration");
    assert_eq!(
        plan[0].block, DIRT,
        "must restore the state before the first edit"
    );
}

#[test]
fn rolling_back_one_player_leaves_the_others_edits_alone() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap();
    j.append(bob(), set(1, 64, 0, DIRT, DIAMOND)).unwrap();
    j.append(alice(), set(2, 64, 0, DIRT, STONE)).unwrap();

    let plan = j.plan_rollback(&Filter::everything().by(alice())).unwrap();
    let mut xs: Vec<i32> = plan.iter().map(|r| r.x).collect();
    xs.sort_unstable();
    assert_eq!(xs, vec![0, 2]);
}

#[test]
fn a_radius_rollback_is_a_horizontal_cylinder() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap(); // centre
    j.append(alice(), set(4, 200, 0, DIRT, STONE)).unwrap(); // inside, far above
    j.append(alice(), set(0, 64, -5, DIRT, STONE)).unwrap(); // inside, behind
    j.append(alice(), set(20, 64, 0, DIRT, STONE)).unwrap(); // outside

    let plan = j
        .plan_rollback(&Filter::everything().near(0, 64, 0, 5))
        .unwrap();
    let mut got: Vec<(i32, i32, i32)> = plan.iter().map(|r| (r.x, r.y, r.z)).collect();
    got.sort_unstable();
    assert_eq!(got, vec![(0, 64, -5), (0, 64, 0), (4, 200, 0)]);
}

#[test]
fn a_time_limited_rollback_stops_at_the_cutoff() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append_at(alice(), set(0, 64, 0, DIRT, STONE), 1_000)
        .unwrap();
    j.append_at(alice(), set(1, 64, 0, DIRT, STONE), 5_000)
        .unwrap();

    let plan = j
        .plan_rollback(&Filter::everything().since_time(4_000))
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].x, 1);
}

#[test]
fn the_plan_is_ordered_newest_first() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    for i in 0..5 {
        j.append(alice(), set(i, 64, 0, DIRT, STONE)).unwrap();
    }
    let seqs: Vec<u64> = j
        .plan_rollback(&Filter::everything())
        .unwrap()
        .iter()
        .map(|r| r.undoing)
        .collect();
    assert_eq!(seqs, vec![5, 4, 3, 2, 1]);
}

#[test]
fn an_empty_filter_selects_everything_and_a_narrow_one_selects_nothing() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap();
    assert_eq!(j.plan_rollback(&Filter::everything()).unwrap().len(), 1);
    assert!(j
        .plan_rollback(&Filter::everything().by(bob()))
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------

fn mint(uid: u64, to: Place) -> EventBody {
    EventBody::ItemMint {
        uid: ItemUid(uid),
        item: it("minecraft:diamond"),
        count: 1,
        to,
    }
}

#[test]
fn the_ledger_follows_an_item_from_the_giver_to_the_ground_and_back() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let slot_a = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    let ground = Place::Ground {
        x: 10,
        y: 64,
        z: 10,
    };
    let slot_b = Place::Inventory {
        owner: bob(),
        slot: 4,
    };
    j.append(alice(), mint(1, slot_a)).unwrap();
    j.append(
        alice(),
        EventBody::ItemMove {
            uid: ItemUid(1),
            from: slot_a,
            to: ground,
            count: 1,
        },
    )
    .unwrap();
    j.append(
        bob(),
        EventBody::ItemMove {
            uid: ItemUid(1),
            from: ground,
            to: slot_b,
            count: 1,
        },
    )
    .unwrap();

    let l = Ledger::replay(&j.all_events().unwrap());
    assert!(l.anomalies().is_empty(), "{:?}", l.anomalies());
    let inst = l.get(ItemUid(1)).unwrap();
    assert_eq!(inst.at, slot_b);
    assert_eq!(inst.trail.len(), 3, "mint plus two hops");
    assert_eq!(l.inventory_of(bob()).len(), 1);
    assert!(l.inventory_of(alice()).is_empty());
}

#[test]
fn one_uid_taken_from_two_places_is_reported_as_a_dupe() {
    // The shape a real duplication leaves behind: the same instance moves out
    // of a slot it already left. Counting items would see nothing wrong until
    // much later; identity sees it on the second move.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let slot = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    let chest = Place::Container {
        x: 0,
        y: 64,
        z: 0,
        slot: 1,
    };
    let other = Place::Inventory {
        owner: bob(),
        slot: 0,
    };
    j.append(alice(), mint(7, slot)).unwrap();
    j.append(
        alice(),
        EventBody::ItemMove {
            uid: ItemUid(7),
            from: slot,
            to: chest,
            count: 1,
        },
    )
    .unwrap();
    // ...and now taken from the slot it is no longer in.
    j.append(
        bob(),
        EventBody::ItemMove {
            uid: ItemUid(7),
            from: slot,
            to: other,
            count: 1,
        },
    )
    .unwrap();

    let l = Ledger::replay(&j.all_events().unwrap());
    assert_eq!(
        l.anomalies(),
        &[Anomaly::MovedFromElsewhere {
            uid: ItemUid(7),
            believed: chest,
            claimed: slot,
            seq: 3,
        }]
    );
}

#[test]
fn an_item_that_was_never_minted_is_reported() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(
        alice(),
        EventBody::ItemMove {
            uid: ItemUid(42),
            from: Place::Nowhere,
            to: Place::Inventory {
                owner: alice(),
                slot: 0,
            },
            count: 1,
        },
    )
    .unwrap();
    let l = Ledger::replay(&j.all_events().unwrap());
    assert_eq!(
        l.anomalies(),
        &[Anomaly::NeverMinted {
            uid: ItemUid(42),
            seq: 1
        }]
    );
}

#[test]
fn a_uid_minted_twice_is_reported_and_the_first_instance_survives() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let a = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    let b = Place::Inventory {
        owner: bob(),
        slot: 0,
    };
    j.append(alice(), mint(5, a)).unwrap();
    j.append(bob(), mint(5, b)).unwrap();
    let l = Ledger::replay(&j.all_events().unwrap());
    assert_eq!(
        l.anomalies(),
        &[Anomaly::MintedTwice {
            uid: ItemUid(5),
            seq: 2
        }]
    );
    assert_eq!(l.get(ItemUid(5)).unwrap().at, a);
}

#[test]
fn a_destroyed_item_cannot_be_moved_again() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let a = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    j.append(alice(), mint(3, a)).unwrap();
    j.append(
        alice(),
        EventBody::ItemDestroy {
            uid: ItemUid(3),
            from: a,
            reason: DestroyReason::Consumed,
        },
    )
    .unwrap();
    j.append(
        alice(),
        EventBody::ItemMove {
            uid: ItemUid(3),
            from: a,
            to: Place::Ground { x: 0, y: 0, z: 0 },
            count: 1,
        },
    )
    .unwrap();
    let l = Ledger::replay(&j.all_events().unwrap());
    assert_eq!(
        l.anomalies(),
        &[Anomaly::UsedAfterDestroyed {
            uid: ItemUid(3),
            seq: 3
        }]
    );
    assert_eq!(l.get(ItemUid(3)).unwrap().at, Place::Nowhere);
}

#[test]
fn minted_uids_do_not_repeat() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1_000 {
        assert!(seen.insert(ledger::mint_uid()), "uid repeated");
    }
}

#[test]
fn the_allocator_hands_out_strictly_increasing_ids_and_never_zero() {
    let store = std::sync::Arc::new(MemStore::new());
    let a = ledger::UidAllocator::open(std::sync::Arc::clone(&store)).unwrap();
    let mut last = 0u64;
    for _ in 0..1_000 {
        let u = a.next().unwrap().0;
        assert_ne!(u, 0, "id 0 is reserved for 'no id'");
        assert!(u > last, "ids must increase: {u} came after {last}");
        last = u;
    }
}

#[test]
fn the_reservation_is_written_once_per_block_not_per_id() {
    let store = std::sync::Arc::new(MemStore::new());
    let a = ledger::UidAllocator::open(std::sync::Arc::clone(&store)).unwrap();
    for _ in 0..100 {
        a.next().unwrap();
    }
    let bound = u64::from_be_bytes(store.get(b"U").unwrap().unwrap().try_into().unwrap());
    assert_eq!(
        bound,
        ledger::UID_BLOCK,
        "a hundred ids fit in the first block"
    );
}

#[test]
fn the_bound_advances_by_a_whole_block_when_the_counter_reaches_it() {
    let store = std::sync::Arc::new(MemStore::new());
    let a = ledger::UidAllocator::open(std::sync::Arc::clone(&store)).unwrap();
    // The id UID_BLOCK is the first that does not lie below the initial bound,
    // so it is the one that must raise the bound to the next block.
    for _ in 0..=ledger::UID_BLOCK {
        a.next().unwrap();
    }
    let bound = u64::from_be_bytes(store.get(b"U").unwrap().unwrap().try_into().unwrap());
    assert_eq!(bound, 2 * ledger::UID_BLOCK);
}

#[test]
fn a_bound_near_u64_max_fails_loudly_instead_of_wrapping_to_zero() {
    // A corrupted 8-byte bound, or an id at the very top of the range: the
    // next whole block does not fit in a u64. Wrapping would publish 0, the
    // reserved "no id" value; the reservation must error and leave the store
    // untouched rather than hand that out to everything that follows.
    let store = MemStore::new();
    let r = Reservation::new(b"U", ledger::UID_BLOCK, u64::MAX - 1);
    let err = r.ensure(&store, u64::MAX).unwrap_err();
    assert!(
        matches!(err, StorageError::Codec(_)),
        "expected a loud codec error, got {err:?}"
    );
    assert!(
        store.get(b"U").unwrap().is_none(),
        "a failed raise must not write a bound"
    );
}

#[test]
fn a_reopened_allocator_resumes_at_or_after_the_persisted_bound() {
    // The failure this guards against is silent: an allocator that restarted
    // its numbering would hand a new stack the uid of an old one, and the
    // ledger would call the second mint of that uid a dupe long after the
    // fact.
    let store = std::sync::Arc::new(MemStore::new());
    let mut max = 0u64;
    {
        let a = ledger::UidAllocator::open(std::sync::Arc::clone(&store)).unwrap();
        for _ in 0..10_000 {
            max = max.max(a.next().unwrap().0);
        }
    }
    let a = ledger::UidAllocator::open(std::sync::Arc::clone(&store)).unwrap();
    let first = a.next().unwrap().0;
    assert!(
        first > max,
        "reopened allocator returned {first}, at or below an id already used ({max})"
    );
}

#[test]
fn concurrent_allocations_never_duplicate_an_id() {
    let store = std::sync::Arc::new(MemStore::new());
    let a = ledger::UidAllocator::open(std::sync::Arc::clone(&store)).unwrap();
    let per_thread = 2_000;
    let ids = std::sync::Mutex::new(Vec::with_capacity(8 * per_thread));
    std::thread::scope(|s| {
        for _ in 0..8 {
            let a = &a;
            let ids = &ids;
            s.spawn(move || {
                let mut mine = Vec::with_capacity(per_thread);
                for _ in 0..per_thread {
                    mine.push(a.next().unwrap().0);
                }
                ids.lock().unwrap().extend(mine);
            });
        }
    });
    let mut ids = ids.into_inner().unwrap();
    ids.sort_unstable();
    let total = ids.len();
    ids.dedup();
    assert_eq!(total, 8 * per_thread);
    assert_eq!(ids.len(), total, "an id was handed out twice");
}

#[test]
fn a_collapsed_restoration_takes_its_two_halves_from_opposite_ends() {
    // Alice places stone over dirt, then diamond over her own stone. Undoing
    // both must put back DIRT (the oldest `from`) while recovering DIAMOND
    // (the newest `to`) — the block the undo actually removes from the world.
    // Taking both from the same event is the bug this exists to catch.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap();
    j.append(alice(), set(0, 64, 0, STONE, DIAMOND)).unwrap();

    let plan = j.plan_rollback(&Filter::everything()).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(
        plan[0].block, DIRT,
        "restore the state before the first edit"
    );
    assert_eq!(plan[0].removed, DIAMOND, "recover what is actually removed");
}

#[test]
fn recovery_credits_the_rolled_back_player_and_not_whoever_built_on_top() {
    // Alice builds in stone; Bob later replaces her block with diamond.
    // Rolling back *Alice* restores dirt and must credit her with STONE.
    // Crediting the world's current content would hand Alice Bob's diamond,
    // which is a duplication: Bob's diamond came from Bob's inventory.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, DIRT, STONE)).unwrap();
    j.append(bob(), set(0, 64, 0, STONE, DIAMOND)).unwrap();

    let plan = j.plan_rollback(&Filter::everything().by(alice())).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].block, DIRT);
    assert_eq!(
        plan[0].removed, STONE,
        "what Alice placed, not what stands there"
    );
}

#[test]
fn undoing_a_break_recovers_nothing() {
    // The `to` side of a break is air, and air is what the stash refuses.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(0, 64, 0, STONE, BlockStateId::AIR))
        .unwrap();
    let plan = j.plan_rollback(&Filter::everything()).unwrap();
    assert_eq!(plan[0].block, STONE, "the block comes back");
    assert_eq!(plan[0].removed, BlockStateId::AIR, "nothing is owed");
}

#[test]
fn the_last_change_at_a_position_is_the_newest_one() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    j.append(alice(), set(4, 64, 4, DIRT, STONE)).unwrap();
    j.append(bob(), set(4, 64, 4, STONE, DIAMOND)).unwrap();
    j.append(alice(), set(5, 64, 4, DIRT, STONE)).unwrap();

    let e = j.last_change_at(4, 64, 4).unwrap().expect("touched");
    assert_eq!(e.seq, 2, "the newest, not the first");
    assert_eq!(e.actor, bob());
}

#[test]
fn an_untouched_block_reports_no_change_even_in_a_busy_column() {
    // The question a survival server asks before paying for a block: was this
    // generated, or did somebody put it here? A false "touched" costs a player
    // their reward; a false "untouched" lets them mint coins by placing and
    // breaking the same block.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    for y in 0..50 {
        j.append(alice(), set(1, y, 1, DIRT, STONE)).unwrap();
    }
    assert!(j.last_change_at(2, 64, 2).unwrap().is_none());
    assert!(j.last_change_at(1, 49, 1).unwrap().is_some());
}

#[test]
fn a_version_3_blob_still_decodes() {
    // A v3 blob stores the item as a two-byte id and a destroy with no reason.
    // History written before v4 must stay readable for `/inspect` and rollback.
    let mut b = header(3, alice().0);
    b.push(2); // TAG_ITEM_MINT
    b.extend_from_slice(&0x1234u64.to_be_bytes());
    b.extend_from_slice(
        &ItemTypeId::vanilla("minecraft:diamond")
            .unwrap()
            .0
            .to_be_bytes(),
    );
    b.push(6); // count
    b.push(4); // Place::Nowhere
    let e = Event::decode(&b).expect("a v3 mint decodes");
    match e.body {
        EventBody::ItemMint { uid, count, .. } => {
            assert_eq!(uid, ItemUid(0x1234));
            assert_eq!(count, 6);
        }
        other => panic!("{other:?}"),
    }

    let mut d = header(3, alice().0);
    d.push(4); // TAG_ITEM_DESTROY
    d.extend_from_slice(&ItemUid(9).0.to_be_bytes());
    d.push(4); // Place::Nowhere
    let e = Event::decode(&d).expect("a v3 destroy decodes");
    assert_eq!(
        e.body,
        EventBody::ItemDestroy {
            uid: ItemUid(9),
            from: Place::Nowhere,
            reason: DestroyReason::Other,
        },
        "a v3 destroy had no reason byte; it reads as other"
    );
}

#[test]
fn a_version_4_merge_and_destroy_reason_round_trip() {
    for reason in [
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
    ] {
        let e = Event {
            seq: 4,
            at_ms: 5,
            actor: alice(),
            cause: None,
            body: EventBody::ItemDestroy {
                uid: ItemUid(7),
                from: Place::Ground { x: 1, y: 2, z: 3 },
                reason,
            },
        };
        assert_eq!(Event::decode(&e.encode()), Ok(e));
    }

    let m = Event {
        seq: 9,
        at_ms: 10,
        actor: bob(),
        cause: Some(8),
        body: EventBody::ItemMerge {
            into: ItemUid(1),
            from: ItemUid(2),
            count: 3,
        },
    };
    let blob = m.encode();
    assert_eq!(blob[0], event::FORMAT_VERSION);
    assert_eq!(Event::decode(&blob), Ok(m));
}

#[test]
fn a_custom_destroy_reason_round_trips_verbatim() {
    // A reason code this release does not define must come back as the same
    // byte, not folded into `Other`: a plugin's destroy reason, or a core code
    // a newer build added, is information the journal must not lose.
    for code in [event::REASON_CUSTOM_MIN, 200, event::REASON_CUSTOM_MAX] {
        let e = Event {
            seq: 3,
            at_ms: 4,
            actor: alice(),
            cause: None,
            body: EventBody::ItemDestroy {
                uid: ItemUid(7),
                from: Place::Ground { x: 1, y: 2, z: 3 },
                reason: DestroyReason::Custom(code),
            },
        };
        let decoded = Event::decode(&e.encode()).expect("decodes");
        assert_eq!(decoded, e, "code {code}");
        match decoded.body {
            EventBody::ItemDestroy {
                reason: DestroyReason::Custom(c),
                ..
            } => assert_eq!(c, code),
            other => panic!("code {code} came back as {other:?}"),
        }
        // The raw byte is on the wire, and re-encoding reproduces it.
        assert_eq!(e.encode()[e.encode().len() - 1], code);
    }
    // `from_u8` keeps the raw code, and an unassigned core code (11..=127)
    // is preserved too rather than guessed at.
    assert_eq!(DestroyReason::from_u8(200), DestroyReason::Custom(200));
    assert_eq!(DestroyReason::from_u8(77), DestroyReason::Custom(77));
    assert_eq!(DestroyReason::from_u8(77).code(), 77);
    assert_eq!(DestroyReason::from_u8(0), DestroyReason::Other);
}

#[test]
fn every_core_code_sits_in_its_reserved_range() {
    // The code space is split so a plugin's code can never collide with a
    // core one. If a core code drifts into the custom range this fails.
    for (code, _) in EVENT_KINDS {
        assert!(
            (event::TAG_CORE_MIN..=event::TAG_CORE_MAX).contains(code),
            "core event tag {code} is outside {}..={}",
            event::TAG_CORE_MIN,
            event::TAG_CORE_MAX
        );
    }
    for reason in DESTROY_REASONS {
        let code = reason.code();
        assert!(
            (event::REASON_CORE_MIN..=event::REASON_CORE_MAX).contains(&code),
            "core destroy reason {code} is outside {}..={}",
            event::REASON_CORE_MIN,
            event::REASON_CORE_MAX
        );
    }
    assert_eq!(event::TAG_CUSTOM_MIN, event::TAG_CORE_MAX + 1);
    assert_eq!(event::REASON_CUSTOM_MIN, event::REASON_CORE_MAX + 1);
    assert_eq!(
        event::EVENT_KIND_UNKNOWN.0,
        0,
        "0 is the reserved no-tag code"
    );
}

#[test]
fn a_custom_body_round_trips_and_its_payload_is_length_prefixed() {
    // A plugin tag the core does not know must survive a round trip with its
    // payload intact, and a cause behind it must still be readable — the
    // length prefix is what lets a reader skip bytes it does not understand.
    let e = Event {
        seq: 3,
        at_ms: 4,
        actor: alice(),
        cause: Some(2),
        body: EventBody::Custom {
            tag: event::TAG_CUSTOM_MIN + 5,
            bytes: vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00],
        },
    };
    assert_eq!(Event::decode(&e.encode()), Ok(e.clone()));
    assert_eq!(e.body.tag(), event::TAG_CUSTOM_MIN + 5);

    // An empty payload is still a custom body, not a decode error.
    let empty = Event {
        body: EventBody::Custom {
            tag: 200,
            bytes: Vec::new(),
        },
        ..e.clone()
    };
    assert_eq!(Event::decode(&empty.encode()), Ok(empty));
}

#[test]
fn an_unknown_tag_in_the_core_range_is_refused_not_read_as_custom() {
    // Tags 1..=63 are the core's; a tag there this release does not know is a
    // version problem, not a plugin's opaque body.
    let mut b = header(4, alice().0);
    b.push(7); // not a core tag, below TAG_CUSTOM_MIN
    assert_eq!(Event::decode(&b), Err(DecodeError::UnknownTag(7)));
}

#[test]
fn a_derived_item_and_a_cause_survive_a_round_trip() {
    let e = Event {
        seq: 9,
        at_ms: 1_700_000_000_000,
        actor: alice(),
        cause: Some(4),
        body: EventBody::ItemDerive {
            uid: ItemUid(0x42),
            item: it("minecraft:iron_pickaxe"),
            count: 1,
            to: Place::Inventory {
                owner: alice(),
                slot: 36,
            },
            from: vec![ItemUid(7), ItemUid(8)],
        },
    };
    assert_eq!(Event::decode(&e.encode()), Ok(e));
}

#[test]
fn an_entry_written_before_causes_existed_reads_with_none() {
    // The old layout simply ended after the body; the trailer is optional.
    let e = Event {
        seq: 1,
        at_ms: 2,
        actor: bob(),
        cause: None,
        body: set(0, 64, 0, STONE, DIRT),
    };
    let old = e.encode();
    let with_cause = Event {
        cause: Some(77),
        ..e.clone()
    }
    .encode();
    assert_eq!(
        &with_cause[..old.len()],
        &old[..],
        "the cause is a pure suffix"
    );
    assert_eq!(Event::decode(&old).unwrap().cause, None);
    assert_eq!(Event::decode(&with_cause).unwrap().cause, Some(77));
}

#[test]
fn the_ledger_remembers_what_an_item_was_made_from() {
    let inv = |slot| Place::Inventory {
        owner: alice(),
        slot,
    };
    let mint = |seq, uid, item: &str| Event {
        seq,
        at_ms: 0,
        actor: alice(),
        cause: None,
        body: EventBody::ItemMint {
            uid: ItemUid(uid),
            item: it(item),
            count: 1,
            to: inv(seq as i16),
        },
    };
    let events = vec![
        mint(1, 10, "minecraft:stick"),
        mint(2, 11, "minecraft:iron_ingot"),
        Event {
            seq: 3,
            at_ms: 0,
            actor: alice(),
            cause: None,
            body: EventBody::ItemDerive {
                uid: ItemUid(12),
                item: it("minecraft:iron_pickaxe"),
                count: 1,
                to: inv(5),
                from: vec![ItemUid(10), ItemUid(11)],
            },
        },
    ];
    let l = ledger::Ledger::replay(&events);
    let pick = l.get(ItemUid(12)).unwrap();
    assert_eq!(pick.derived_from, vec![ItemUid(10), ItemUid(11)]);
    assert!(l.anomalies().is_empty());
}

#[test]
fn a_merge_moves_its_units_and_empties_the_source() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let a = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    let b = Place::Inventory {
        owner: alice(),
        slot: 1,
    };
    j.append(alice(), mint(1, a)).unwrap();
    j.append(alice(), mint(2, b)).unwrap();
    j.append(
        alice(),
        EventBody::ItemMerge {
            into: ItemUid(1),
            from: ItemUid(2),
            count: 1,
        },
    )
    .unwrap();
    let l = Ledger::replay(&j.all_events().unwrap());
    assert!(l.anomalies().is_empty(), "{:?}", l.anomalies());
    assert_eq!(l.get(ItemUid(1)).unwrap().count, 2, "the units joined it");
    assert_eq!(l.get(ItemUid(1)).unwrap().merged_in, 1);
    let gone = l.get(ItemUid(2)).unwrap();
    assert_eq!(gone.count, 0);
    assert_eq!(gone.at, Place::Nowhere, "a fully merged stack ceases");
    let bal = l.balance();
    assert!(bal.is_balanced(), "{bal:?}");
    assert_eq!(
        (bal.created, bal.alive, bal.destroyed, bal.merged_away),
        (2, 1, 0, 1)
    );
    assert!(l.units_conserved(), "a merge must account for every unit");
}

#[test]
fn a_count_that_grows_in_place_is_not_unit_conserved() {
    // The signature of the live `/give` bug: a stack whose count grew with no
    // mint and no merge to explain it. Instance balance cannot see it (the uid
    // never left), but the per-uid unit count must.
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let slot = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    j.append(alice(), mint(1, slot)).unwrap();
    j.append(
        alice(),
        EventBody::ItemMove {
            uid: ItemUid(1),
            from: slot,
            to: slot,
            count: 5,
        },
    )
    .unwrap();
    let l = Ledger::replay(&j.all_events().unwrap());
    assert!(l.balance().is_balanced(), "the instance never left");
    assert!(
        !l.units_conserved(),
        "four units appeared with no mint or merge"
    );
    assert_eq!(l.get(ItemUid(1)).unwrap().minted, 1);
    assert_eq!(l.get(ItemUid(1)).unwrap().count, 5);
}

#[test]
fn a_merge_from_a_stack_that_was_never_minted_is_reported() {
    let store = MemStore::new();
    let j = Journal::open(&store).unwrap();
    let a = Place::Inventory {
        owner: alice(),
        slot: 0,
    };
    j.append(alice(), mint(1, a)).unwrap();
    j.append(
        alice(),
        EventBody::ItemMerge {
            into: ItemUid(1),
            from: ItemUid(99),
            count: 1,
        },
    )
    .unwrap();
    let l = Ledger::replay(&j.all_events().unwrap());
    assert_eq!(
        l.anomalies(),
        &[Anomaly::NeverMinted {
            uid: ItemUid(99),
            seq: 2
        }]
    );
}

#[test]
fn append_caused_records_the_cause() {
    let j = Journal::open(MemStore::new()).unwrap();
    let brk = j
        .append(alice(), set(1, 2, 3, DIAMOND, BlockStateId::AIR))
        .unwrap();
    let drop = j
        .append_caused(
            alice(),
            EventBody::ItemMint {
                uid: ItemUid(5),
                item: it("minecraft:diamond"),
                count: 1,
                to: Place::Ground { x: 1, y: 2, z: 3 },
            },
            Some(brk),
        )
        .unwrap();
    assert_eq!(j.get(drop).unwrap().unwrap().cause, Some(brk));
    assert_eq!(j.get(brk).unwrap().unwrap().cause, None);
}

#[test]
fn a_malformed_reservation_bound_refuses_to_open_rather_than_restart_numbering() {
    // Read as "absent", a damaged bound would restart the uid counter at 1 and
    // hand every id out again; for the journal it would discard the bound.
    let store = std::sync::Arc::new(MemStore::new());
    store.put(b"U", &[1, 2, 3]).unwrap();
    assert!(ledger::UidAllocator::open(std::sync::Arc::clone(&store)).is_err());
    store.put(b"N", &[0; 5]).unwrap();
    assert!(Journal::open(std::sync::Arc::clone(&store)).is_err());
}

/// A backend that counts the work a query does — proof that a bounded command
/// reads a bounded amount instead of materialising the whole history.
struct Counting<B> {
    inner: B,
    gets: std::sync::atomic::AtomicUsize,
    scans: std::sync::atomic::AtomicUsize,
}

impl<B: KvBackend> Counting<B> {
    fn new(inner: B) -> Self {
        Self {
            inner,
            gets: std::sync::atomic::AtomicUsize::new(0),
            scans: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn reset(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.gets.store(0, SeqCst);
        self.scans.store(0, SeqCst);
    }
    fn get_calls(&self) -> usize {
        self.gets.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn scan_prefix_calls(&self) -> usize {
        self.scans.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl<B: KvBackend> KvBackend for Counting<B> {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, value)
    }
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.get(key)
    }
    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.inner.delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        self.scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.scan_prefix(prefix)
    }
}

#[test]
fn a_bounded_query_over_100k_events_reads_only_what_it_returns() {
    // `/lookup` and `/audit` used to pull every event into a `Vec`; on a long
    // history that is seconds of work and all of it in RAM. The bounded queries
    // must touch a bounded number of entries.
    let store = std::sync::Arc::new(Counting::new(MemStore::new()));
    let j = Journal::open(std::sync::Arc::clone(&store)).unwrap();
    for i in 0..100_000u64 {
        j.append_at(alice(), set(i as i32, 64, 0, DIRT, STONE), i)
            .unwrap();
    }

    store.reset();
    let recent = j.recent_events(10).unwrap();
    assert_eq!(recent.len(), 10);
    assert_eq!(recent.first().unwrap().seq, 99_991);
    assert_eq!(recent.last().unwrap().seq, 100_000);
    assert_eq!(
        store.scan_prefix_calls(),
        0,
        "recent_events scanned the whole event index"
    );
    assert!(
        store.get_calls() <= 10,
        "read {} events to answer for 10",
        store.get_calls()
    );

    // By actor: the index is scanned lazily and only the newest few events
    // are read.
    store.reset();
    let by_actor = j.actor_events_recent(alice(), 5).unwrap();
    assert_eq!(by_actor.len(), 5);
    assert_eq!(by_actor.last().unwrap().seq, 100_000);
    assert!(
        store.get_calls() <= 5,
        "read {} events to answer for 5",
        store.get_calls()
    );
}
