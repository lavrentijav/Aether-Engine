//! Persistent world storage: a KV backend keyed by [`SubChunkKey`], holding
//! Zstandard-compressed sub-chunk blobs.
//!
//! * [`KvBackend`] abstracts the key/value store. [`MemStore`] is an in-memory
//!   implementation always available; [`FjallStore`] (feature `fjall`) is the
//!   persistent LSM backend named in the roadmap.
//! * Sub-chunk blobs are produced by [`format`] and compressed with Zstandard
//!   when the `zstd` feature is on (raw passthrough otherwise), so the layer is
//!   fully functional even in a dependency-free build.
//! * [`WorldStorage`] ties a backend and the codec together into
//!   `save` / `load` / `delete` over whole sub-chunks.

pub mod format;

use crate::subchunk::SubChunk;
use format::{FormatError, SubChunkKey};
use std::collections::BTreeMap;
use std::sync::RwLock;

/// Errors from the storage layer.
#[derive(Debug)]
pub enum StorageError {
    /// The stored blob could not be decoded.
    Format(FormatError),
    /// Compression / decompression failed.
    Codec(String),
    /// The underlying KV backend failed.
    Backend(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Format(e) => write!(f, "format error: {e}"),
            StorageError::Codec(e) => write!(f, "codec error: {e}"),
            StorageError::Backend(e) => write!(f, "backend error: {e}"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<FormatError> for StorageError {
    fn from(e: FormatError) -> Self {
        StorageError::Format(e)
    }
}

/// A minimal ordered key/value store.
///
/// Keys are the 9-byte encoding of [`SubChunkKey`]; values are compressed
/// sub-chunk blobs. Implementations must be `Send + Sync` so the work-stealing
/// save pool can share one.
pub trait KvBackend: Send + Sync {
    /// Insert or overwrite `key`.
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    /// Fetch `key`, or `None` if absent.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    /// Remove `key` (no error if absent).
    fn delete(&self, key: &[u8]) -> Result<(), StorageError>;
    /// Every entry whose key starts with `prefix`, in ascending key order.
    ///
    /// The journal leans on the ordering: its keys are built so that
    /// big-endian sequence numbers sort chronologically, which is what lets a
    /// rollback walk a player's edits backwards without holding the whole
    /// world's history in memory.
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError>;
    /// Flush any buffered writes to durable storage.
    fn flush(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

/// A shared reference is itself a backend.
///
/// The journal and the sub-chunk store are two views of one keyspace — they
/// use disjoint key prefixes precisely so they can be — and this is what lets
/// both hold the same opened database without an `Arc` in between.
impl<B: KvBackend + ?Sized> KvBackend for std::sync::Arc<B> {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        (**self).put(key, value)
    }
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        (**self).get(key)
    }
    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        (**self).delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        (**self).scan_prefix(prefix)
    }
    fn flush(&self) -> Result<(), StorageError> {
        (**self).flush()
    }
}

impl<B: KvBackend + ?Sized> KvBackend for &B {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        (**self).put(key, value)
    }
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        (**self).get(key)
    }
    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        (**self).delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        (**self).scan_prefix(prefix)
    }
    fn flush(&self) -> Result<(), StorageError> {
        (**self).flush()
    }
}

/// In-memory backend for tests, tooling and `--no-default-features` builds.
#[derive(Default)]
pub struct MemStore {
    map: RwLock<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MemStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }
    /// Number of stored keys.
    pub fn len(&self) -> usize {
        self.map.read().expect("memstore poisoned").len()
    }
    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl KvBackend for MemStore {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.map
            .write()
            .expect("memstore poisoned")
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .map
            .read()
            .expect("memstore poisoned")
            .get(key)
            .cloned())
    }
    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.map.write().expect("memstore poisoned").remove(key);
        Ok(())
    }
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        // `range(prefix..)` then stop at the first key that no longer matches:
        // a BTreeMap's order is byte order, so matching keys are contiguous.
        Ok(self
            .map
            .read()
            .expect("memstore poisoned")
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}

#[cfg(feature = "fjall")]
mod fjall_backend {
    use super::{KvBackend, StorageError};
    use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};
    use std::path::Path;

    /// Persistent LSM KV backend built on [Fjall](https://crates.io/crates/fjall).
    pub struct FjallStore {
        _keyspace: Keyspace,
        partition: PartitionHandle,
    }

    impl FjallStore {
        /// Open (creating if needed) a world store rooted at `path`.
        pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
            let keyspace = Config::new(path)
                .open()
                .map_err(|e| StorageError::Backend(e.to_string()))?;
            let partition = keyspace
                .open_partition("subchunks", PartitionCreateOptions::default())
                .map_err(|e| StorageError::Backend(e.to_string()))?;
            Ok(Self {
                _keyspace: keyspace,
                partition,
            })
        }
    }

    impl KvBackend for FjallStore {
        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
            self.partition
                .insert(key, value)
                .map_err(|e| StorageError::Backend(e.to_string()))
        }
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
            self.partition
                .get(key)
                .map(|opt| opt.map(|slice| slice.to_vec()))
                .map_err(|e| StorageError::Backend(e.to_string()))
        }
        fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
            self.partition
                .remove(key)
                .map_err(|e| StorageError::Backend(e.to_string()))
        }
        fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
            self.partition
                .prefix(prefix)
                .map(|r| {
                    r.map(|(k, v)| (k.to_vec(), v.to_vec()))
                        .map_err(|e| StorageError::Backend(e.to_string()))
                })
                .collect()
        }
        fn flush(&self) -> Result<(), StorageError> {
            self.partition
                .rotate_memtable_and_wait()
                .map_err(|e| StorageError::Backend(e.to_string()))?;
            Ok(())
        }
    }
}

