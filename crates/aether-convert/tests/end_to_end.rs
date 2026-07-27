//! End-to-end conversion test: synthesise a real Anvil region file (zlib-
//! compressed NBT), run the full converter, and read the blocks back out of the
//! KV store.

use aether_convert::convert::convert_world;
use aether_world::{BlockStateId, MemStore, SubChunkKey, WorldStorage};

/// Minimal NBT byte writer — enough to emit one chunk.
#[derive(Default)]
struct NbtWriter {
    buf: Vec<u8>,
}

impl NbtWriter {
    fn name(&mut self, s: &str) {
        self.buf.extend_from_slice(&(s.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(s.as_bytes());
    }
    fn byte_field(&mut self, name: &str, v: i8) {
        self.buf.push(1);
        self.name(name);
        self.buf.push(v as u8);
    }
    fn string_field(&mut self, name: &str, v: &str) {
        self.buf.push(8);
        self.name(name);
        self.name(v);
    }
    fn end(&mut self) {
        self.buf.push(0);
    }
}

/// Build the NBT bytes for a chunk with a single section: palette
/// `[air, stone]`, `data` selecting stone at block index 0 (local 0,0,0).
fn build_chunk_nbt() -> Vec<u8> {
    let mut w = NbtWriter::default();
    // root compound, empty name
    w.buf.push(10);
    w.name("");

    // "sections": list of compounds, length 1
    w.buf.push(9);
    w.name("sections");
    w.buf.push(10); // element type compound
    w.buf.extend_from_slice(&1i32.to_be_bytes());

    // -- section compound (no name, it's a list element)
    w.byte_field("Y", 0);

    // "block_states" compound
    w.buf.push(10);
    w.name("block_states");

    // "palette": list of compounds, length 2
    w.buf.push(9);
    w.name("palette");
    w.buf.push(10);
    w.buf.extend_from_slice(&2i32.to_be_bytes());
    // palette[0] = air
    w.string_field("Name", "minecraft:air");
    w.end();
    // palette[1] = stone
    w.string_field("Name", "minecraft:stone");
    w.end();

    // "data": long array, 256 longs (bits=4 -> 16 per long -> 4096/16=256)
    w.buf.push(12);
    w.name("data");
    let mut data = vec![0i64; 256];
    data[0] = 1; // block index 0 -> palette index 1 (stone)
    w.buf.extend_from_slice(&(data.len() as i32).to_be_bytes());
    for v in data {
        w.buf.extend_from_slice(&v.to_be_bytes());
    }

    w.end(); // end block_states
    w.end(); // end section compound
    w.end(); // end root
    w.buf
}

/// Wrap chunk NBT into a one-chunk Anvil region file (chunk stored at sector 2).
fn build_region_file(chunk_nbt: &[u8]) -> Vec<u8> {
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(chunk_nbt, 6);

    let mut file = vec![0u8; 8192]; // header (locations + timestamps)
                                    // Location entry for local chunk (0,0): 3-byte sector offset + 1-byte count.
    file[0] = 0;
    file[1] = 0;
    file[2] = 2; // sector offset 2 -> byte 8192
    file[3] = 1; // one sector

    // Chunk payload frame: 4-byte length, 1-byte compression, data.
    let length = (compressed.len() + 1) as u32;
    file.extend_from_slice(&length.to_be_bytes());
    file.push(2); // zlib
    file.extend_from_slice(&compressed);
    // Pad to a 4 KiB sector boundary (Anvil convention; not required by reader).
    while file.len() % 4096 != 0 {
        file.push(0);
    }
    file
}

#[test]
fn converts_synthetic_region_into_kv_store() {
    let dir = std::env::temp_dir().join(format!("aether-e2e-{}", std::process::id()));
    let region_dir = dir.join("region");
    std::fs::create_dir_all(&region_dir).unwrap();
    std::fs::write(
        region_dir.join("r.0.0.mca"),
        build_region_file(&build_chunk_nbt()),
    )
    .unwrap();

    let storage = WorldStorage::new(MemStore::new());
    let report = convert_world(&dir, &storage, 2).unwrap();

    assert_eq!(report.regions, 1, "one region processed");
    assert_eq!(report.chunks, 1, "one chunk decoded");
    assert_eq!(report.sections, 1, "one non-empty sub-chunk saved");
    assert!(report.is_clean(), "unexpected errors: {:?}", report.errors);
    assert_ne!(report.checksum, 0, "audit checksum should be populated");

    // Chunk (0,0), section Y=0 -> global chunk (0,0).
    let loaded = storage
        .load(SubChunkKey::new(0, 0, 0))
        .unwrap()
        .expect("sub-chunk should have been written");
    // Block (0,0,0) is stone (non-air); its neighbour is air.
    assert_ne!(loaded.get(0, 0, 0), BlockStateId::AIR);
    assert_eq!(loaded.get(1, 0, 0), BlockStateId::AIR);
    assert_eq!(loaded.solid_mask().count(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}
