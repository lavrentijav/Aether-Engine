//! A disk cache for *unmodified* generated columns.
//!
//! The engine's storage model deliberately writes nothing for a column nobody
//! has edited: `world = generator(seed) + journal`, and a deterministic
//! generator is a baseline that costs zero bytes. That is the right model for
//! *durability* — there is nothing to lose and nothing to corrupt — but it
//! makes the generator's cost recur. Every restart, and every time a column
//! falls out of memory, the terrain is computed again from scratch. With the
//! vanilla generator at ~75 ms a column, a view-radius-8 join is ~20 s of pure
//! arithmetic that produces exactly the bytes it produced last time.
//!
//! So this is a cache in the strict sense: **it is never a source of truth**.
//! Every entry is reproducible from the seed, so losing the whole directory,
//! or any single corrupt file in it, costs time and nothing else. Nothing here
//! ever fails a chunk load — a read error is a miss.
//!
//! # What invalidates an entry
//!
//! * **Age.** Entries older than [`CacheConfig::ttl`] are ignored and
//!   rewritten. A day by default.
//! * **Count.** Above [`CacheConfig::max_entries`] the oldest files are
//!   deleted, on a sweep at startup and hourly after.
//! * **The generator itself.** The seed and [`GEN_REVISION`] are part of the
//!   directory name, so a changed seed or a changed generator reads an empty
//!   cache rather than last week's terrain. **Bump `GEN_REVISION` whenever the
//!   generator's output changes** — surface rules landing is exactly such a
//!   change, and shipping it against a warm cache would serve the old bare
//!   stone until the day was out.
//!
//! The journal is untouched by any of this: edits still go where they went.
//! An edited column is loaded as cache-or-generator *plus* the journal on top,
//! so a cached baseline and an edited world compose the same way a freshly
//! generated baseline and an edited world always did.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aether_world::storage::format::{deserialize_subchunk, serialize_subchunk};
use aether_worldgen::{ChunkGenerator, ColumnBiomes, GeneratedColumn};

/// Bump this whenever the generator's output changes for the same seed.
///
/// It is part of the cache directory's name, so bumping it orphans every old
/// entry instead of serving terrain the current generator would not produce.
pub const GEN_REVISION: u32 = 3;

/// How long a half-written `.tmp` file is left alone before the sweep treats
/// it as debris from a crash rather than a write in progress.
const TMP_GRACE: Duration = Duration::from_secs(600);

/// The on-disk format's own version, independent of the terrain's.
const FORMAT_MAGIC: &[u8; 4] = b"AGC2";

/// How the cache is sized and aged.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Where the per-seed directories live. Empty disables the cache.
    pub root: String,
    /// How long an entry stays usable.
    pub ttl: Duration,
    /// How many entries to keep before the oldest are swept away.
    pub max_entries: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            root: "world-cache".into(),
            ttl: Duration::from_secs(24 * 60 * 60),
            max_entries: 20_000,
        }
    }
}

/// How the cache has been behaving. Reported, not acted on.
#[derive(Debug, Default)]
pub struct CacheStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub writes: AtomicU64,
    /// Reads that found a file and could not use it — truncated, corrupt, or
    /// written by an older format. Counted separately from a plain miss
    /// because a rising number here means something is wrong with the disk,
    /// not with the workload.
    pub rejected: AtomicU64,
    pub evicted: AtomicU64,
}

/// A generator that reads and writes a column cache around another one.
pub struct Cached<G> {
    inner: G,
    dir: Option<PathBuf>,
    ttl: Duration,
    stats: Arc<CacheStats>,
    /// Biomes of recently generated columns. The world keeps a column's
    /// blocks but not its biomes, so they are remembered here for the chunk
    /// encoder; bounded, and a forgotten column falls back to plains.
    biomes:
        std::sync::Mutex<std::collections::HashMap<(i32, i32), Arc<aether_worldgen::ColumnBiomes>>>,
}

/// How many columns' biomes are remembered.
const BIOME_MEMORY: usize = 20_000;

impl<G: ChunkGenerator> Cached<G> {
    /// Wrap `inner`, creating the cache directory for `seed`.
    ///
    /// A directory that cannot be created disables the cache and says so: a
    /// server that will not start because its *cache* is unwritable would be
    /// trading a performance feature for an outage.
    #[cfg(test)]
    pub fn new(inner: G, seed: u64, cfg: &CacheConfig) -> Self {
        Self::with_revision(inner, seed, GEN_REVISION, cfg)
    }