#[cfg(feature = "fjall")]
pub use fjall_backend::FjallStore;

/// Compress a raw blob. Zstandard when the `zstd` feature is on, else identity.
pub fn compress(raw: &[u8]) -> Result<Vec<u8>, StorageError> {
    #[cfg(feature = "zstd")]
    {
        zstd::encode_all(raw, 3).map_err(|e| StorageError::Codec(e.to_string()))
    }
    #[cfg(not(feature = "zstd"))]
    {
        Ok(raw.to_vec())
    }
}

/// Inverse of [`compress`].
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, StorageError> {
    #[cfg(feature = "zstd")]
    {
        zstd::decode_all(data).map_err(|e| StorageError::Codec(e.to_string()))
    }
    #[cfg(not(feature = "zstd"))]
    {
        Ok(data.to_vec())
    }
}

/// Value written under a column marker; bumping it invalidates old markers if
/// the meaning of "generated" ever changes.
const MARKER_VERSION: u8 = 1;

/// Key holding the world's block-name table.
///
/// Three bytes, where sub-chunk keys are nine and column markers ten, so it
/// cannot collide with either.
const REGISTRY_KEY: &[u8] = b"REG";
const REGISTRY_VERSION: u8 = 1;

/// Key for a column-generated marker.
///
/// Ten bytes, where sub-chunk keys are nine, so a marker can never collide
/// with a section however far out the column sits.
fn column_marker_key(cx: i32, cz: i32) -> [u8; 10] {
    let mut k = [0u8; 10];
    k[0] = b'C';
    k[1..5].copy_from_slice(&cx.to_be_bytes());
    k[5..9].copy_from_slice(&cz.to_be_bytes());
    k
}

/// High-level world storage: sub-chunk `save` / `load` over a [`KvBackend`],
/// transparently serializing and (de)compressing blobs.
pub struct WorldStorage<B: KvBackend> {
    backend: B,
}

impl<B: KvBackend> WorldStorage<B> {
    /// Wrap a backend.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Borrow the underlying backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Serialize, compress and store `sc` under `key`.
    pub fn save(&self, key: SubChunkKey, sc: &SubChunk) -> Result<(), StorageError> {
        let raw = format::serialize_subchunk(sc);
        let blob = compress(&raw)?;
        self.backend.put(&key.encode(), &blob)
    }

    /// Load, decompress and deserialize the sub-chunk at `key`, if present.
    pub fn load(&self, key: SubChunkKey) -> Result<Option<SubChunk>, StorageError> {
        match self.backend.get(&key.encode())? {
            None => Ok(None),
            Some(blob) => {
                let raw = decompress(&blob)?;
                Ok(Some(format::deserialize_subchunk(&raw)?))
            }
        }
    }

    /// Delete the sub-chunk at `key`.
    pub fn delete(&self, key: SubChunkKey) -> Result<(), StorageError> {
        self.backend.delete(&key.encode())
    }

    /// Record that column `(cx, cz)` has been generated in full.
    ///
    /// Presence of *some* sub-chunks cannot answer "has this column been
    /// generated?": a column's all-air sections are never stored, and a column
    /// whose first save was interrupted looks the same as a complete one. A
    /// caller that guesses from section presence regenerates half-written
    /// columns and leaves holes in the world, so completion is recorded
    /// explicitly instead.
    pub fn mark_column_generated(&self, cx: i32, cz: i32) -> Result<(), StorageError> {
        self.backend.put(&column_marker_key(cx, cz), &[MARKER_VERSION])
    }

    /// Whether [`Self::mark_column_generated`] has been called for `(cx, cz)`.
    pub fn column_generated(&self, cx: i32, cz: i32) -> Result<bool, StorageError> {
        Ok(self.backend.get(&column_marker_key(cx, cz))?.is_some())
    }

    /// Store the block-name table whose indices the saved sub-chunks use.
    ///
    /// Sub-chunks persist engine block ids, and ids past the seeded set are
    /// handed out in the order names were first interned. Without the table a
    /// reopened world would read those ids against whatever order the new
    /// session happened to intern in, silently turning one block into another.
    pub fn save_registry(&self, names: &[String]) -> Result<(), StorageError> {
        let mut blob = vec![REGISTRY_VERSION];
        blob.extend_from_slice(&(names.len() as u32).to_be_bytes());
        for name in names {
            blob.extend_from_slice(&(name.len() as u32).to_be_bytes());
            blob.extend_from_slice(name.as_bytes());
        }
        self.backend.put(REGISTRY_KEY, &blob)
    }

