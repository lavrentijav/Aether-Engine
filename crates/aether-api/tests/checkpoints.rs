//! A column's journal checkpoint: loading replays only the events after it.
//!
//! Without one, every load of a column re-read its whole history — the cost of
//! opening a column grew with the age of the world, not with what changed
//! since it was last saved.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use aether_api::{block_ids as b, JournalActor, NoiseGenerator, World};
use aether_world::registry::blocks::props_of_state;
use aether_world::{KvBackend, MemStore, StorageError};

/// A store that counts reads of journal events (`E` + an eight-byte number).
#[derive(Default)]
struct Counting {
    inner: MemStore,
    event_reads: AtomicUsize,
}

impl KvBackend for Counting {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, value)
    }
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        if key.len() == 9 && key[0] == b'E' {
            self.event_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.get(key)
    }
    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.inner.delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        self.inner.scan_prefix(prefix)
    }
}

fn put(world: &World<Arc<Counting>, NoiseGenerator>, x: i32, z: i32, id: aether_api::BlockStateId) {
    world.set_block_by(JournalActor(1), x, 120, z, id, props_of_state(id).unwrap());
}

#[test]
fn a_load_replays_only_the_events_after_the_checkpoint() {
    let backend = Arc::new(Counting::default());
    {
        let world = World::new(Arc::clone(&backend), NoiseGenerator::new(7));
        // A long history in one column, saved...
        for i in 0..1000 {
            let id = if i % 2 == 0 { b::OAK_LOG } else { b::SAND };
            put(&world, i % 16, i / 16 % 16, id);
        }
        world.flush().unwrap();
        // ...then three more edits the crash takes before any flush.
        put(&world, 0, 0, b::OAK_LEAVES);
        put(&world, 1, 0, b::OAK_LEAVES);
        put(&world, 2, 0, b::OAK_LEAVES);
    }

    backend.event_reads.store(0, Ordering::SeqCst);
    let world = World::new(Arc::clone(&backend), NoiseGenerator::new(7));
    assert_eq!(
        world.get_block(0, 120, 0),
        b::OAK_LEAVES,
        "the unsaved tail is replayed"
    );
    assert_eq!(world.get_block(2, 120, 0), b::OAK_LEAVES);
    assert_eq!(
        world.get_block(3, 120, 0),
        b::SAND,
        "the saved history is in the snapshot"
    );
    assert_eq!(
        backend.event_reads.load(Ordering::SeqCst),
        3,
        "only the three events after the checkpoint are read, not 1003"
    );
}

#[test]
fn a_column_saved_before_checkpoints_replays_its_whole_history() {
    // No checkpoint key at all reads as "from the beginning".
    let backend = Arc::new(Counting::default());
    {
        let world = World::new(Arc::clone(&backend), NoiseGenerator::new(7));
        put(&world, 5, 5, b::OAK_LOG);
        put(&world, 6, 5, b::SAND);
    }
    backend.event_reads.store(0, Ordering::SeqCst);
    let world = World::new(Arc::clone(&backend), NoiseGenerator::new(7));
    assert_eq!(world.get_block(5, 120, 5), b::OAK_LOG);
    assert_eq!(world.get_block(6, 120, 5), b::SAND);
    assert_eq!(backend.event_reads.load(Ordering::SeqCst), 2);
}
