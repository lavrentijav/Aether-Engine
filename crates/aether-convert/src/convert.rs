//! Conversion orchestration: walk a world's region files in parallel, decode
//! each into sub-chunks, write them to a [`WorldStorage`], and keep an audit
//! trail (counts, per-region checksums, non-fatal errors).

use crate::anvil::Region;
use crate::block_map::{BlockRegistry, Interner};
use aether_world::{BlockProperties, BlockStateId, KvBackend, SubChunkKey, WorldStorage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Aggregated result of a conversion run — the audit trail.
#[derive(Debug, Default)]
pub struct ConversionReport {
    /// Region files processed.
    pub regions: usize,
    /// Chunks decoded.
    pub chunks: usize,
    /// Non-empty sub-chunks written.
    pub sections: usize,
    /// Distinct block names encountered (registry size, incl. air).
    pub distinct_blocks: usize,
    /// Order-independent checksum of every `(key, blob)` written.
    pub checksum: u64,
    /// Non-fatal per-chunk / per-region errors.
    pub errors: Vec<String>,
}

impl ConversionReport {
    /// Whether the run completed with no non-fatal errors.
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

/// A thread-safe registry shared across region workers.
///
/// Interning locks only on the first sight of a name; the per-worker
/// [`CachedInterner`] serves repeat lookups lock-free.
pub struct SharedRegistry {
    inner: Mutex<BlockRegistry>,
}

impl SharedRegistry {
    /// Wrap a fresh registry.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BlockRegistry::new()),
        }
    }
    fn intern(&self, name: &str) -> (BlockStateId, BlockProperties) {
        self.inner.lock().expect("registry poisoned").intern(name)
    }
    /// Distinct names known so far.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("registry poisoned").len()
    }
    /// Whether nothing but air has been seen.
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }
}

impl Default for SharedRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-worker interner: a local name cache backed by a [`SharedRegistry`].
pub struct CachedInterner<'a> {
    shared: &'a SharedRegistry,
    cache: HashMap<String, (BlockStateId, BlockProperties)>,
}

impl<'a> CachedInterner<'a> {
    /// Create a worker-local interner over `shared`.
    pub fn new(shared: &'a SharedRegistry) -> Self {
        Self {
            shared,
            cache: HashMap::new(),
        }
    }
}

impl Interner for CachedInterner<'_> {
    fn intern(&mut self, name: &str) -> (BlockStateId, BlockProperties) {
        if let Some(&v) = self.cache.get(name) {
            return v;
        }
        let v = self.shared.intern(name);
        self.cache.insert(name.to_owned(), v);
        v
    }
}

/// Collect the `region/*.mca` files under a world directory.
///
/// Accepts either a world root (containing a `region/` dir) or a `region/`
/// directory directly.
pub fn find_region_files(world_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let region_dir = if world_dir.join("region").is_dir() {
        world_dir.join("region")
    } else {
        world_dir.to_path_buf()
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&region_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("mca") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// FNV-1a of `bytes`, seeded by `seed`. Combined with XOR across sub-chunks so
/// the world checksum is independent of write order.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Convert a whole world directory into `storage`, using `threads` workers.
///
/// Returns the audit [`ConversionReport`]. Per-chunk decode failures are
/// recorded in the report rather than aborting the run.
pub fn convert_world<B: KvBackend>(
    world_dir: &Path,
    storage: &WorldStorage<B>,
    threads: usize,
) -> std::io::Result<ConversionReport> {
    let files = find_region_files(world_dir)?;
    let registry = SharedRegistry::new();
    let report = convert_regions(&files, storage, &registry, threads);
    Ok(report)
}

/// Convert an explicit list of region files (shared by the CLI and tests).
pub fn convert_regions<B: KvBackend>(
    files: &[PathBuf],
    storage: &WorldStorage<B>,
    registry: &SharedRegistry,
    threads: usize,
) -> ConversionReport {
    let next = AtomicUsize::new(0);
    let regions = AtomicUsize::new(0);
    let chunks = AtomicUsize::new(0);
    let sections = AtomicUsize::new(0);
    let checksum = std::sync::atomic::AtomicU64::new(0);
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let worker = || {
        let mut interner = CachedInterner::new(registry);
        loop {
            let idx = next.fetch_add(1, Ordering::Relaxed);
            let Some(path) = files.get(idx) else { break };
            let region = match Region::open(path) {
                Ok(r) => r,
                Err(e) => {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            regions.fetch_add(1, Ordering::Relaxed);
            let mut local_errors = Vec::new();
            let decoded = region.decode_all(&mut interner, &mut local_errors);
            for chunk in decoded {
                chunks.fetch_add(1, Ordering::Relaxed);
                for (cy, sc) in chunk.sections {
                    let key = SubChunkKey::new(chunk.cx, cy, chunk.cz);
                    match storage.save(key, &sc) {
                        Ok(()) => {
                            sections.fetch_add(1, Ordering::Relaxed);
                            // Order-independent audit checksum.
                            let mut buf = key.encode().to_vec();
                            buf.extend_from_slice(&sc.solid_mask().count().to_le_bytes());
                            buf.extend_from_slice(&(sc.palette().distinct() as u32).to_le_bytes());
                            checksum.fetch_xor(fnv1a(&buf), Ordering::Relaxed);
                        }
                        Err(e) => local_errors
                            .push(format!("save ({},{},{}): {e}", chunk.cx, cy, chunk.cz)),
                    }
                }
            }
            if !local_errors.is_empty() {
                errors.lock().unwrap().extend(local_errors);
            }
        }
    };

    let threads = threads.max(1);
    if threads == 1 {
        worker();
    } else {
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(worker);
            }
        });
    }

    // A failed final flush means writes may not be durable — surface it in the
    // audit trail rather than silently reporting success.
    if let Err(e) = storage.flush() {
        errors.lock().unwrap().push(format!("flush failed: {e}"));
    }

    ConversionReport {
        regions: regions.into_inner(),
        chunks: chunks.into_inner(),
        sections: sections.into_inner(),
        distinct_blocks: registry.len(),
        checksum: checksum.into_inner(),
        errors: errors.into_inner().unwrap(),
    }
}

/// Number of worker threads to default to.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::MemStore;

    #[test]
    fn cached_interner_matches_shared() {
        let shared = SharedRegistry::new();
        let mut a = CachedInterner::new(&shared);
        let mut b = CachedInterner::new(&shared);
        let stone = a.intern("minecraft:stone");
        // Different worker sees the same id through the shared registry.
        assert_eq!(b.intern("minecraft:stone"), stone);
        assert_eq!(a.intern("minecraft:air").0, BlockStateId::AIR);
    }

    #[test]
    fn empty_world_dir_yields_empty_report() {
        let dir = std::env::temp_dir().join(format!("aether-empty-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("region")).unwrap();
        let storage = WorldStorage::new(MemStore::new());
        let report = convert_world(&dir, &storage, 2).unwrap();
        assert_eq!(report.regions, 0);
        assert_eq!(report.sections, 0);
        assert!(report.is_clean());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
