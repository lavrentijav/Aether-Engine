//! Durable scheduled block ticks: the scheduler's pending entries, written to
//! the KV store next to the chunk they belong to.
//!
//! The queues in [`super::scheduler`] live only in memory, so a restart used to
//! drop every pending fall, growth and flow. Vanilla keeps `block_ticks` and
//! `fluid_ticks` in the chunk's NBT; here each chunk's pending entries are one
//! blob under the chunk's own key, written by [`save`] and read back by
//! [`load`] as the chunk materializes (see [`super::blockticks::load_column`]).
//!
//! Keys are ten bytes — a two-byte prefix, the chunk `cx` then `cz`, each
//! big-endian `i32` — so they are never a nine-byte sub-chunk key and never a
//! `C`/`V`/`K`/`REG`/`ITM` meta key. The two prefixes, `BT` and `BF`, keep the
//! block-tick and fluid-tick queues apart.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use aether_world::KvBackend;
use aether_worldgen::ChunkGenerator;

use super::scheduler::Scheduler;

/// A block position, as the queues key them.
pub type Pos = (i32, i32, i32);

/// Key prefix for the scheduled block-tick queue (sand, leaves, crops).
pub const BLOCK_TICKS: &[u8] = aether_api::keys::SCHEDULED_BLOCK_TICKS;
/// Key prefix for the scheduled fluid-tick queue (water, lava).
pub const FLUID_TICKS: &[u8] = aether_api::keys::SCHEDULED_FLUID_TICKS;

/// On-disk format version, bumped if the layout below ever changes.
const VERSION: u8 = 1;
/// Bytes per entry: three `i32` coordinates and the `u64` due tick.
const ENTRY_LEN: usize = 20;

/// The minimal store the queues need. Implemented for the engine's [`World`],
/// and by a map in tests.
///
/// [`World`]: aether_api::World
pub trait TickStore {
    /// The blob stored under `key`, if any.
    fn tick_get(&self, key: &[u8]) -> Option<Vec<u8>>;
    /// Store `value` under `key`.
    fn tick_put(&self, key: &[u8], value: &[u8]);
    /// Remove `key`.
    fn tick_delete(&self, key: &[u8]);
}

/// A shared reference is itself a store, so an `Arc<World>` held by the
/// server's threads can be handed to [`save`] / [`load`] directly.
impl<T: TickStore + ?Sized> TickStore for std::sync::Arc<T> {
    fn tick_get(&self, key: &[u8]) -> Option<Vec<u8>> {
        (**self).tick_get(key)
    }
    fn tick_put(&self, key: &[u8], value: &[u8]) {
        (**self).tick_put(key, value)
    }
    fn tick_delete(&self, key: &[u8]) {
        (**self).tick_delete(key)
    }
}

impl<B: KvBackend, G: ChunkGenerator> TickStore for aether_api::World<B, G> {
    fn tick_get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.get_meta(key).ok().flatten()
    }
    fn tick_put(&self, key: &[u8], value: &[u8]) {
        let _ = self.put_meta(key, value);
    }
    fn tick_delete(&self, key: &[u8]) {
        let _ = self.journal().backend().delete(key);
    }
}

/// The chunk `(cx, cz)` a position falls in. `div_euclid` so a negative
/// coordinate floors towards `-∞`, matching [`aether_api::World::key_of`].
pub fn chunk_of(pos: Pos) -> (i32, i32) {
    (pos.0.div_euclid(16), pos.2.div_euclid(16))
}

/// The ten-byte key for a chunk's queue under `prefix`.
fn key(prefix: &[u8], cx: i32, cz: i32) -> [u8; 10] {
    let mut k = [0u8; 10];
    k[..2].copy_from_slice(prefix);
    k[2..6].copy_from_slice(&cx.to_be_bytes());
    k[6..10].copy_from_slice(&cz.to_be_bytes());
    k
}

/// Serialize one chunk's pending entries.
fn encode(entries: &[(Pos, u64)]) -> Vec<u8> {
    let mut b = Vec::with_capacity(5 + entries.len() * ENTRY_LEN);
    b.push(VERSION);
    b.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for &((x, y, z), at) in entries {
        b.extend_from_slice(&x.to_be_bytes());
        b.extend_from_slice(&y.to_be_bytes());
        b.extend_from_slice(&z.to_be_bytes());
        b.extend_from_slice(&at.to_be_bytes());
    }
    b
}

