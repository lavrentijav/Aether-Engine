//! Columns leave memory when nobody needs them, and come back unchanged.
//!
//! Without this the cache only ever grew: every column anyone had loaded
//! stayed until a restart, players or not.

use std::sync::Arc;

use aether_api::{block_ids as b, JournalActor, NoiseGenerator, World};
use aether_world::registry::blocks::props_of_state;
use aether_world::MemStore;

type W = World<Arc<MemStore>, NoiseGenerator>;

fn world(backend: &Arc<MemStore>) -> W {
    World::new(Arc::clone(backend), NoiseGenerator::new(7))
}

fn touch(world: &W, cx: i32, cz: i32) {
    let _ = world.get_block(cx * 16, 0, cz * 16);
}

fn place(world: &W, x: i32, z: i32) {
    let id = b::OAK_LOG;
    world.set_block_by(JournalActor(1), x, 120, z, id, props_of_state(id).unwrap());
}

#[test]
fn an_unneeded_column_unloads_once_idle_and_reloads_as_it_was() {
    let backend = Arc::new(MemStore::new());
    let world = world(&backend);
    for cx in 0..4 {
        touch(&world, cx, 0);
    }
    place(&world, 3 * 16 + 2, 5);
    world.flush().unwrap();
    let before: Vec<_> = (0..64).map(|y| world.get_block(3 * 16 + 1, y, 1)).collect();
    assert_eq!(world.resident_columns(), 4);

    let keep = |cx: i32, _cz: i32| cx < 2;
    // Touched during the sweep just past: not idle yet.
    assert_eq!(world.unload(&keep, 0, 1).unneeded, 0);
    let s = world.unload(&keep, 0, 1);
    assert_eq!(s.unneeded, 2, "columns 2 and 3 are idle and unneeded");
    assert_eq!(s.resident, 2);

    // Back exactly as it was: terrain, and the saved edit.
    assert_eq!(world.get_block(3 * 16 + 2, 120, 5), b::OAK_LOG);
    let after: Vec<_> = (0..64).map(|y| world.get_block(3 * 16 + 1, y, 1)).collect();
    assert_eq!(before, after);
    assert_eq!(world.resident_columns(), 3);
}

#[test]
fn an_unsaved_edit_keeps_its_column_until_it_is_flushed() {
    let backend = Arc::new(MemStore::new());
    let world = world(&backend);
    place(&world, 5, 5);
    let nothing = |_: i32, _: i32| false;
    world.unload(&nothing, 0, 0);
    assert_eq!(
        world.unload(&nothing, 0, 0).resident,
        1,
        "a dirty column is never dropped"
    );
    world.flush().unwrap();
    assert_eq!(world.unload(&nothing, 0, 0).resident, 0);
    assert_eq!(world.get_block(5, 120, 5), b::OAK_LOG);
}

#[test]
fn an_edit_after_the_last_flush_is_replayed_from_the_journal() {
    // Unloading needs a clean column, so this can only arise across a
    // restart — but the journal tail must replay after an unload too, which
    // it does because the reload is the same path.
    let backend = Arc::new(MemStore::new());
    {
        let world = world(&backend);
        place(&world, 1, 1);
        world.flush().unwrap();
        place(&world, 2, 1);
    }
    let world = world(&backend);
    assert_eq!(world.get_block(2, 120, 1), b::OAK_LOG);
    world.flush().unwrap();
    let nothing = |_: i32, _: i32| false;
    world.unload(&nothing, 0, 0);
    world.unload(&nothing, 0, 0);
    assert_eq!(world.resident_columns(), 0);
    assert_eq!(world.get_block(1, 120, 1), b::OAK_LOG);
    assert_eq!(world.get_block(2, 120, 1), b::OAK_LOG);
}

#[test]
fn over_budget_the_least_recently_used_go_first() {
    let backend = Arc::new(MemStore::new());
    let world = world(&backend);
    let everything = |_: i32, _: i32| true;
    for cx in 0..6 {
        touch(&world, cx, 0);
        // A sweep between touches gives each column its own age.
        world.unload(&everything, 0, 0);
    }
    // Column 0, the oldest, is used again: now the youngest.
    touch(&world, 0, 0);
    let s = world.unload(&everything, 3, 0);
    assert_eq!(s.over_budget, 3);
    assert_eq!(s.resident, 3);
    // 1, 2 and 3 went; touching 0, 4 and 5 loads nothing new.
    for cx in [0, 4, 5] {
        touch(&world, cx, 0);
    }
    assert_eq!(world.resident_columns(), 3);
}

#[test]
fn a_resident_column_is_a_few_kilobytes() {
    let backend = Arc::new(MemStore::new());
    let world = world(&backend);
    for cx in 0..8 {
        for cz in 0..8 {
            touch(&world, cx, cz);
        }
    }
    let per_column = world.resident_bytes() / world.resident_columns();
    eprintln!(
        "{} sections in 64 columns, {per_column} bytes per column",
        world.resident_sections()
    );
    assert!(per_column < 20_000, "{per_column} bytes per column");
}
