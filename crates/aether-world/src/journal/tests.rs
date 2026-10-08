//! Tests for the journal and the rollback.
//!
//! The decoders here are written from the layout described in [`super::event`]
//! rather than by calling the encoder, so an encoder bug cannot hide behind a
//! test that shares it.

use super::*;
use crate::storage::MemStore;
use crate::BlockStateId;

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

#[test]
fn an_event_survives_a_round_trip_through_storage() {
    let e = Event {
        seq: 7,
        at_ms: 1_700_000_000_000,
        actor: alice(),
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
        body: set(1, 2, 3, DIRT, STONE),
    };
    let b = e.encode();
    assert_eq!(b[0], event::FORMAT_VERSION);
    assert_eq!(&b[1..9], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(&b[9..17], &[0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);
    assert_eq!(u128::from_be_bytes(b[17..33].try_into().unwrap()), 0xAB);
}

#[test]
fn a_blob_from_another_format_version_is_refused_not_guessed_at() {
    let mut b = Event {
        seq: 1,
        at_ms: 1,
        actor: alice(),
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
        body: EventBody::ItemMint {
            uid: ItemUid(9),
            item: "minecraft:diamond".into(),
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
            body: EventBody::ItemDestroy {
                uid: ItemUid(1),
                from: place,
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
    assert_eq!(seq, 3);
    assert_eq!(j.all_events().unwrap().len(), 3);
}

#[test]
fn a_lost_head_marker_still_resumes_above_the_last_event() {
    let store = MemStore::new();
    {
        let j = Journal::open(&store).unwrap();
        for i in 0..5 {
            j.append(alice(), set(i, 64, 0, DIRT, STONE)).unwrap();
        }
    }
    store.delete(b"H").unwrap();
    let j = Journal::open(&store).unwrap();
    assert_eq!(j.head(), 6, "must resume above the highest stored event");
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

fn mint(uid: u128, to: Place) -> EventBody {
    EventBody::ItemMint {
        uid: ItemUid(uid),
        item: "minecraft:diamond".into(),
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
