//! A [`ChunkGenerator`](crate::ChunkGenerator) over the vanilla pipeline.
//!
//! The stages run in the game's order, each on a [`ProtoChunk`]:
//!
//! 1. **biomes** — the multi-noise source at every quart of the chunk;
//! 2. **noise** — the cell-interpolated density, aquifers and ore veins;
//! 3. **surface** — the surface rule tree ([`super::surface`]);
//! 4. **carvers** — caves and canyons started in any chunk within eight
//!    chunks ([`super::carver`]);
//! 5. **features** — per-biome decoration ([`super::feature`]), which reads
//!    and writes the 3×3 chunks around the one being decorated.
//!
//! Stages 1–4 depend only on the chunk itself, so they are computed once per
//! chunk and kept in a bounded cache: decorating one chunk needs its eight
//! neighbours' terrain, and a column needs the decoration of its eight
//! neighbours. Each chunk's decoration is likewise computed once and cached
//! as the list of blocks it wrote.
//!
//! # Ids versus names
//!
//! Blocks are resolved by name through the registry. The engine's
//! [`BlockStateId`] is vanilla 1.21.11's own flattened state id, so the
//! names in the pack and the ids line up one to one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aether_world::registry::{blocks, props, BlockRegistry};
use aether_world::BlockStateId;

use super::biome::{BiomeId, BiomeRegistry};
use super::biome_manager;
use super::carver::Carvers;
use super::chunk::ProtoChunk;
use super::climate::ParameterList;
use super::density::{BuildError, DataPack};
use super::feature::Decorator;
use super::json::Json;
use super::ore_veins::NoiseBlock;
use super::overworld::OverworldBiomeSource;
use super::surface::{SurfaceEnv, SurfaceSystem};
use super::terrain::Terrain;
use crate::{ChunkGenerator, ColumnBiomes, ColumnBuilder, GeneratedColumn};

/// The block ids the noise stage can emit, resolved once.
#[derive(Debug, Clone)]
struct Palette {
    air: BlockStateId,
    water: BlockStateId,
    lava: BlockStateId,
    stone: BlockStateId,
    granite: BlockStateId,
    tuff: BlockStateId,
    copper_ore: BlockStateId,
    raw_copper_block: BlockStateId,
    deepslate_iron_ore: BlockStateId,
    raw_iron_block: BlockStateId,
}

impl Palette {
    fn resolve() -> Result<Self, BuildError> {
        let one = |name: &str| -> Result<BlockStateId, BuildError> {
            blocks::default_state(name)
                .ok_or_else(|| BuildError::new(format!("block registry has no `{name}`")))
        };
        // Water is the one block whose *state* matters here: a source block is
        // `level=0`, and the default state of `minecraft:water` is not
        // guaranteed to be it.
        let water_block = blocks::block_id_of("minecraft:water")
            .ok_or_else(|| BuildError::new("block registry has no `minecraft:water`"))?;
        let water = props::state_with(water_block, &[("level", "0")])
            .map(BlockStateId)
            .ok_or_else(|| BuildError::new("minecraft:water has no `level=0` state"))?;
        Ok(Self {
            air: one("minecraft:air")?,
            water,
            lava: one("minecraft:lava")?,
            stone: one("minecraft:stone")?,
            granite: one("minecraft:granite")?,
            tuff: one("minecraft:tuff")?,
            copper_ore: one("minecraft:copper_ore")?,
            raw_copper_block: one("minecraft:raw_copper_block")?,
            deepslate_iron_ore: one("minecraft:deepslate_iron_ore")?,
            raw_iron_block: one("minecraft:raw_iron_block")?,
        })
    }

    fn id_of(&self, b: NoiseBlock) -> BlockStateId {
        match b {
            NoiseBlock::Air => self.air,
            NoiseBlock::Water => self.water,
            NoiseBlock::Lava => self.lava,
            NoiseBlock::Stone => self.stone,
            NoiseBlock::Granite => self.granite,
            NoiseBlock::Tuff => self.tuff,
            NoiseBlock::CopperOre => self.copper_ore,
            NoiseBlock::RawCopperBlock => self.raw_copper_block,
            NoiseBlock::DeepslateIronOre => self.deepslate_iron_ore,
            NoiseBlock::RawIronBlock => self.raw_iron_block,
        }
    }
}