/// Read back what [`encode`] wrote; a malformed or truncated blob yields the
/// entries that were intact, never a panic — a damaged tick table must not
/// stop a world from opening.
fn decode(blob: &[u8]) -> Vec<(Pos, u64)> {
    if blob.len() < 5 || blob[0] != VERSION {
        return Vec::new();
    }
    let count = u32::from_be_bytes(blob[1..5].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(count.min((blob.len() - 5) / ENTRY_LEN));
    let mut at = 5usize;
    for _ in 0..count {
        let Some(e) = blob.get(at..at + ENTRY_LEN) else {
            break;
        };
        let x = i32::from_be_bytes(e[0..4].try_into().unwrap());
        let y = i32::from_be_bytes(e[4..8].try_into().unwrap());
        let z = i32::from_be_bytes(e[8..12].try_into().unwrap());
        let due = u64::from_be_bytes(e[12..20].try_into().unwrap());
        out.push(((x, y, z), due));
        at += ENTRY_LEN;
    }
    out
}

/// Chunks whose stored ticks are held in memory now, per prefix: loaded by
/// [`load`], or written by [`save`]. Only these may have their blob deleted —
/// a chunk not loaded since the start still has its ticks only on disk, and
/// an absent queue entry says nothing about it.
fn owned() -> &'static Mutex<HashMap<Vec<u8>, HashSet<(i32, i32)>>> {
    static O: OnceLock<Mutex<HashMap<Vec<u8>, HashSet<(i32, i32)>>>> = OnceLock::new();
    O.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Persist `entries` per chunk under `prefix`.
///
/// A chunk with pending entries gets its blob rewritten. A chunk whose ticks
/// are in memory (see [`owned`]) but has no entries left has its blob
/// deleted, so a consumed tick is not resurrected after the next restart. A
/// chunk not loaded since the start keeps its blob untouched.
pub fn save(store: &dyn TickStore, prefix: &[u8], entries: Vec<(u64, Pos)>) {
    let mut groups: BTreeMap<(i32, i32), Vec<(Pos, u64)>> = BTreeMap::new();
    for (at, pos) in entries {
        groups.entry(chunk_of(pos)).or_default().push((pos, at));
    }
    for (&(cx, cz), list) in &groups {
        store.tick_put(&key(prefix, cx, cz), &encode(list));
    }
    let mut owned = owned().lock().unwrap();
    let mine = owned.entry(prefix.to_vec()).or_default();
    mine.retain(|&(cx, cz)| {
        if groups.contains_key(&(cx, cz)) {
            return true;
        }
        store.tick_delete(&key(prefix, cx, cz));
        false
    });
    mine.extend(groups.keys().copied());
}

/// The entries stored for the chunk `(cx, cz)` under `prefix`.
pub fn load(store: &dyn TickStore, prefix: &[u8], cx: i32, cz: i32) -> Vec<(Pos, u64)> {
    owned()
        .lock()
        .unwrap()
        .entry(prefix.to_vec())
        .or_default()
        .insert((cx, cz));
    store
        .tick_get(&key(prefix, cx, cz))
        .map(|b| decode(&b))
        .unwrap_or_default()
}

/// Put `entries` back into `scheduler`, unless the key is already pending.
pub fn restore(scheduler: &mut Scheduler<Pos>, entries: Vec<(Pos, u64)>) {
    for (pos, at) in entries {
        scheduler.schedule(pos, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A [`TickStore`] over a map, standing in for the world's KV backend.
    #[derive(Default)]
    struct Fake(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

    impl TickStore for Fake {
        fn tick_get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn tick_put(&self, key: &[u8], value: &[u8]) {
            self.0.lock().unwrap().insert(key.to_vec(), value.to_vec());
        }
        fn tick_delete(&self, key: &[u8]) {
            self.0.lock().unwrap().remove(key);
        }
    }

    #[test]
    fn a_chunk_blob_round_trips() {
        let entries = vec![((3, 5, 7), 10u64), ((-1, 0, 20), 12u64)];
        assert_eq!(decode(&encode(&entries)), entries);
    }

    #[test]
    fn a_damaged_blob_yields_no_entries_rather_than_panicking() {
        assert!(decode(&[]).is_empty());
        assert!(decode(&[2, 0, 0, 0, 1]).is_empty(), "unknown version");
        // A count that promises more than the blob holds yields the intact
        // prefix, never an out-of-bounds read.
        let mut blob = encode(&[((1, 2, 3), 4)]);
        blob[1..5].copy_from_slice(&9u32.to_be_bytes());
        assert_eq!(decode(&blob), vec![((1, 2, 3), 4)]);
    }

    #[test]
    fn a_key_is_ten_bytes_and_cannot_be_a_subchunk_key() {
        let k = key(BLOCK_TICKS, i32::MAX, i32::MIN);
        assert_eq!(k.len(), 10);
        assert_eq!(&k[..2], BLOCK_TICKS);
        assert_ne!(
            k.as_slice(),
            aether_api::SubChunkKey::new(i32::MAX, 0, i32::MIN)
                .encode()
                .as_slice()
        );
    }

    #[test]
    fn saving_groups_entries_by_chunk_and_removes_consumed_ones() {
        let store = Fake::default();
        save(
            &store,
            BLOCK_TICKS,
            vec![(10, (3, 5, 7)), (12, (-1, 0, 20))],
        );
        assert_eq!(load(&store, BLOCK_TICKS, 0, 0), vec![((3, 5, 7), 10)]);
        assert_eq!(load(&store, BLOCK_TICKS, -1, 1), vec![((-1, 0, 20), 12)]);
        assert_eq!(load(&store, BLOCK_TICKS, 2, 2), vec![], "another chunk");

        // The (0, 0) tick runs; the next save must not leave its blob behind
        // to be replayed after a restart.
        save(&store, BLOCK_TICKS, vec![(12, (-1, 0, 20))]);
        assert_eq!(load(&store, BLOCK_TICKS, 0, 0), vec![]);
        assert_eq!(load(&store, BLOCK_TICKS, -1, 1), vec![((-1, 0, 20), 12)]);
    }

    #[test]
    fn a_save_keeps_the_ticks_of_a_chunk_not_loaded_since_the_start() {
        // Distinct coordinates: the owned set is process-wide.
        let store = Fake::default();
        store.tick_put(
            &key(BLOCK_TICKS, 500, 500),
            &encode(&[((8000, 1, 8000), 40)]),
        );
        // Restart: nothing loaded yet, an autosave with an unrelated entry.
        save(&store, BLOCK_TICKS, vec![(41, (8100, 1, 8100))]);
        assert_eq!(
            load(&store, BLOCK_TICKS, 500, 500),
            vec![((8000, 1, 8000), 40)],
            "the unloaded chunk's tick must survive the save"
        );
        // Now loaded (owned) and consumed: the next save deletes it.
        save(&store, BLOCK_TICKS, vec![(41, (8100, 1, 8100))]);
        assert_eq!(load(&store, BLOCK_TICKS, 500, 500), vec![]);
    }
}
