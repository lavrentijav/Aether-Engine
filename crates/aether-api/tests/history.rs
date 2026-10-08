//! End-to-end checks of the baseline-plus-journal world model.
//!
//! These go through the public [`World`] facade rather than the journal
//! directly, because the claims being tested are about the world as a whole:
//! that touching it costs nothing, that editing it costs one entry, and that a
//! rollback puts back exactly what was there.

use aether_api::{JournalActor, World};
use aether_world::journal::Filter;
use aether_world::{KvBackend, MemStore, StorageError};
use aether_worldgen::NoiseGenerator;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A store that counts writes, so "nothing was stored" can be asserted rather
/// than assumed.
#[derive(Default)]
struct Counting {
    inner: MemStore,
    puts: AtomicUsize,
}

impl Counting {
    fn puts(&self) -> usize {
        self.puts.load(Ordering::SeqCst)
    }
}

impl KvBackend for Counting {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.inner.put(key, value)
    }
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get(key)
    }
    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.inner.delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        self.inner.scan_prefix(prefix)
    }
}

fn alice() -> JournalActor {
    JournalActor(1)
}

#[test]
fn walking_across_an_untouched_world_writes_nothing() {
    // The whole point of the baseline model. Before it, every column a player
    // saw was serialized and stored, whether or not anyone had changed it.
    let counter = std::sync::Arc::new(Counting::default());
    let world = World::new(std::sync::Arc::clone(&counter), NoiseGenerator::new(42));
    for cx in -3..=3 {
        for cz in -3..=3 {
            world.get_block(cx * 16, 64, cz * 16);
        }
    }
    world.flush().unwrap();
    // `flush` writes the block-name table, and nothing else may join it.
    assert!(
        counter.puts() <= 1,
        "an untouched world stored {} blob(s) across 49 columns",
        counter.puts()
    );
}

#[test]
fn a_generated_column_reads_back_identically_without_being_stored() {
    // The baseline is only free if it is reproducible. Two worlds on the same
    // seed must agree block for block, or "regenerate instead of storing"
    // silently loses terrain.
    let a = World::new(MemStore::new(), NoiseGenerator::new(42));
    let b = World::new(MemStore::new(), NoiseGenerator::new(42));
    for y in (0..128).step_by(7) {
        for x in [-33, -1, 0, 15, 47] {
            assert_eq!(a.get_block(x, y, x), b.get_block(x, y, x), "at {x},{y}");
        }
    }
}

#[test]
fn an_edit_survives_a_reopen_of_the_same_backend() {
    use std::sync::Arc;
    let backend = Arc::new(MemStore::new());
    let stone = {
        let world = World::new(Arc::clone(&backend), NoiseGenerator::new(42));
        let (id, props) = world.intern_block("minecraft:stone");
        world.set_block_by(alice(), 5, 90, 5, id, props);
        world.flush().unwrap();
        id
    };
    let reopened = World::new(backend, NoiseGenerator::new(42));
    assert_eq!(reopened.get_block(5, 90, 5), stone);
}

#[test]
fn an_edit_survives_a_reopen_even_when_nothing_was_ever_flushed() {
    // The crash case: the journal is written on every edit, the sub-chunk
    // snapshot only on flush. Replaying the journal over the baseline is what
    // turns a crash from "lost the last thirty seconds" into "lost nothing".
    use std::sync::Arc;
    let backend = Arc::new(MemStore::new());
    let stone = {
        let world = World::new(Arc::clone(&backend), NoiseGenerator::new(42));
        let (id, props) = world.intern_block("minecraft:stone");
        world.set_block_by(alice(), 5, 90, 5, id, props);
        id // no flush: simulate the process dying here
    };
    let reopened = World::new(backend, NoiseGenerator::new(42));
    assert_eq!(reopened.get_block(5, 90, 5), stone);
}

#[test]
fn a_rollback_restores_the_generated_terrain_it_replaced() {
    let world = World::new(MemStore::new(), NoiseGenerator::new(42));
    let before = world.get_block(9, 70, 9);
    let (stone, props) = world.intern_block("minecraft:stone");
    assert_ne!(before, stone, "pick a coordinate the generator left alone");

    world.set_block_by(alice(), 9, 70, 9, stone, props);
    assert_eq!(world.get_block(9, 70, 9), stone);

    let plan = world
        .rollback(JournalActor::SERVER, &Filter::everything().by(alice()))
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(world.get_block(9, 70, 9), before);
}

#[test]
fn a_rollback_is_itself_recorded_and_can_be_rolled_back() {
    // History is appended to, never rewritten — so an operator who rolls back
    // the wrong player can undo that too.
    let world = World::new(MemStore::new(), NoiseGenerator::new(42));
    let original = world.get_block(11, 71, 11);
    let (stone, props) = world.intern_block("minecraft:stone");
    world.set_block_by(alice(), 11, 71, 11, stone, props);

    let head_before = world.journal().head();
    world
        .rollback(JournalActor::SERVER, &Filter::everything().by(alice()))
        .unwrap();
    assert!(
        world.journal().head() > head_before,
        "the rollback must leave a trace of its own"
    );
    assert_eq!(world.get_block(11, 71, 11), original);

    // Undo the undo, attributed to the server, and the player's block is back.
    world
        .rollback(
            JournalActor::SERVER,
            &Filter::everything().by(JournalActor::SERVER),
        )
        .unwrap();
    assert_eq!(world.get_block(11, 71, 11), stone);
}

#[test]
fn a_rollback_of_one_player_leaves_the_others_work_standing() {
    let world = World::new(MemStore::new(), NoiseGenerator::new(42));
    let bob = JournalActor(2);
    let (stone, sp) = world.intern_block("minecraft:stone");
    let (glass, gp) = world.intern_block("minecraft:glass");
    world.set_block_by(alice(), 20, 72, 20, stone, sp);
    world.set_block_by(bob, 21, 72, 20, glass, gp);

    world
        .rollback(JournalActor::SERVER, &Filter::everything().by(alice()))
        .unwrap();
    assert_ne!(world.get_block(20, 72, 20), stone, "alice's block is gone");
    assert_eq!(world.get_block(21, 72, 20), glass, "bob's block stands");
}

#[test]
fn setting_a_block_to_what_it_already_is_records_nothing() {
    // A no-op event would still be undone by a rollback, quietly reverting a
    // later edit by someone else to the same block.
    let world = World::new(MemStore::new(), NoiseGenerator::new(42));
    let existing = world.get_block(0, 60, 0);
    let props = world.props_of(existing);
    let head = world.journal().head();
    assert_eq!(world.set_block_by(alice(), 0, 60, 0, existing, props), None);
    assert_eq!(world.journal().head(), head);
}
