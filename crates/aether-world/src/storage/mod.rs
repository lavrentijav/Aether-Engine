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
    /// Flush any buffered writes to durable storage.
    fn flush(&self) -> Result<(), StorageError> {
        Ok(())
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

    /// Flush buffered writes.
    pub fn flush(&self) -> Result<(), StorageError> {
        self.backend.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockProperties, BlockStateId};

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
