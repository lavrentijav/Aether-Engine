//! Sub-chunk storage round-trip throughput: serialize + compress on save,
//! decompress + deserialize on load, through the in-memory KV backend.

use aether_world::{BlockProperties, BlockStateId, MemStore, SubChunk, SubChunkKey, WorldStorage};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A moderately varied sub-chunk (a few palette entries, half-filled).
fn sample_subchunk() -> SubChunk {
    let mut sc = SubChunk::new();
    for y in 0..8 {
        for z in 0..16 {
            for x in 0..16 {
                let id = BlockStateId(1 + ((x + z + y) % 6) as u32);
                sc.set(x, y, z, id, BlockProperties::SOLID);
            }
        }
    }
    sc
}

fn bench_storage(c: &mut Criterion) {
    let sc = sample_subchunk();
    let storage = WorldStorage::new(MemStore::new());
    let key = SubChunkKey::new(0, 0, 0);

    c.bench_function("subchunk_save", |b| {
        b.iter(|| storage.save(black_box(key), black_box(&sc)).unwrap())
    });

    storage.save(key, &sc).unwrap();
    c.bench_function("subchunk_load", |b| {
        b.iter(|| black_box(storage.load(black_box(key)).unwrap()))
    });
}

criterion_group!(benches, bench_storage);
criterion_main!(benches);