    /// [`Self::new`] for a generator of a given revision — the vanilla
    /// generator's [`GEN_REVISION`], or another value for another kind of
    /// terrain, so switching generators never serves one's columns as the
    /// other's.
    pub fn with_revision(inner: G, seed: u64, revision: u32, cfg: &CacheConfig) -> Self {
        let dir = if cfg.root.is_empty() {
            None
        } else {
            let dir = Path::new(&cfg.root).join(format!("s{seed}-r{revision}"));
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    for old in remove_stale_revisions(Path::new(&cfg.root), seed, revision) {
                        crate::log::info(&format!(
                            "column cache: removed `{}`, written by an older generator",
                            old.display()
                        ));
                    }
                    Some(dir)
                }
                Err(e) => {
                    crate::log::warn(&format!("column cache disabled: cannot use `{cfg:?}`: {e}"));
                    None
                }
            }
        };
        let stats = Arc::new(CacheStats::default());
        if let Some(dir) = &dir {
            spawn_sweeper(dir.clone(), cfg.max_entries, cfg.ttl, Arc::clone(&stats));
        }
        Self {
            inner,
            dir,
            ttl: cfg.ttl,
            stats,
            biomes: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The biomes of column `(cx, cz)`, if it was generated recently.
    pub fn biomes(&self, cx: i32, cz: i32) -> Option<Arc<aether_worldgen::ColumnBiomes>> {
        self.biomes.lock().unwrap().get(&(cx, cz)).cloned()
    }

    fn remember(&self, cx: i32, cz: i32, column: &GeneratedColumn) {
        if let Some(b) = &column.biomes {
            let mut m = self.biomes.lock().unwrap();
            if m.len() >= BIOME_MEMORY {
                m.clear();
            }
            m.insert((cx, cz), Arc::new(b.clone()));
        }
    }

    /// A one-line summary for the console.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        let (h, m) = (
            s.hits.load(Ordering::Relaxed),
            s.misses.load(Ordering::Relaxed),
        );
        let rate = if h + m == 0 {
            0.0
        } else {
            100.0 * h as f64 / (h + m) as f64
        };
        format!(
            "{h} hits / {m} misses ({rate:.0}%), {} written, {} rejected, {} evicted",
            s.writes.load(Ordering::Relaxed),
            s.rejected.load(Ordering::Relaxed),
            s.evicted.load(Ordering::Relaxed),
        )
    }

    fn path(&self, cx: i32, cz: i32) -> Option<PathBuf> {
        // Sharded so no single directory holds every column: the sweep lists
        // directories, and a flat one with tens of thousands of entries makes
        // that walk the slowest thing in the process.
        let dir = self.dir.as_ref()?;
        let shard = format!("{:02x}", (cx.rem_euclid(16) * 16 + cz.rem_euclid(16)) as u8);
        Some(dir.join(shard).join(format!("{cx}.{cz}.col")))
    }

    fn read(&self, path: &Path) -> Option<GeneratedColumn> {
        let meta = std::fs::metadata(path).ok()?;
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or(Duration::ZERO);
        if age > self.ttl {
            return None;
        }
        let mut buf = Vec::with_capacity(meta.len() as usize);
        std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
        match decode(&buf) {
            Some(c) => Some(c),
            None => {
                self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn write(&self, path: &Path, column: &GeneratedColumn) {
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        // Write beside the target and rename over it, so a crash mid-write
        // leaves either the old entry or none — never half a column that
        // would decode into terrain with holes in it.
        let tmp = path.with_extension("col.tmp");
        let ok = std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(&encode(column)))
            .and_then(|()| std::fs::rename(&tmp, path))
            .is_ok();
        if ok {
            self.stats.writes.fetch_add(1, Ordering::Relaxed);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

impl<G: ChunkGenerator> ChunkGenerator for Cached<G> {
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        let path = self.path(cx, cz);
        if let Some(path) = &path {
            if let Some(column) = self.read(path) {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                self.remember(cx, cz, &column);
                return column;
            }
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        let column = self.inner.generate_column(cx, cz);
        self.remember(cx, cz, &column);
        if let Some(path) = &path {
            self.write(path, &column);
        }
        column
    }
}

/// `AGC2`, a section count, then `(cy, length, bytes)` per section, then the
/// biomes: a presence byte and, when present, the first section Y, the
/// palette (`u16` count, then `u8`-length-prefixed names) and 64 palette
/// indices per section (`u16` count).
fn encode(column: &GeneratedColumn) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(FORMAT_MAGIC);
    out.extend_from_slice(&(column.sections.len() as u32).to_le_bytes());
    for (cy, sc) in &column.sections {
        let body = serialize_subchunk(sc);
        out.push(*cy as u8);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
    }
    match &column.biomes {
        None => out.push(0),
        Some(b) => {
            out.push(1);
            out.push(b.min_section_y as u8);
            out.extend_from_slice(&(b.palette.len() as u16).to_le_bytes());
            for name in &b.palette {
                out.push(name.len().min(255) as u8);
                out.extend_from_slice(&name.as_bytes()[..name.len().min(255)]);
            }
            out.extend_from_slice(&(b.sections.len() as u16).to_le_bytes());
            for sec in &b.sections {
                out.extend_from_slice(sec);
            }
        }
    }
    out
}

fn decode_biomes(buf: &[u8], pos: &mut usize) -> Option<Option<ColumnBiomes>> {
    let flag = *buf.get(*pos)?;
    *pos += 1;
    if flag == 0 {
        return Some(None);
    }
    let min_section_y = *buf.get(*pos)? as i8;
    let n = u16::from_le_bytes(buf.get(*pos + 1..*pos + 3)?.try_into().ok()?) as usize;
    *pos += 3;
    if n > buf.len().saturating_sub(*pos) {
        return None;
    }
    let mut palette = Vec::with_capacity(n);
    for _ in 0..n {
        let len = *buf.get(*pos)? as usize;
        let name = std::str::from_utf8(buf.get(*pos + 1..*pos + 1 + len)?).ok()?;
        palette.push(name.to_string());
        *pos += 1 + len;
    }
    let count = u16::from_le_bytes(buf.get(*pos..*pos + 2)?.try_into().ok()?) as usize;
    *pos += 2;
    if count > buf.len().saturating_sub(*pos) / 64 {
        return None;
    }
    let mut sections = Vec::with_capacity(count);
    for _ in 0..count {
        let sec: [u8; 64] = buf.get(*pos..*pos + 64)?.try_into().ok()?;
        if sec.iter().any(|i| *i as usize >= palette.len()) {
            return None;
        }
        sections.push(sec);
        *pos += 64;
    }
    Some(Some(ColumnBiomes {
        min_section_y,
        palette,
        sections,
    }))
}

/// The inverse. `None` for anything that does not decode cleanly — a cache
/// entry is never trusted far enough to panic over.
fn decode(buf: &[u8]) -> Option<GeneratedColumn> {
    if buf.len() < 8 || &buf[0..4] != FORMAT_MAGIC {
        return None;
    }
    let count = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    // The count is untrusted: every section takes at least five bytes, and a
    // column has at most 256 of them, so anything claiming more is corrupt —
    // and must be rejected *before* it sizes an allocation.
    if count > 256 || count > (buf.len() - 8) / 5 {
        return None;
    }
    let mut pos = 8;
    let mut sections = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 5 > buf.len() {
            return None;
        }
        let cy = buf[pos] as i8;
        let len = u32::from_le_bytes(buf[pos + 1..pos + 5].try_into().ok()?) as usize;
        pos += 5;
        let body = buf.get(pos..pos + len)?;
        pos += len;
        sections.push((cy, deserialize_subchunk(body).ok()?));
    }
    let biomes = decode_biomes(buf, &mut pos)?;
    // Trailing bytes mean this is not the file we think it is.
    if pos != buf.len() {
        return None;
    }
    Some(GeneratedColumn { sections, biomes })
}

/// Delete expired entries, and the oldest ones above `max_entries`.
///
/// Returns how many files it removed. Separate from the thread that calls it
/// so a test can run one sweep and look at the result.
pub fn sweep(dir: &Path, max_entries: usize, ttl: Duration) -> usize {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut removed = 0usize;
    let Ok(shards) = std::fs::read_dir(dir) else {
        return 0;
    };
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or(Duration::ZERO);
            if path.extension().and_then(|s| s.to_str()) != Some("col") {
                // Leftover `.tmp` from a crashed write: rubbish rather than
                // cache. Only once it is *old*, though — a fresh one is a
                // write in flight on another thread, and deleting it makes
                // that write fail. This sweep runs at startup, which is
                // exactly when the first columns are being written, so the
                // race is the common case and not the rare one.
                if age > TMP_GRACE {
                    let _ = std::fs::remove_file(&path);
                    removed += 1;
                }
                continue;
            }
            let expired = age > ttl;
            if expired {
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            } else {
                files.push((modified, path));
            }
        }
    }
    if files.len() > max_entries {
        // Oldest first, so the survivors are the ones most recently written.
        files.sort_by_key(|(t, _)| *t);
        for (_, path) in files.iter().take(files.len() - max_entries) {
            if std::fs::remove_file(path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Delete `root/s{seed}-r{N}` for every revision but `revision`: they are
/// never read again, since the revision is part of the path. Other seeds'
/// directories are left alone — another world may share the cache root.
/// Returns what was removed.
fn remove_stale_revisions(root: &Path, seed: u64, revision: u32) -> Vec<PathBuf> {
    let prefix = format!("s{seed}-r");
    let current = format!("{prefix}{revision}");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stale = name != current
            && name
                .strip_prefix(&prefix)
                .is_some_and(|rev| !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()));
        if stale && e.path().is_dir() && std::fs::remove_dir_all(e.path()).is_ok() {
            removed.push(e.path());
        }
    }
    removed
}

/// Sweep once at startup and hourly after.
fn spawn_sweeper(dir: PathBuf, max_entries: usize, ttl: Duration, stats: Arc<CacheStats>) {
    std::thread::spawn(move || loop {
        let n = sweep(&dir, max_entries, ttl);
        if n > 0 {
            stats.evicted.fetch_add(n as u64, Ordering::Relaxed);
            crate::log::info(&format!(
                "column cache: swept {n} stale entr{}",
                if n == 1 { "y" } else { "ies" }
            ));
        }
        std::thread::sleep(Duration::from_secs(3600));
    });
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    #[test]
    fn only_this_seeds_older_revisions_are_removed() {
        let root = std::env::temp_dir().join(format!("gencache-rev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["s42-r1", "s42-r2", "s7-r1", "s42-rx", "notes"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::create_dir_all(root.join(format!("s42-r{GEN_REVISION}"))).unwrap();
        let mut removed: Vec<String> = remove_stale_revisions(&root, 42, GEN_REVISION)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(removed, ["s42-r1", "s42-r2"]);
        assert!(root.join(format!("s42-r{GEN_REVISION}")).is_dir());
        assert!(root.join("s7-r1").is_dir(), "another seed's cache stays");
        assert!(root.join("s42-rx").is_dir() && root.join("notes").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::subchunk::SubChunk;
    use aether_world::BlockStateId;
    use std::sync::atomic::AtomicUsize;

    /// A generator that counts how often it is asked.
    struct Counting(AtomicUsize);
    impl ChunkGenerator for Counting {
        fn generate_column(&self, cx: i32, _cz: i32) -> GeneratedColumn {
            self.0.fetch_add(1, Ordering::Relaxed);
            let mut sc = SubChunk::new();
            sc.set(1, 2, 3, BlockStateId(cx as u32 + 1), Default::default());
            let mut other = SubChunk::new();
            other.set(0, 0, 0, BlockStateId(7), Default::default());
            GeneratedColumn {
                sections: vec![(0, sc), (4, other)],
                biomes: Some(ColumnBiomes {
                    min_section_y: -4,
                    palette: vec!["minecraft:plains".into(), "minecraft:river".into()],
                    sections: vec![[1u8; 64]; 24],
                }),
            }
        }
    }

    fn tmpdir(name: &str) -> String {
        let p = std::env::temp_dir().join(format!("aether-gencache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p.to_string_lossy().into_owned()
    }

    fn cfg(root: String) -> CacheConfig {
        CacheConfig {
            root,
            ..Default::default()
        }
    }

    #[test]
    fn a_column_is_generated_once_and_then_read_back_identically() {
        let root = tmpdir("hit");
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(root.clone()));
        let first = c.generate_column(3, -4);
        let second = c.generate_column(3, -4);
        assert_eq!(c.inner.0.load(Ordering::Relaxed), 1, "generated once");
        assert_eq!(c.stats.hits.load(Ordering::Relaxed), 1);
        assert_eq!(first.sections.len(), second.sections.len());
        for ((cy1, a), (cy2, b)) in first.sections.iter().zip(&second.sections) {
            assert_eq!(cy1, cy2);
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        assert_eq!(a.get(x, y, z), b.get(x, y, z), "block ({x},{y},{z})");
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_different_seed_does_not_read_another_seeds_terrain() {
        let root = tmpdir("seed");
        let a = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(root.clone()));
        a.generate_column(0, 0);
        let b = Cached::new(Counting(AtomicUsize::new(0)), 2, &cfg(root.clone()));
        b.generate_column(0, 0);
        assert_eq!(
            b.stats.hits.load(Ordering::Relaxed),
            0,
            "seed 2 starts cold"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_expired_entry_is_regenerated_rather_than_served() {
        let root = tmpdir("ttl");
        let cfg = CacheConfig {
            root: root.clone(),
            ttl: Duration::ZERO,
            ..Default::default()
        };
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg);
        c.generate_column(0, 0);
        c.generate_column(0, 0);
        assert_eq!(c.inner.0.load(Ordering::Relaxed), 2, "both were generated");
        assert_eq!(c.stats.hits.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_and_not_a_crash() {
        let root = tmpdir("corrupt");
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(root.clone()));
        c.generate_column(5, 5);
        let path = c.path(5, 5).unwrap();
        std::fs::write(&path, b"AGC2\xff\xff\xff\xffgarbage").unwrap();
        let again = c.generate_column(5, 5);
        assert_eq!(again.sections.len(), 2, "the generator answered instead");
        assert_eq!(c.stats.rejected.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_column_survives_the_round_trip_through_the_file_format() {
        let g = Counting(AtomicUsize::new(0));
        let column = g.generate_column(3, -4);
        let back = decode(&encode(&column)).expect("round trip");
        assert_eq!(back.sections.len(), column.sections.len());
        assert_eq!(back.biomes, column.biomes, "biomes survive too");
    }

    #[test]
    fn a_file_that_is_not_ours_at_all_is_a_miss() {
        assert!(decode(b"").is_none());
        assert!(decode(b"NOPE\0\0\0\0").is_none());
        // Right magic, claims one section, has none.
        assert!(decode(b"AGC2\x01\x00\x00\x00").is_none());
        // An older format is a miss, not a misread.
        assert!(decode(b"AGC1\x00\x00\x00\x00").is_none());
    }

    /// Regression: the startup sweep used to delete every non-`.col` file it
    /// saw, which on a busy start meant deleting the `.tmp` file a write was
    /// in the middle of producing. The first column written after a restart
    /// therefore never made it into the cache.
    #[test]
    fn the_sweep_does_not_delete_a_write_that_is_still_in_flight() {
        let root = tmpdir("inflight");
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(root.clone()));
        let dir = c.dir.clone().unwrap();
        let shard = dir.join("00");
        std::fs::create_dir_all(&shard).unwrap();
        let tmp = shard.join("0.0.col.tmp");
        std::fs::write(&tmp, b"half a column").unwrap();
        sweep(&dir, 1000, Duration::from_secs(3600));
        assert!(tmp.exists(), "a fresh .tmp is a write in progress");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sweep_deletes_the_oldest_once_there_are_too_many() {
        let root = tmpdir("sweep");
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(root.clone()));
        for i in 0..10 {
            c.generate_column(i, 0);
        }
        let dir = c.dir.clone().unwrap();
        let removed = sweep(&dir, 4, Duration::from_secs(3600));
        assert_eq!(removed, 6, "ten written, four kept");
        let mut left = 0;
        for shard in std::fs::read_dir(&dir).unwrap().flatten() {
            left += std::fs::read_dir(shard.path()).unwrap().count();
        }
        assert_eq!(left, 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sweep_removes_everything_once_it_has_all_expired() {
        let root = tmpdir("expire");
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(root.clone()));
        for i in 0..5 {
            c.generate_column(i, i);
        }
        let dir = c.dir.clone().unwrap();
        assert_eq!(sweep(&dir, 1000, Duration::ZERO), 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_root_disables_the_cache_without_failing() {
        let c = Cached::new(Counting(AtomicUsize::new(0)), 1, &cfg(String::new()));
        c.generate_column(0, 0);
        c.generate_column(0, 0);
        assert_eq!(c.inner.0.load(Ordering::Relaxed), 2);
        assert_eq!(c.stats.writes.load(Ordering::Relaxed), 0);
    }
}