    /// Read back the table written by [`Self::save_registry`].
    ///
    /// `Ok(None)` means this world has none yet — either it predates the table
    /// or nothing has been saved. A blob that is truncated or of an unknown
    /// version also reads as `None`: the caller then keeps its default
    /// registry, which is right for a world that only ever used seeded blocks
    /// and no worse than guessing for one that did not.
    pub fn load_registry(&self) -> Result<Option<Vec<String>>, StorageError> {
        let Some(blob) = self.backend.get(REGISTRY_KEY)? else {
            return Ok(None);
        };
        if blob.first() != Some(&REGISTRY_VERSION) || blob.len() < 5 {
            return Ok(None);
        }
        let count = u32::from_be_bytes(blob[1..5].try_into().unwrap()) as usize;
        let mut names = Vec::with_capacity(count);
        let mut pos = 5usize;
        for _ in 0..count {
            if pos + 4 > blob.len() {
                return Ok(None);
            }
            let len = u32::from_be_bytes(blob[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            let Some(bytes) = blob.get(pos..pos + len) else {
                return Ok(None);
            };
            let Ok(name) = std::str::from_utf8(bytes) else {
                return Ok(None);
            };
            names.push(name.to_owned());
            pos += len;
        }
        Ok(Some(names))
    }

    /// Flush buffered writes.
    pub fn flush(&self) -> Result<(), StorageError> {
        self.backend.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockProperties, BlockStateId};

    #[test]
    fn column_markers_round_trip_and_never_collide_with_sections() {
        let storage = WorldStorage::new(MemStore::new());
        assert!(!storage.column_generated(4, -7).unwrap());
        storage.mark_column_generated(4, -7).unwrap();
        assert!(storage.column_generated(4, -7).unwrap());
        // Neighbours are unaffected.
        assert!(!storage.column_generated(5, -7).unwrap());
        assert!(!storage.column_generated(4, -6).unwrap());

        // A marker is ten bytes where a sub-chunk key is nine, so no column —
        // however far out — can have a section key equal to any marker key.
        let marker = column_marker_key(i32::MAX, i32::MIN);
        for cy in [i8::MIN, -1, 0, 1, i8::MAX] {
            for (cx, cz) in [(i32::MAX, i32::MIN), (0, 0), (4, -7)] {
                assert_ne!(
                    marker.as_slice(),
                    SubChunkKey::new(cx, cy, cz).encode().as_slice()
                );
            }
        }
    }

    fn sample_subchunk() -> SubChunk {
        let mut sc = SubChunk::new();
        for i in 0..16 {
            sc.set(i, 0, 0, BlockStateId(i as u32 + 1), BlockProperties::SOLID);
        }
        sc.set(
            1,
            1,
            1,
            BlockStateId(55),
            BlockProperties {
                solid: false,
                collision: false,
                redstone: true,
                light_emission: 0,
                light_opacity: 15,
            },
        );
        sc
    }

    #[test]
    fn codec_round_trips() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(10);
        let packed = compress(&data).unwrap();
        assert_eq!(decompress(&packed).unwrap(), data);
    }

    #[test]
    fn save_and_load_via_memstore() {
        let storage = WorldStorage::new(MemStore::new());
        let key = SubChunkKey::new(3, 2, -7);
        let sc = sample_subchunk();
        storage.save(key, &sc).unwrap();

        let loaded = storage.load(key).unwrap().expect("sub-chunk should exist");
        for i in 0..16 {
            assert_eq!(loaded.get(i, 0, 0), BlockStateId(i as u32 + 1));
        }
        assert_eq!(loaded.redstone_mask(), sc.redstone_mask());
        assert_eq!(loaded.solid_mask(), sc.solid_mask());
    }

    #[test]
    fn load_missing_is_none() {
        let storage = WorldStorage::new(MemStore::new());
        assert!(storage.load(SubChunkKey::new(0, 0, 0)).unwrap().is_none());
    }

    #[test]
    fn delete_removes() {
        let storage = WorldStorage::new(MemStore::new());
        let key = SubChunkKey::new(1, 1, 1);
        storage.save(key, &sample_subchunk()).unwrap();
        assert!(storage.load(key).unwrap().is_some());
        storage.delete(key).unwrap();
        assert!(storage.load(key).unwrap().is_none());
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn save_and_load_via_fjall() {
        let dir = std::env::temp_dir().join(format!("aether-fjall-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).unwrap();
        let storage = WorldStorage::new(store);
        let key = SubChunkKey::new(-4, 3, 9);
        let sc = sample_subchunk();
        storage.save(key, &sc).unwrap();
        storage.flush().unwrap();
        let loaded = storage.load(key).unwrap().expect("persisted");
        assert_eq!(loaded.solid_mask(), sc.solid_mask());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