/// A pipeline stage, for inspecting a chunk part-way through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Biomes only (blocks are all air).
    Biomes,
    /// After the noise fill.
    Noise,
    /// After the surface rules.
    Surface,
    /// After carving.
    Carvers,
    /// After decoration.
    Features,
}

/// A small bounded map: least-recently-inserted eviction is enough, because
/// generation walks outwards from players and rarely revisits.
struct Lru<V> {
    map: HashMap<(i32, i32), (u64, V)>,
    tick: u64,
    cap: usize,
}

impl<V: Clone> Lru<V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            cap,
        }
    }

    fn get(&mut self, k: (i32, i32)) -> Option<V> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(&k).map(|e| {
            e.0 = t;
            e.1.clone()
        })
    }

    fn put(&mut self, k: (i32, i32), v: V) {
        if self.map.len() >= self.cap {
            // Drop the oldest quarter in one sweep so eviction stays cheap.
            let mut ticks: Vec<u64> = self.map.values().map(|e| e.0).collect();
            ticks.sort_unstable();
            let cut = ticks[ticks.len() / 4];
            self.map.retain(|_, e| e.0 > cut);
        }
        self.tick += 1;
        self.map.insert(k, (self.tick, v));
    }
}

/// Everything shared by the stages.
pub(crate) struct Core {
    pub(crate) seed: i64,
    pub(crate) terrain: Arc<Terrain>,
    pub(crate) biome_source: OverworldBiomeSource,
    /// Biome table row → registry id.
    entry_ids: Vec<BiomeId>,
    pub(crate) biomes: BiomeRegistry,
    pub(crate) surface: SurfaceSystem,
    pub(crate) zoom_seed: i64,
    palette: Palette,
    pub(crate) min_y: i32,
    pub(crate) height: i32,
    pub(crate) sea_level: i32,
}

impl Core {
    /// The biome at a quart, from the biome source.
    pub(crate) fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> BiomeId {
        self.entry_ids[self.biome_source.entry_at(qx, qy, qz)]
    }
}

/// Generates the overworld for a given seed.
pub struct VanillaGenerator {
    core: Arc<Core>,
    carvers: Carvers,
    decorator: Decorator,
    registry: BlockRegistry,
    base_cache: Mutex<Lru<Arc<ProtoChunk>>>,
    decor_cache: Mutex<Lru<Arc<[DecorWrite]>>>,
}

/// One decoration write, packed to eight bytes relative to the decorated
/// chunk's corner: a chunk's decoration runs to thousands of writes, and a
/// thousand chunks of them kept as `(i32, i32, i32, BlockStateId)` were over
/// a hundred MiB.
#[derive(Clone, Copy)]
struct DecorWrite {
    dx: i16,
    y: i16,
    dz: i16,
    state: u16,
}

impl DecorWrite {
    /// `None` for a write that does not fit — never for vanilla features,
    /// which stay within a chunk's neighbours and its height, and block
    /// states, which number under 65 536.
    fn pack(cx: i32, cz: i32, (x, y, z, s): (i32, i32, i32, BlockStateId)) -> Option<Self> {
        Some(Self {
            dx: i16::try_from(x - cx * 16).ok()?,
            y: i16::try_from(y).ok()?,
            dz: i16::try_from(z - cz * 16).ok()?,
            state: u16::try_from(s.0).ok()?,
        })
    }

    fn unpack(self, cx: i32, cz: i32) -> (i32, i32, i32, BlockStateId) {
        (
            cx * 16 + self.dx as i32,
            self.y as i32,
            cz * 16 + self.dz as i32,
            BlockStateId(self.state as u32),
        )
    }
}

/// Quart biomes around one chunk, filled on first use: the jittered lookup
/// reaches one quart past the chunk on every side.
pub(crate) struct QuartCache<'a> {
    core: &'a Core,
    qx0: i32,
    qz0: i32,
    min_qy: i32,
    qh: i32,
    cells: std::cell::RefCell<Vec<u16>>,
}

const UNSET: u16 = u16::MAX;

impl<'a> QuartCache<'a> {
    pub(crate) fn new(core: &'a Core, cx: i32, cz: i32) -> Self {
        let qh = core.height / 4;
        Self {
            core,
            qx0: cx * 4 - 1,
            qz0: cz * 4 - 1,
            min_qy: core.min_y >> 2,
            qh,
            cells: std::cell::RefCell::new(vec![UNSET; 6 * 6 * qh as usize]),
        }
    }

