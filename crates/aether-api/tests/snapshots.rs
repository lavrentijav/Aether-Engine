//! Snapshots saved under an older generator are rebuilt, not trusted whole.
//!
//! The live bug: sub-chunks edited while the vanilla generator still produced
//! bare stone were saved whole, and kept loading as bare stone between
//! neighbours the new generator had given grass and trees.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use aether_api::{block_ids as b, BlockStateId, JournalActor, SnapshotPolicy, World};
use aether_world::registry::blocks::props_of_state;
use aether_world::{MemStore, SubChunk};
use aether_worldgen::{ChunkGenerator, GeneratedColumn};

/// A column of `top` over stone: stone below y = 60, then `top` up to 63.
struct Terrain {
    top: BlockStateId,
}

impl ChunkGenerator for Terrain {
    fn generate_column(&self, _cx: i32, _cz: i32) -> GeneratedColumn {
        let mut s3 = SubChunk::default(); // y 48..64
        for y in 0..16 {
            let id = if y < 12 { b::STONE } else { self.top };
            for z in 0..16 {
                for x in 0..16 {
                    s3.set(x, y, z, id, props_of_state(id).unwrap());
                }
            }
        }
        GeneratedColumn {
            sections: vec![(3, s3)],
            biomes: None,
        }
    }
}

fn set(
    world: &World<Arc<MemStore>, Terrain>,
    x: i32,
    y: i32,
    z: i32,
    id: BlockStateId,
    journal: bool,
) {
    let props = props_of_state(id).unwrap();
    if journal {
        world.set_block_by(JournalActor(1), x, y, z, id, props);
    } else {
        world.set_block_id(x, y, z, id, props);
    }
}

fn policy(rebuilt: &Arc<AtomicUsize>) -> SnapshotPolicy {
    let rebuilt = Arc::clone(rebuilt);
    SnapshotPolicy {
        revision: 2,
        // The old generator made nothing but stone and air.
        keep: Box::new(|id| id != b::STONE && id != b::AIR),
        on_rebuild: Box::new(move |_, _| {
            rebuilt.fetch_add(1, Ordering::Relaxed);
        }),
    }
}

#[test]
fn a_stale_snapshot_gets_the_new_terrain_and_keeps_what_players_built() {
    let backend = Arc::new(MemStore::new());

    // Under the old generator: bare stone. A build with a journal entry, one
    // without (made before the journal existed), and a journalled dig.
    {
        let old = World::new(Arc::clone(&backend), Terrain { top: b::STONE });
        set(&old, 1, 62, 1, b::OAK_LOG, true);
        set(&old, 2, 62, 1, b::OAK_LEAVES, false);
        set(&old, 3, 62, 3, b::AIR, true);
        old.flush().unwrap();
    }

    let rebuilt = Arc::new(AtomicUsize::new(0));
    let new = World::new(Arc::clone(&backend), Terrain { top: b::DIRT })
        .with_snapshot_policy(policy(&rebuilt));
    assert_eq!(
        new.get_block(5, 62, 5),
        b::DIRT,
        "the old terrain is replaced"
    );
    assert_eq!(
        new.get_block(5, 50, 5),
        b::STONE,
        "and the unchanged part stays"
    );
    assert_eq!(
        new.get_block(1, 62, 1),
        b::OAK_LOG,
        "a journalled build survives"
    );
    assert_eq!(
        new.get_block(2, 62, 1),
        b::OAK_LEAVES,
        "so does one the journal never saw"
    );
    assert_eq!(new.get_block(3, 62, 3), b::AIR, "and a journalled dig");
    assert_eq!(rebuilt.load(Ordering::Relaxed), 1);
    new.flush().unwrap();
    drop(new);

    // Saved again, stamped: loads whole from now on.
    let again = Arc::new(AtomicUsize::new(0));
    let reopened =
        World::new(backend, Terrain { top: b::DIRT }).with_snapshot_policy(policy(&again));
    assert_eq!(reopened.get_block(5, 62, 5), b::DIRT);
    assert_eq!(reopened.get_block(2, 62, 1), b::OAK_LEAVES);
    assert_eq!(
        again.load(Ordering::Relaxed),
        0,
        "a current snapshot is not rebuilt"
    );
}

#[test]
fn without_a_policy_snapshots_load_whole() {
    let backend = Arc::new(MemStore::new());
    {
        let old = World::new(Arc::clone(&backend), Terrain { top: b::STONE });
        set(&old, 1, 62, 1, b::OAK_LOG, true);
        old.flush().unwrap();
    }
    let new = World::new(backend, Terrain { top: b::DIRT });
    assert_eq!(
        new.get_block(5, 62, 5),
        b::STONE,
        "the snapshot wins, as before"
    );
}