    /// The biome at a quart, `qy` clamped to the world.
    pub(crate) fn get(&self, qx: i32, qy: i32, qz: i32) -> BiomeId {
        let qy = qy.clamp(self.min_qy, self.min_qy + self.qh - 1);
        let (ix, iz) = (qx - self.qx0, qz - self.qz0);
        if !(0..6).contains(&ix) || !(0..6).contains(&iz) {
            return self.core.noise_biome(qx, qy, qz);
        }
        let i = (((qy - self.min_qy) * 6 + iz) * 6 + ix) as usize;
        let v = self.cells.borrow()[i];
        if v != UNSET {
            return v;
        }
        let b = self.core.noise_biome(qx, qy, qz);
        self.cells.borrow_mut()[i] = b;
        b
    }

    /// The biome at a block, through the jittered zoom.
    pub(crate) fn at_block(&self, x: i32, y: i32, z: i32) -> BiomeId {
        let (qx, qy, qz) = biome_manager::zoomed_quart(self.core.zoom_seed, x, y, z);
        self.get(qx, qy, qz)
    }
}

struct Env<'a> {
    quarts: &'a QuartCache<'a>,
    terrain: &'a Terrain,
}

impl SurfaceEnv for Env<'_> {
    fn biome_at_block(&self, x: i32, y: i32, z: i32) -> BiomeId {
        self.quarts.at_block(x, y, z)
    }

    fn preliminary_surface_level(&self, x: i32, z: i32) -> i32 {
        self.terrain.preliminary_surface_level(x, z)
    }
}

impl VanillaGenerator {
    /// Build from an unpacked copy of the game's worldgen data and a seed.
    ///
    /// `pack_root` is a directory containing `data/minecraft/worldgen/` — the
    /// operator's own copy of the game, read at run time and never vendored.
    /// The biome table is rebuilt in code ([`super::biome_table`]).
    pub fn new(pack_root: impl AsRef<Path>, seed: u64) -> Result<Self, BuildError> {
        let biome_source = OverworldBiomeSource::new(&pack_root, seed)?;
        Self::build(pack_root, biome_source, seed)
    }

    /// As [`Self::new`], but with the biome table read from a `--reports`
    /// dump instead of the built-in one. Kept for parity measurements.
    pub fn load(
        pack_root: impl AsRef<Path>,
        biome_report: impl AsRef<Path>,
        seed: u64,
    ) -> Result<Self, BuildError> {
        let biome_source = OverworldBiomeSource::load(&pack_root, biome_report, seed)?;
        Self::build(pack_root, biome_source, seed)
    }

    fn build(
        pack_root: impl AsRef<Path>,
        biome_source: OverworldBiomeSource,
        seed: u64,
    ) -> Result<Self, BuildError> {
        let terrain = Terrain::load(&pack_root, seed)?;
        let pack = DataPack::open(&pack_root)?;
        let biomes = BiomeRegistry::load(&pack)?;
        let entry_ids = biome_source
            .biomes()
            .entries()
            .iter()
            .map(|e| {
                biomes.id(&e.biome).ok_or_else(|| {
                    BuildError::new(format!("biome table names unknown biome `{}`", e.biome))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let settings_json = pack.noise_settings("minecraft:overworld")?;
        let surface_rule_json = settings_json
            .get("surface_rule")
            .ok_or_else(|| BuildError::new("overworld noise settings: no `surface_rule`"))?;
        let s = terrain.settings();
        let surface = SurfaceSystem::load(
            surface_rule_json,
            terrain.noises(),
            &biomes,
            s.min_y,
            s.height,
            s.sea_level,
        )?;
        let carvers = Carvers::load(&pack, &biomes, s.min_y, s.height)?;
        let decorator = Decorator::load(
            &pack,
            &biomes,
            &biome_source,
            &entry_ids,
            s.min_y,
            s.height,
            terrain.noises(),
        )?;
        let core = Core {
            seed: seed as i64,
            terrain: Arc::new(terrain),
            biome_source,
            entry_ids,
            biomes,
            surface,
            zoom_seed: biome_manager::obfuscate_seed(seed as i64),
            palette: Palette::resolve()?,
            min_y: s.min_y,
            height: s.height,
            sea_level: s.sea_level,
        };
        Ok(Self {
            core: Arc::new(core),
            carvers,
            decorator,
            registry: BlockRegistry::new(),
            base_cache: Mutex::new(Lru::new(1024)),
            decor_cache: Mutex::new(Lru::new(1024)),
        })
    }

    /// The terrain this generator draws from.
    pub fn terrain(&self) -> &Terrain {
        &self.core.terrain
    }

    /// The biome registry.
    pub fn biomes(&self) -> &BiomeRegistry {
        &self.core.biomes
    }

    /// The surface rule tree's condition/rule types this port does not
    /// evaluate. See [`SurfaceSystem::unsupported`].
    pub fn unsupported_surface_rules(&self) -> &[String] {
        self.core.surface.unsupported()
    }

    /// Feature and placement types the data pack uses that the decorator
    /// skips.
    pub fn unsupported_features(&self) -> Vec<String> {
        self.decorator.unsupported()
    }

    /// The biome at a quart position.
    pub fn biome_at(&self, qx: i32, qy: i32, qz: i32) -> &str {
        self.core.biomes.name(self.core.noise_biome(qx, qy, qz))
    }

    /// The biome at a block, through the jittered zoom surface rules use.
    pub fn biome_at_block(&self, x: i32, y: i32, z: i32) -> &str {
        let q = QuartCache::new(&self.core, x >> 4, z >> 4);
        self.core.biomes.name(q.at_block(x, y, z))
    }

    /// The chunk's biome ids by name, ordered `qy`, `qz`, `qx` from the floor.
    pub fn chunk_biome_names(&self, cx: i32, cz: i32) -> Vec<String> {
        let mut c = ProtoChunk::new(cx, cz, self.core.min_y, self.core.height);
        self.fill_biomes(&mut c);
        c.biomes()
            .iter()
            .map(|b| self.core.biomes.name(*b).to_string())
            .collect()
    }

    fn fill_biomes(&self, c: &mut ProtoChunk) {
        let min_qy = self.core.min_y >> 2;
        for qy in 0..(self.core.height / 4) as usize {
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let b = self.core.noise_biome(
                        c.cx * 4 + qx as i32,
                        min_qy + qy as i32,
                        c.cz * 4 + qz as i32,
                    );
                    c.set_biome(qx, qy, qz, b);
                }
            }
        }
    }

    fn fill_noise(&self, c: &mut ProtoChunk) {
        let core = &self.core;
        let st = core.terrain.settings();
        let _grid = super::density::ChunkGridGuard::enter(
            c.cx,
            c.cz,
            st.min_y,
            st.height,
            st.cell_width,
            st.cell_height,
        );
        let chunk = core.terrain.chunk(c.cx, c.cz);
        for lz in 0..16usize {
            for lx in 0..16usize {
                let col = chunk.column(c.min_x() + lx as i32, c.min_z() + lz as i32);
                for (i, nb) in col.into_iter().enumerate() {
                    let id = core.palette.id_of(nb);
                    if id != core.palette.air {
                        c.set_raw(lx, core.min_y + i as i32, lz, id);
                    }
                }
            }
        }
        c.recompute_heightmap();
    }

    /// Run the pipeline on one chunk up to and including `stage`.
    pub fn proto_chunk(&self, cx: i32, cz: i32, stage: Stage) -> ProtoChunk {
        let core = &self.core;
        let mut c = ProtoChunk::new(cx, cz, core.min_y, core.height);
        self.fill_biomes(&mut c);
        if stage >= Stage::Noise {
            self.fill_noise(&mut c);
        }
        if stage >= Stage::Surface {
            let quarts = QuartCache::new(core, cx, cz);
            let env = Env {
                quarts: &quarts,
                terrain: &core.terrain,
            };
            core.surface.build(&mut c, &env, &core.biomes);
            if stage >= Stage::Carvers {
                let chunk_terrain = core.terrain.chunk(cx, cz);
                self.carvers
                    .carve(core, &mut c, &chunk_terrain, &quarts, &env);
            }
        }
        if stage >= Stage::Features {
            let writes = self.decoration_around(cx, cz);
            for (x, y, z, s) in writes {
                c.set((x - c.min_x()) as usize, y, (z - c.min_z()) as usize, s);
            }
        }
        c
    }

    /// The chunk's blocks after `stage`, ordered `y`, `z`, `x` from the
    /// floor — the order the parity dumps use.
    pub fn chunk_at_stage(&self, cx: i32, cz: i32, stage: Stage) -> Vec<BlockStateId> {
        let c = self.proto_chunk(cx, cz, stage);
        c.raw_blocks()
            .iter()
            .map(|v| BlockStateId(*v as u32))
            .collect()
    }

    /// What the generator's caches hold right now: `(chunks, bytes)` of
    /// carved chunks kept for their neighbours, then of decoration writes.
    pub fn cache_footprint(&self) -> ((usize, usize), (usize, usize)) {
        let base = self.base_cache.lock().unwrap();
        let decor = self.decor_cache.lock().unwrap();
        (
            (
                base.map.len(),
                base.map.values().map(|(_, c)| c.block_bytes()).sum(),
            ),
            (
                decor.map.len(),
                decor
                    .map
                    .values()
                    .map(|(_, d)| std::mem::size_of_val::<[DecorWrite]>(d))
                    .sum(),
            ),
        )
    }

    /// The chunk through carving, cached.
    pub(crate) fn base(&self, cx: i32, cz: i32) -> Arc<ProtoChunk> {
        if let Some(c) = self.base_cache.lock().unwrap().get((cx, cz)) {
            return c;
        }
        // Kept for the neighbours' decoration, so packed: 1024 dense chunks
        // were 200 MiB of the generator's memory on their own.
        let mut c = self.proto_chunk(cx, cz, Stage::Carvers);
        c.freeze();
        let c = Arc::new(c);
        self.base_cache
            .lock()
            .unwrap()
            .put((cx, cz), Arc::clone(&c));
        c
    }

    /// The blocks chunk `(cx, cz)`'s decoration writes, anywhere in its 3×3
    /// neighbourhood. Cached.
    fn decoration_of(&self, cx: i32, cz: i32) -> Arc<[DecorWrite]> {
        if let Some(d) = self.decor_cache.lock().unwrap().get((cx, cz)) {
            return d;
        }
        let mut region = [[None, None, None], [None, None, None], [None, None, None]];
        for (dz, row) in region.iter_mut().enumerate() {
            for (dx, slot) in row.iter_mut().enumerate() {
                *slot = Some(self.base(cx + dx as i32 - 1, cz + dz as i32 - 1));
            }
        }
        let region = region.map(|r| r.map(|c| c.expect("filled")));
        // Kept as the neighbours will read it: only writes inside the 3×3
        // they cover, and one per position — the last value, in the order
        // the position was first written, which is how `decoration_around`
        // reads a list with repeats anyway.
        let (x0, z0) = ((cx - 1) * 16, (cz - 1) * 16);
        let mut at: super::FxHashMap<(i32, i32, i32), usize> = Default::default();
        let mut kept: Vec<(i32, i32, i32, BlockStateId)> = Vec::new();
        for w in self.decorator.decorate(&self.core, cx, cz, &region) {
            let (x, y, z, s) = w;
            if !(x0..x0 + 48).contains(&x) || !(z0..z0 + 48).contains(&z) {
                continue;
            }
            match at.entry((x, y, z)) {
                std::collections::hash_map::Entry::Occupied(e) => kept[*e.get()].3 = s,
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(kept.len());
                    kept.push(w);
                }
            }
        }
        let writes: Arc<[DecorWrite]> = kept
            .into_iter()
            .filter_map(|w| DecorWrite::pack(cx, cz, w))
            .collect();
        self.decor_cache
            .lock()
            .unwrap()
            .put((cx, cz), Arc::clone(&writes));
        writes
    }

    /// Every decoration write that lands in chunk `(cx, cz)`, from it and its
    /// eight neighbours, in a fixed order (west to east, north to south).
    fn decoration_around(&self, cx: i32, cz: i32) -> Vec<(i32, i32, i32, BlockStateId)> {
        let (x0, z0) = (cx * 16, cz * 16);
        // Each neighbour decorated against undecorated terrain, so two of
        // them can claim the same cell — a tree trunk from one, the grass the
        // other saw room for. In the game whichever chunk decorates later
        // sees the earlier one's blocks and gives way, so a trunk is never
        // overwritten by grass; but an ore blob does land on another's
        // granite. Merge by what the game would end up with: trees over
        // leaves over other solids over plants, and between equals the later
        // decoration in a fixed north-to-south, west-to-east order.
        let rank = |s: BlockStateId| -> u8 {
            let n = super::blockinfo::name(s);
            if n.ends_with("_log")
                || n.ends_with("_wood")
                || n.ends_with("mushroom_block")
                || n == "minecraft:mushroom_stem"
            {
                4
            } else if super::blockinfo::is_leaves(s) {
                3
            } else if super::blockinfo::blocks_motion(s) {
                2
            } else {
                1
            }
        };
        let mut claimed: super::FxHashMap<(i32, i32, i32), BlockStateId> = Default::default();
        let mut order: Vec<(i32, i32, i32)> = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (ncx, ncz) = (cx + dx, cz + dz);
                let d = self.decoration_of(ncx, ncz);
                let mut mine: super::FxHashMap<(i32, i32, i32), BlockStateId> = Default::default();
                let mut mine_order = Vec::new();
                for (x, y, z, s) in d.iter().map(|w| w.unpack(ncx, ncz)) {
                    if (x0..x0 + 16).contains(&x)
                        && (z0..z0 + 16).contains(&z)
                        && mine.insert((x, y, z), s).is_none()
                    {
                        mine_order.push((x, y, z));
                    }
                }
                for k in mine_order {
                    let s = mine[&k];
                    match claimed.entry(k) {
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(s);
                            order.push(k);
                        }
                        std::collections::hash_map::Entry::Occupied(mut e) => {
                            if rank(s) >= rank(*e.get()) {
                                e.insert(s);
                            }
                        }
                    }
                }
            }
        }
        order
            .into_iter()
            .map(|k| (k.0, k.1, k.2, claimed[&k]))
            .collect()
    }

    /// Per-section biomes for the column, as the client wants them.
    fn column_biomes(&self, c: &ProtoChunk) -> ColumnBiomes {
        let mut palette: Vec<String> = Vec::new();
        let mut index: HashMap<BiomeId, u8> = HashMap::new();
        let sections = (c.height / 16) as usize;
        let mut cells = Vec::with_capacity(sections);
        for s in 0..sections {
            let mut sec = [0u8; 64];
            for qy in 0..4 {
                for qz in 0..4 {
                    for qx in 0..4 {
                        let b = c.biome(qx, (s * 4 + qy) as i32, qz);
                        let i = *index.entry(b).or_insert_with(|| {
                            palette.push(self.core.biomes.name(b).to_string());
                            (palette.len() - 1) as u8
                        });
                        sec[(qy * 4 + qz) * 4 + qx] = i;
                    }
                }
            }
            cells.push(sec);
        }
        ColumnBiomes {
            min_section_y: (c.min_y >> 4) as i8,
            palette,
            sections: cells,
        }
    }
}

impl ChunkGenerator for VanillaGenerator {
    fn generate_column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        let base = self.base(cx, cz);
        let mut c = (*base).clone();
        for (x, y, z, s) in self.decoration_around(cx, cz) {
            c.set((x - c.min_x()) as usize, y, (z - c.min_z()) as usize, s);
        }
        let mut b = ColumnBuilder::new(&self.registry);
        for y in c.min_y..c.max_y() {
            for lz in 0..16usize {
                for lx in 0..16usize {
                    b.set(lx, y, lz, c.get(lx, y, lz));
                }
            }
        }
        let mut col = b.finish();
        col.biomes = Some(self.column_biomes(&c));
        col
    }
}

/// The biome table rows from a `--reports` dump, for callers that want to
/// compare it against [`super::biome_table`].
pub fn report_table(json: &Json) -> Result<ParameterList, BuildError> {
    ParameterList::from_report(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_block_the_noise_stage_emits_resolves() {
        // A missing name here would silently become air in the world, so the
        // whole palette is resolved up front and checked here.
        let p = Palette::resolve().expect("palette resolves against the block registry");
        for b in [
            NoiseBlock::Air,
            NoiseBlock::Water,
            NoiseBlock::Lava,
            NoiseBlock::Stone,
            NoiseBlock::Granite,
            NoiseBlock::Tuff,
            NoiseBlock::CopperOre,
            NoiseBlock::RawCopperBlock,
            NoiseBlock::DeepslateIronOre,
            NoiseBlock::RawIronBlock,
        ] {
            let id = p.id_of(b);
            if b != NoiseBlock::Air {
                assert_ne!(id, BlockStateId::AIR, "{} resolved to air", b.name());
            }
        }
        // Water must be the source state, not merely the block's default.
        assert_eq!(
            p.water,
            BlockStateId(
                props::state_with(
                    blocks::block_id_of("minecraft:water").unwrap(),
                    &[("level", "0")]
                )
                .unwrap()
            )
        );
    }
}
